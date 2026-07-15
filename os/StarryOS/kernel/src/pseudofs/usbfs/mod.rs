mod descriptor;
mod irq;
mod manager;
mod sysfs;
mod tree;

use alloc::{borrow::ToOwned, collections::VecDeque, sync::Arc, vec::Vec};
use core::{
    any::Any,
    future::{Future, poll_fn},
    mem::size_of,
    pin::pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll},
    time::Duration,
};

use ax_errno::{AxError, AxResult, LinuxError, LinuxResult};
use ax_kspin::SpinNoIrq as Mutex;
use ax_sync::PiMutex;
use axfs_ng_vfs::Filesystem;
use axpoll::{IoEvents, PollSet, Pollable};
use crab_usb::usb_if::endpoint::{TransferCompletion, TransferRequest};
use event_listener::Event as NotifyEvent;
use starry_vm::{VmMutPtr, VmPtr, vm_load, vm_write_slice};

use self::{irq::manager, manager::UsbFsManager, tree::UsbRootDir};
use crate::{
    file::{File as KernelFile, FileLike, IoDst, IoSrc, Kstat},
    pseudofs::{SimpleDir, SimpleFs},
};

fn create_filesystem(manager: Arc<UsbFsManager>) -> Filesystem {
    info!("usbfs: creating filesystem instance");
    SimpleFs::new_with("usbfs".into(), descriptor::USBFS_MAGIC, move |fs| {
        SimpleDir::new_maker(
            fs.clone(),
            Arc::new(UsbRootDir {
                fs: fs.clone(),
                manager: manager.clone(),
            }),
        )
    })
}

pub(crate) fn new_usbfs() -> LinuxResult<Option<Filesystem>> {
    if let Some(manager) = manager() {
        return Ok(Some(create_filesystem(manager)));
    }

    info!("usbfs: initializing manager");
    let (hosts, irq_slots) = manager::discover_hosts();
    if hosts.is_empty() {
        info!("usbfs: no USB host found, skip mounting usbfs");
        return Ok(None);
    }

    let manager = Arc::new(UsbFsManager::new(hosts));
    irq::init_globals(manager.clone(), irq_slots);
    // Polling USB hosts need their event handler active while the initial
    // probe waits for xHCI command and transfer events.
    irq::start_event_pump();

    let initialized_hosts = manager::initialize_hosts(&manager) > 0;
    if !initialized_hosts {
        info!("usbfs: no USB host initialized, skip mounting usbfs");
        return Ok(None);
    }

    info!("usbfs: spawning refresh task");
    let refresh_manager = manager.clone();
    crate::task::spawn_kernel_thread(
        move || manager::usbfs_refresh_task(refresh_manager.clone()),
        "usbfs-refresh".to_owned(),
    );
    manager.notify_refresh();

    Ok(Some(create_filesystem(manager)))
}

pub(crate) fn has_manager() -> bool {
    manager().is_some_and(|manager| manager.has_hosts())
}

pub(crate) fn start_event_pump() {
    irq::start_event_pump();
}

pub(crate) fn new_bus_usb_sysfs() -> Filesystem {
    sysfs::new_bus_usb_sysfs()
}

#[derive(Clone)]
pub(crate) struct UsbDeviceSnapshotInfo {
    pub(crate) bus_num: u8,
    pub(crate) device_num: u8,
    pub(crate) descriptor_blob: Vec<u8>,
}

pub(crate) struct UsbDeviceHandle {
    lease: manager::UsbDeviceLease,
}

impl UsbDeviceHandle {
    pub(crate) fn claim_interface(&self, interface: u8, alternate: u8) -> AxResult<()> {
        self.lease.claim_interface(interface, alternate)
    }

    pub(crate) fn release_interface(&self, interface: u8) -> AxResult<()> {
        self.lease.release_interface(interface)
    }

    pub(crate) fn control_transfer(
        &self,
        b_request_type: u8,
        b_request: u8,
        w_value: u16,
        w_index: u16,
        data: &mut [u8],
    ) -> AxResult<usize> {
        self.lease
            .control_transfer(b_request_type, b_request, w_value, w_index, data)
    }

    pub(crate) fn bulk_in(&self, endpoint: u8, data: &mut [u8]) -> AxResult<usize> {
        self.lease.bulk_in(endpoint, data)
    }

    pub(crate) fn bulk_out(&self, endpoint: u8, data: &[u8]) -> AxResult<usize> {
        self.lease.bulk_out(endpoint, data)
    }
}

pub(crate) fn usb_device_snapshots() -> Vec<UsbDeviceSnapshotInfo> {
    let Some(manager) = manager() else {
        return Vec::new();
    };

    let mut snapshots = Vec::new();
    for bus_num in manager.bus_numbers() {
        for device_num in manager.device_numbers(bus_num) {
            let Some(snapshot) = manager.device_snapshot(bus_num, device_num) else {
                continue;
            };
            snapshots.push(UsbDeviceSnapshotInfo {
                bus_num,
                device_num,
                descriptor_blob: snapshot.descriptor_blob,
            });
        }
    }
    snapshots
}

pub(crate) fn acquire_usb_device(bus_num: u8, device_num: u8) -> AxResult<UsbDeviceHandle> {
    let manager = manager().ok_or(AxError::NoSuchDevice)?;
    manager
        .acquire_device(bus_num, device_num)
        .map(|lease| UsbDeviceHandle { lease })
}

pub(crate) fn is_usbfs_device(inner: &dyn Any) -> bool {
    inner.is::<tree::UsbDeviceOps>()
}

pub(crate) fn open_usbfs_file(
    inner: &dyn Any,
    file: ax_fs_ng::File,
    open_flags: u32,
) -> AxResult<Arc<dyn FileLike>> {
    let ops = inner
        .downcast_ref::<tree::UsbDeviceOps>()
        .ok_or(ax_errno::AxError::InvalidInput)?;
    let manager = manager().ok_or(ax_errno::AxError::NoSuchDevice)?;
    let snapshot = manager
        .device_snapshot(ops.bus_num, ops.device_num)
        .ok_or(ax_errno::AxError::NoSuchDevice)?;
    Ok(Arc::new(UsbDeviceFile {
        base: KernelFile::new(file, open_flags),
        manager,
        bus_num: ops.bus_num,
        device_num: ops.device_num,
        snapshot,
        lease: PiMutex::new(None),
        lifecycle_lock: PiMutex::new(()),
        claimed_interfaces: Mutex::new(Default::default()),
        submitted_urbs: Arc::new(PiMutex::new(VecDeque::new())),
        pending_urbs: Arc::new(Mutex::new(VecDeque::new())),
        poll_urbs: Arc::new(PollSet::new()),
        urb_worker: Arc::new(UrbWorker::new()),
    }))
}

static USBFS_URB_LOG_BUDGET: AtomicUsize = AtomicUsize::new(96);
const USBFS_URB_CANCEL_TIMEOUT: Duration = Duration::from_secs(1);

struct UsbDeviceFile {
    base: KernelFile,
    manager: Arc<UsbFsManager>,
    bus_num: u8,
    device_num: u8,
    snapshot: descriptor::UsbDeviceSnapshot,
    lease: PiMutex<Option<Arc<manager::UsbDeviceLease>>>,
    lifecycle_lock: PiMutex<()>,
    claimed_interfaces: Mutex<alloc::collections::BTreeMap<u8, u8>>,
    submitted_urbs: Arc<PiMutex<VecDeque<SubmittedUrb>>>,
    pending_urbs: Arc<Mutex<VecDeque<CompletedUrb>>>,
    poll_urbs: Arc<PollSet>,
    urb_worker: Arc<UrbWorker>,
}

struct UrbWorker {
    wake_event: NotifyEvent,
    running: AtomicBool,
    closed: AtomicBool,
}

impl UrbWorker {
    fn new() -> Self {
        Self {
            wake_event: NotifyEvent::new(),
            running: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    fn notify(&self) {
        self.wake_event.notify(usize::MAX);
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.notify();
    }

    fn try_start(&self) -> bool {
        self.running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }
}

struct SubmittedUrb {
    user_urb_ptr: usize,
    transfer: SubmittedUrbTransfer,
    interface: Option<u8>,
    buffer: Vec<u8>,
    is_in: bool,
    data_offset: usize,
    packet_lengths: Vec<usize>,
    log: bool,
}

enum SubmittedUrbTransfer {
    Live(manager::SubmittedTransfer),
    Deferred(UsbfsQuirk),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UsbfsQuirk {
    UserspaceClaimedUvcControlInterface,
    DeferredStatusInterrupt,
}

impl SubmittedUrb {
    fn try_reclaim(&self) -> AxResult<Option<TransferCompletion>> {
        match &self.transfer {
            SubmittedUrbTransfer::Live(transfer) => transfer.try_reclaim(),
            SubmittedUrbTransfer::Deferred(_) => Ok(None),
        }
    }

    fn poll_reclaim(&self, cx: &mut Context<'_>) -> Poll<AxResult<TransferCompletion>> {
        match &self.transfer {
            SubmittedUrbTransfer::Live(transfer) => transfer.poll_reclaim(cx),
            SubmittedUrbTransfer::Deferred(_) => Poll::Pending,
        }
    }

    fn cancel(&self) -> AxResult<()> {
        match &self.transfer {
            SubmittedUrbTransfer::Live(transfer) => transfer.cancel(),
            SubmittedUrbTransfer::Deferred(_) => Ok(()),
        }
    }

    fn deferred_quirk(&self) -> Option<UsbfsQuirk> {
        match &self.transfer {
            SubmittedUrbTransfer::Live(_) => None,
            SubmittedUrbTransfer::Deferred(quirk) => Some(*quirk),
        }
    }

    fn is_deferred(&self) -> bool {
        self.deferred_quirk().is_some()
    }
}

struct CompletedUrb {
    user_urb_ptr: usize,
    result: AxResult<UrbTransferResult>,
    log: bool,
}

struct UrbTransferResult {
    data: Vec<u8>,
    data_offset: usize,
    actual_length: usize,
    packet_lengths: Vec<usize>,
    packet_actual_lengths: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndpointTransferType {
    Bulk,
    Interrupt,
    Isochronous,
}

#[derive(Clone, Copy)]
struct ClaimedEndpoint {
    transfer_type: EndpointTransferType,
    interface: u8,
}

impl UsbDeviceFile {
    fn live_lease(&self) -> AxResult<Arc<manager::UsbDeviceLease>> {
        let mut lease = self.lease.lock();
        if let Some(lease) = lease.as_ref() {
            return Ok(lease.clone());
        }

        let new_lease = Arc::new(self.manager.acquire_device(self.bus_num, self.device_num)?);
        *lease = Some(new_lease.clone());
        Ok(new_lease)
    }

    fn with_live_lease<R>(
        &self,
        f: impl FnOnce(&manager::UsbDeviceLease) -> AxResult<R>,
    ) -> AxResult<R> {
        let lease = self.live_lease()?;
        f(&lease)
    }

    fn claim_interface(
        &self,
        interface: u8,
        alternate: u8,
        force_reconfigure: bool,
    ) -> AxResult<usize> {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        if !snapshot_has_interface(&self.snapshot, interface, alternate) {
            return Err(AxError::NotFound);
        }
        if self.claimed_interfaces.lock().get(&interface).copied() == Some(alternate) {
            if force_reconfigure {
                debug!(
                    "usbfs: interface {} alt {} already claimed on this fd, treating reconfigure \
                     as no-op",
                    interface, alternate
                );
            }
            return Ok(0);
        }

        let interface_quirk = usbfs_quirk_for_interface(&self.snapshot, interface, alternate);
        self.cancel_submitted_urbs_for_interface(interface)?;
        if interface_quirk != Some(UsbfsQuirk::UserspaceClaimedUvcControlInterface)
            && self
                .submitted_urbs
                .lock()
                .iter()
                .any(|submitted| !submitted.is_deferred())
        {
            return Err(AxError::ResourceBusy);
        }
        self.release_endpoint_handles_for_interface(interface)?;
        if interface_quirk == Some(UsbfsQuirk::UserspaceClaimedUvcControlInterface) {
            self.claimed_interfaces.lock().insert(interface, alternate);
            return Ok(0);
        }
        self.with_live_lease(|lease| lease.claim_interface(interface, alternate))?;
        self.claimed_interfaces.lock().insert(interface, alternate);
        Ok(0)
    }

    fn release_interface(&self, interface: u8) -> AxResult<usize> {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        self.cancel_submitted_urbs_for_interface(interface)?;
        if let Some(lease) = self.lease.lock().as_ref().cloned() {
            lease.release_interface(interface)?;
        } else {
            self.release_endpoint_handles_for_interface(interface)?;
        }
        self.claimed_interfaces.lock().remove(&interface);
        Ok(0)
    }

    fn set_configuration_ioctl(&self, arg: usize) -> AxResult<usize> {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        let configuration = descriptor::read_usbdevfs_u32(arg)?;
        if configuration > u8::MAX as u32 {
            return Err(AxError::InvalidInput);
        }
        self.collect_submitted_urbs(None);
        if !self.claimed_interfaces.lock().is_empty()
            || !self.submitted_urbs.lock().is_empty()
            || !self.pending_urbs.lock().is_empty()
        {
            return Err(AxError::ResourceBusy);
        }
        self.with_live_lease(|lease| lease.set_configuration(configuration as u8))?;
        Ok(0)
    }

    fn cancel_submitted_urbs_for_interface(&self, interface: u8) -> AxResult<()> {
        let remaining = cleanup_submitted_urbs(
            self.drain_submitted_urbs_for_interface(interface),
            Some(USBFS_URB_CANCEL_TIMEOUT),
        );
        if !remaining.is_empty() {
            self.submitted_urbs.lock().extend(remaining);
            return Err(AxError::ResourceBusy);
        }
        Ok(())
    }

    fn drain_submitted_urbs_for_interface(&self, interface: u8) -> Vec<SubmittedUrb> {
        let mut submitted_urbs = self.submitted_urbs.lock();
        let mut drained = Vec::new();
        let mut index = 0;
        while index < submitted_urbs.len() {
            if submitted_urbs[index].interface == Some(interface) {
                drained.push(
                    submitted_urbs
                        .remove(index)
                        .expect("submitted URB disappeared during interface drain"),
                );
            } else {
                index += 1;
            }
        }
        drained
    }

    fn drain_all_submitted_urbs(&self) -> Vec<SubmittedUrb> {
        self.submitted_urbs.lock().drain(..).collect()
    }

    fn drain_submitted_urb_by_ptr(&self, user_urb_ptr: usize) -> AxResult<SubmittedUrb> {
        let mut submitted_urbs = self.submitted_urbs.lock();
        let index = submitted_urbs
            .iter()
            .position(|submitted| submitted.user_urb_ptr == user_urb_ptr)
            .ok_or(AxError::InvalidInput)?;
        submitted_urbs.remove(index).ok_or(AxError::InvalidInput)
    }

    fn release_endpoint_handles_for_interface(&self, interface: u8) -> AxResult<()> {
        let endpoints = claimed_interface_endpoints(&self.snapshot, interface);
        if endpoints.is_empty() {
            return Ok(());
        }
        let lease = self.lease.lock().clone();
        if let Some(lease) = lease {
            lease.release_endpoints(&endpoints)?;
        }
        Ok(())
    }

    fn get_driver_ioctl(&self, arg: usize) -> AxResult<usize> {
        let mut get_driver = (arg as *const descriptor::UsbdevfsGetDriver).vm_read()?;
        if get_driver.interface > u8::MAX as u32 {
            return Err(AxError::InvalidInput);
        }

        get_driver.driver.fill(0);
        get_driver.driver[..5].copy_from_slice(b"usbfs");
        (arg as *mut descriptor::UsbdevfsGetDriver).vm_write(get_driver)?;
        Ok(0)
    }

    fn kernel_driver_ioctl(&self, arg: usize) -> AxResult<usize> {
        let command = descriptor::read_usbdevfs_ioctl(arg)?;
        if command.ifno < 0 || command.ifno > u8::MAX as i32 {
            return Err(AxError::InvalidInput);
        }
        match command.ioctl_code as u32 {
            descriptor::USBDEVFS_DISCONNECT | descriptor::USBDEVFS_CONNECT => Ok(0),
            _ => Err(AxError::Unsupported),
        }
    }

    fn disconnect_claim_ioctl(&self, arg: usize) -> AxResult<usize> {
        let claim = descriptor::read_usbdevfs_disconnect_claim(arg)?;
        if claim.interface > u8::MAX as u32 {
            return Err(AxError::InvalidInput);
        }
        self.claim_interface(claim.interface as u8, 0, false)
    }

    fn claimed_endpoint(&self, endpoint: u8) -> AxResult<ClaimedEndpoint> {
        let claimed = self.claimed_interfaces.lock();
        snapshot_claimed_endpoint(&self.snapshot, endpoint, &claimed)
            .ok_or(AxError::OperationNotPermitted)
    }

    fn run_endpoint_transfer(
        &self,
        endpoint: u8,
        transfer_type: EndpointTransferType,
        data: *mut u8,
        len: usize,
        iso_packet_lengths: &[usize],
    ) -> AxResult<usize> {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        let claimed_endpoint = self.claimed_endpoint(endpoint)?;
        if claimed_endpoint.transfer_type != transfer_type {
            return Err(AxError::InvalidInput);
        }
        self.with_live_lease(|lease| {
            if endpoint & 0x80 != 0 {
                let mut buffer = alloc::vec![0; len];
                let actual = match transfer_type {
                    EndpointTransferType::Bulk => lease.bulk_in(endpoint, &mut buffer)?,
                    EndpointTransferType::Interrupt => lease.interrupt_in(endpoint, &mut buffer)?,
                    EndpointTransferType::Isochronous => {
                        lease
                            .iso_in(endpoint, &mut buffer, iso_packet_lengths)?
                            .actual_length
                    }
                };
                if actual > len {
                    return Err(AxError::InvalidData);
                }
                if actual > 0 {
                    vm_write_slice(data, &buffer[..actual])?;
                }
                Ok(actual)
            } else {
                let buffer = read_user_bytes(data as *const u8, len)?;
                match transfer_type {
                    EndpointTransferType::Bulk => lease.bulk_out(endpoint, &buffer),
                    EndpointTransferType::Interrupt => lease.interrupt_out(endpoint, &buffer),
                    EndpointTransferType::Isochronous => {
                        lease.iso_out(endpoint, &buffer, iso_packet_lengths)
                    }
                }
            }
        })
    }

    fn bulk_ioctl(&self, arg: usize) -> AxResult<usize> {
        let bulk = descriptor::read_usbdevfs_bulktransfer(arg)?;
        if bulk.ep > u8::MAX as u32 {
            return Err(AxError::InvalidInput);
        }
        self.run_endpoint_transfer(
            bulk.ep as u8,
            EndpointTransferType::Bulk,
            bulk.data,
            bulk.len as usize,
            &[],
        )
    }

    fn read_iso_packet_lengths(&self, urb_ptr: usize, num_packets: usize) -> AxResult<Vec<usize>> {
        let packet_descs = read_iso_packet_descs(urb_ptr, num_packets)?;
        let mut total_length = 0usize;
        let mut packet_lengths = Vec::with_capacity(num_packets);
        for packet_desc in &packet_descs {
            let packet_length = packet_desc.length as usize;
            total_length = total_length
                .checked_add(packet_length)
                .ok_or(AxError::OutOfRange)?;
            packet_lengths.push(packet_length);
        }
        Ok(packet_lengths)
    }

    fn write_iso_packet_results(
        &self,
        urb_ptr: usize,
        packet_lengths: &[usize],
        actual_total: usize,
        packet_actual_lengths: &[usize],
    ) -> AxResult<()> {
        let mut packet_descs = read_iso_packet_descs(urb_ptr, packet_lengths.len())?;
        if !packet_actual_lengths.is_empty() {
            if packet_actual_lengths.len() != packet_lengths.len() {
                return Err(AxError::InvalidData);
            }
            for (packet_desc, packet_actual) in packet_descs.iter_mut().zip(packet_actual_lengths) {
                packet_desc.actual_length = (*packet_actual).min(u32::MAX as usize) as u32;
                packet_desc.status = 0;
            }
            return write_iso_packet_descs(urb_ptr, &packet_descs);
        }

        let mut remaining = actual_total;
        for (packet_desc, packet_length) in packet_descs.iter_mut().zip(packet_lengths.iter()) {
            let packet_actual = remaining.min(*packet_length);
            packet_desc.actual_length = packet_actual as u32;
            packet_desc.status = 0;
            remaining -= packet_actual;
        }
        write_iso_packet_descs(urb_ptr, &packet_descs)
    }

    fn write_completed_urb(&self, completed: CompletedUrb) -> AxResult<()> {
        let mut urb = (completed.user_urb_ptr as *const descriptor::UsbdevfsUrb).vm_read()?;
        let buffer = urb.buffer;
        let buffer_length = urb.buffer_length;

        match completed.result {
            Ok(result) => {
                if !result.data.is_empty() {
                    let user_len = buffer_length.max(0) as usize;
                    if result.data_offset > user_len {
                        return Err(AxError::InvalidInput);
                    }
                    let copy_len = result.data.len().min(user_len - result.data_offset);
                    let buffer_ptr = (buffer as usize)
                        .checked_add(result.data_offset)
                        .ok_or(AxError::InvalidInput)?
                        as *mut u8;
                    vm_write_slice(buffer_ptr, &result.data[..copy_len])?;
                }
                if !result.packet_lengths.is_empty() {
                    self.write_iso_packet_results(
                        completed.user_urb_ptr,
                        &result.packet_lengths,
                        result.actual_length,
                        &result.packet_actual_lengths,
                    )?;
                }
                urb.status = 0;
                urb.actual_length = result.actual_length as i32;
                urb.error_count = 0;
                (completed.user_urb_ptr as *mut descriptor::UsbdevfsUrb).vm_write(urb)?;
                if completed.log {
                    debug!(
                        "usbfs: reap urb ptr={:#x} status=0 actual={} packets={}",
                        completed.user_urb_ptr,
                        result.actual_length,
                        result.packet_lengths.len()
                    );
                }
            }
            Err(err) => {
                let linux_error = LinuxError::from(err);
                let status = -linux_error.code();
                urb.status = status;
                urb.actual_length = 0;
                urb.error_count = 1;
                (completed.user_urb_ptr as *mut descriptor::UsbdevfsUrb).vm_write(urb)?;
                if completed.log {
                    if matches!(
                        linux_error,
                        LinuxError::ECONNRESET | LinuxError::EINTR | LinuxError::ENOENT
                    ) {
                        debug!(
                            "usbfs: reap urb ptr={:#x} status={} err={:?}",
                            completed.user_urb_ptr, status, err
                        );
                    } else {
                        warn!(
                            "usbfs: reap urb ptr={:#x} status={} err={:?}",
                            completed.user_urb_ptr, status, err
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn transfer_completion_to_result(
        mut submitted: SubmittedUrb,
        completion: TransferCompletion,
    ) -> UrbTransferResult {
        let data = if submitted.is_in {
            let actual =
                if submitted.packet_lengths.is_empty() || completion.iso_packets.is_empty() {
                    completion.actual_length
                } else {
                    iso_copy_len(&submitted.packet_lengths, &completion.iso_packets)
                }
                .min(submitted.buffer.len());
            submitted.buffer.truncate(actual);
            submitted.buffer
        } else {
            Vec::new()
        };

        let packet_actual_lengths =
            iso_packet_actual_lengths(&submitted.packet_lengths, submitted.is_in, &completion);

        UrbTransferResult {
            data,
            data_offset: submitted.data_offset,
            actual_length: completion.actual_length,
            packet_lengths: submitted.packet_lengths,
            packet_actual_lengths,
        }
    }

    fn complete_submitted_urb(
        &self,
        submitted: SubmittedUrb,
        result: AxResult<TransferCompletion>,
    ) {
        if submitted.log {
            match &result {
                Ok(completion) => debug!(
                    "usbfs: complete urb ptr={:#x} actual={} packets={}",
                    submitted.user_urb_ptr,
                    completion.actual_length,
                    completion.iso_packets.len()
                ),
                Err(err) => warn!(
                    "usbfs: complete urb ptr={:#x} err={:?}",
                    submitted.user_urb_ptr, err
                ),
            }
        }
        let user_urb_ptr = submitted.user_urb_ptr;
        let log = submitted.log;
        complete_urb(
            &self.pending_urbs,
            &self.poll_urbs,
            completed_urb_from_result(user_urb_ptr, log, submitted, result),
        );
    }

    fn collect_submitted_urbs(&self, mut cx: Option<&mut Context<'_>>) -> bool {
        let mut ready = Vec::new();
        {
            let mut submitted_urbs = self.submitted_urbs.lock();
            let mut index = 0;
            while index < submitted_urbs.len() {
                let result = match cx.as_mut() {
                    Some(cx) => match submitted_urbs[index].poll_reclaim(cx) {
                        Poll::Ready(result) => Some(result),
                        Poll::Pending => None,
                    },
                    None => match submitted_urbs[index].try_reclaim() {
                        Ok(Some(completion)) => Some(Ok(completion)),
                        Ok(None) => None,
                        Err(err) => Some(Err(err)),
                    },
                };

                if let Some(result) = result {
                    let submitted = submitted_urbs
                        .remove(index)
                        .expect("pending submitted URB disappeared");
                    ready.push((submitted, result));
                } else {
                    index += 1;
                }
            }
        }

        let found_ready = !ready.is_empty();
        for (submitted, result) in ready {
            self.complete_submitted_urb(submitted, result);
        }
        found_ready
    }

    fn ensure_urb_worker(&self) {
        if !self.urb_worker.try_start() {
            self.urb_worker.notify();
            return;
        }
        let submitted_urbs = self.submitted_urbs.clone();
        let pending_urbs = self.pending_urbs.clone();
        let poll_urbs = self.poll_urbs.clone();
        let worker = self.urb_worker.clone();
        let manager = self.manager.clone();
        crate::task::spawn_kernel_thread(
            move || {
                crate::task::future::block_on(async {
                    loop {
                        let mut ready = Vec::new();
                        {
                            let mut submitted = submitted_urbs.lock();
                            let mut index = 0;
                            while index < submitted.len() {
                                let result = match submitted[index].try_reclaim() {
                                    Ok(Some(completion)) => Some(Ok(completion)),
                                    Ok(None) => None,
                                    Err(err) => Some(Err(err)),
                                };
                                if let Some(result) = result {
                                    ready.push((
                                        submitted.remove(index).expect("submitted URB disappeared"),
                                        result,
                                    ));
                                } else {
                                    index += 1;
                                }
                            }
                        }

                        for (submitted, result) in ready {
                            complete_urb(
                                &pending_urbs,
                                &poll_urbs,
                                completed_urb_from_result(
                                    submitted.user_urb_ptr,
                                    submitted.log,
                                    submitted,
                                    result,
                                ),
                            );
                        }

                        if worker.closed.load(Ordering::Acquire) {
                            break;
                        }
                        let activity_seq = manager.usb_activity_seq();
                        let wake_listener = worker.wake_event.listen();
                        let activity_listener = manager.listen_usb_activity();
                        let mut wake_listener = pin!(wake_listener);
                        let mut activity_listener = pin!(activity_listener);
                        let has_live = submitted_urbs
                            .lock()
                            .iter()
                            .any(|submitted| !submitted.is_deferred());
                        if !has_live {
                            poll_fn(|cx| {
                                if worker.closed.load(Ordering::Acquire)
                                    || manager.usb_activity_seq() != activity_seq
                                    || wake_listener.as_mut().poll(cx).is_ready()
                                    || activity_listener.as_mut().poll(cx).is_ready()
                                {
                                    Poll::Ready(())
                                } else {
                                    Poll::Pending
                                }
                            })
                            .await;
                            continue;
                        }

                        let completed = poll_fn(|cx| {
                            if worker.closed.load(Ordering::Acquire)
                                || wake_listener.as_mut().poll(cx).is_ready()
                            {
                                return Poll::Ready(None);
                            }
                            let usb_activity_ready = manager.usb_activity_seq() != activity_seq
                                || activity_listener.as_mut().poll(cx).is_ready();
                            let mut submitted = submitted_urbs.lock();
                            let mut index = 0;
                            while index < submitted.len() {
                                if submitted[index].is_deferred() {
                                    index += 1;
                                    continue;
                                }
                                match submitted[index].poll_reclaim(cx) {
                                    Poll::Ready(result) => {
                                        let submitted = submitted
                                            .remove(index)
                                            .expect("submitted URB disappeared");
                                        return Poll::Ready(Some((submitted, result)));
                                    }
                                    Poll::Pending => index += 1,
                                }
                            }
                            if usb_activity_ready {
                                Poll::Ready(None)
                            } else {
                                Poll::Pending
                            }
                        })
                        .await;
                        if let Some((submitted, result)) = completed {
                            complete_urb(
                                &pending_urbs,
                                &poll_urbs,
                                completed_urb_from_result(
                                    submitted.user_urb_ptr,
                                    submitted.log,
                                    submitted,
                                    result,
                                ),
                            );
                        } else {
                            crate::task::yield_now();
                        }
                    }
                });
                worker.stop();
            },
            "usbfs-urb-worker".to_owned(),
        );
    }

    fn submit_endpoint_urb_async(
        &self,
        arg: usize,
        expected_urb_type: u8,
        transfer_type: EndpointTransferType,
        packet_lengths: Vec<usize>,
        total_length: usize,
    ) -> AxResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read()?;
        let (urb_type, endpoint, buffer, buffer_length) =
            (urb.type_, urb.endpoint, urb.buffer, urb.buffer_length);
        if urb_type != expected_urb_type {
            return Err(ax_errno::AxError::Unsupported);
        }
        if buffer_length < 0 || total_length > buffer_length as usize {
            return Err(ax_errno::AxError::InvalidInput);
        }

        let claimed_endpoint = self.claimed_endpoint(endpoint)?;
        if claimed_endpoint.transfer_type != transfer_type {
            return Err(AxError::InvalidInput);
        }

        let is_in = endpoint & 0x80 != 0;
        let mut buffer = if is_in {
            alloc::vec![0; total_length]
        } else {
            read_user_bytes(buffer as *const u8, total_length)?
        };

        let log = usbfs_should_log_urb();

        if log {
            debug!(
                "usbfs: submit urb ptr={:#x} type={:?} ep={:#04x} len={} packets={} dir={}",
                arg,
                transfer_type,
                endpoint,
                total_length,
                packet_lengths.len(),
                if is_in { "in" } else { "out" }
            );
        }

        let request = match (transfer_type, is_in) {
            (EndpointTransferType::Bulk, true) => TransferRequest::bulk_in(&mut buffer),
            (EndpointTransferType::Bulk, false) => TransferRequest::bulk_out(&buffer),
            (EndpointTransferType::Interrupt, true) => TransferRequest::interrupt_in(&mut buffer),
            (EndpointTransferType::Interrupt, false) => TransferRequest::interrupt_out(&buffer),
            (EndpointTransferType::Isochronous, true) => {
                TransferRequest::iso_in(&mut buffer, &packet_lengths)
            }
            (EndpointTransferType::Isochronous, false) => {
                TransferRequest::iso_out(&buffer, &packet_lengths)
            }
        };

        self.collect_submitted_urbs(None);
        let mut transfer =
            self.with_live_lease(|lease| lease.submit_endpoint_transfer(endpoint, request));
        if matches!(&transfer, Err(AxError::ResourceBusy)) {
            self.collect_submitted_urbs(None);
            let request = match (transfer_type, is_in) {
                (EndpointTransferType::Bulk, true) => TransferRequest::bulk_in(&mut buffer),
                (EndpointTransferType::Bulk, false) => TransferRequest::bulk_out(&buffer),
                (EndpointTransferType::Interrupt, true) => {
                    TransferRequest::interrupt_in(&mut buffer)
                }
                (EndpointTransferType::Interrupt, false) => TransferRequest::interrupt_out(&buffer),
                (EndpointTransferType::Isochronous, true) => {
                    TransferRequest::iso_in(&mut buffer, &packet_lengths)
                }
                (EndpointTransferType::Isochronous, false) => {
                    TransferRequest::iso_out(&buffer, &packet_lengths)
                }
            };
            transfer =
                self.with_live_lease(|lease| lease.submit_endpoint_transfer(endpoint, request));
        }
        if let Err(err) = &transfer {
            warn!(
                "usbfs: submit endpoint urb failed ep={:#04x} type={:?} len={} packets={} err={:?}",
                endpoint,
                transfer_type,
                total_length,
                packet_lengths.len(),
                err
            );
        }
        let transfer = transfer?;
        if log {
            debug!("usbfs: submit endpoint urb queued ptr={:#x}", arg);
        }
        let submitted = SubmittedUrb {
            user_urb_ptr: arg,
            transfer: SubmittedUrbTransfer::Live(transfer),
            interface: Some(claimed_endpoint.interface),
            buffer,
            is_in,
            data_offset: 0,
            packet_lengths,
            log,
        };
        self.submitted_urbs.lock().push_back(submitted);
        self.ensure_urb_worker();

        Ok(0)
    }

    fn submit_control_urb(&self, arg: usize) -> AxResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read()?;
        let (urb_type, urb_buffer, buffer_length) = (urb.type_, urb.buffer, urb.buffer_length);
        if urb_type != descriptor::USBDEVFS_URB_TYPE_CONTROL {
            return Err(ax_errno::AxError::Unsupported);
        }
        if buffer_length < 8 {
            return Err(ax_errno::AxError::InvalidInput);
        }

        let mut setup_bytes = [0u8; 8];
        read_user_bytes_into(urb_buffer as *const u8, &mut setup_bytes)?;
        let b_request_type = setup_bytes[0];
        let b_request = setup_bytes[1];
        let w_value = u16::from_le_bytes([setup_bytes[2], setup_bytes[3]]);
        let w_index = u16::from_le_bytes([setup_bytes[4], setup_bytes[5]]);
        let w_length = u16::from_le_bytes([setup_bytes[6], setup_bytes[7]]) as usize;
        if (buffer_length as usize) < 8 + w_length {
            return Err(ax_errno::AxError::InvalidInput);
        }

        let log = usbfs_should_log_urb();
        if log {
            debug!(
                "usbfs: submit control urb ptr={:#x} req_type={:#04x} req={:#04x} value={:#06x} \
                 index={:#06x} len={}",
                arg, b_request_type, b_request, w_value, w_index, w_length
            );
        }

        let is_in = b_request_type & 0x80 != 0;
        let setup = manager::control_setup_from_raw(b_request_type, b_request, w_value, w_index);
        let mut buffer = if is_in {
            alloc::vec![0; w_length]
        } else {
            let data_ptr = (urb_buffer as usize)
                .checked_add(8)
                .ok_or(AxError::InvalidInput)? as *const u8;
            read_user_bytes(data_ptr, w_length)?
        };
        let request = match is_in {
            true => TransferRequest::control_in(setup, &mut buffer),
            false => TransferRequest::control_out(setup, &buffer),
        };

        self.collect_submitted_urbs(None);
        let mut transfer = self.with_live_lease(|lease| lease.submit_control_transfer(request));
        if matches!(&transfer, Err(AxError::ResourceBusy)) {
            self.collect_submitted_urbs(None);
            let setup =
                manager::control_setup_from_raw(b_request_type, b_request, w_value, w_index);
            let request = match is_in {
                true => TransferRequest::control_in(setup, &mut buffer),
                false => TransferRequest::control_out(setup, &buffer),
            };
            transfer = self.with_live_lease(|lease| lease.submit_control_transfer(request));
        }
        let transfer = transfer?;
        if log {
            debug!("usbfs: submit control urb queued ptr={:#x}", arg);
        }
        let submitted = SubmittedUrb {
            user_urb_ptr: arg,
            transfer: SubmittedUrbTransfer::Live(transfer),
            interface: None,
            buffer,
            is_in,
            data_offset: 8,
            packet_lengths: Vec::new(),
            log,
        };
        self.submitted_urbs.lock().push_back(submitted);
        self.ensure_urb_worker();
        Ok(0)
    }

    fn submit_bulk_urb(&self, arg: usize) -> AxResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read()?;
        if urb.type_ != descriptor::USBDEVFS_URB_TYPE_BULK {
            return Err(ax_errno::AxError::Unsupported);
        }
        if urb.buffer_length < 0 {
            return Err(ax_errno::AxError::InvalidInput);
        }

        self.submit_endpoint_urb_async(
            arg,
            descriptor::USBDEVFS_URB_TYPE_BULK,
            EndpointTransferType::Bulk,
            Vec::new(),
            urb.buffer_length as usize,
        )
    }

    fn submit_interrupt_urb(&self, arg: usize) -> AxResult<usize> {
        let mut urb = (arg as *const descriptor::UsbdevfsUrb).vm_read()?;
        if urb.type_ != descriptor::USBDEVFS_URB_TYPE_INTERRUPT {
            return Err(ax_errno::AxError::Unsupported);
        }
        if urb.buffer_length < 0 {
            return Err(ax_errno::AxError::InvalidInput);
        }
        if usbfs_quirk_for_interrupt_endpoint(&self.snapshot, urb.endpoint)
            == Some(UsbfsQuirk::DeferredStatusInterrupt)
        {
            let claimed_endpoint = self.claimed_endpoint(urb.endpoint)?;
            if claimed_endpoint.transfer_type != EndpointTransferType::Interrupt {
                return Err(AxError::InvalidInput);
            }
            urb.status = 0;
            urb.actual_length = 0;
            (arg as *mut descriptor::UsbdevfsUrb).vm_write(urb)?;
            self.submitted_urbs.lock().push_back(SubmittedUrb {
                user_urb_ptr: arg,
                transfer: SubmittedUrbTransfer::Deferred(UsbfsQuirk::DeferredStatusInterrupt),
                interface: Some(claimed_endpoint.interface),
                buffer: Vec::new(),
                is_in: true,
                data_offset: 0,
                packet_lengths: Vec::new(),
                log: usbfs_should_log_urb(),
            });
            return Ok(0);
        }

        self.submit_endpoint_urb_async(
            arg,
            descriptor::USBDEVFS_URB_TYPE_INTERRUPT,
            EndpointTransferType::Interrupt,
            Vec::new(),
            urb.buffer_length as usize,
        )
    }

    fn submit_iso_urb(&self, arg: usize) -> AxResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read()?;
        if urb.type_ != descriptor::USBDEVFS_URB_TYPE_ISO {
            return Err(ax_errno::AxError::Unsupported);
        }
        if urb.buffer_length < 0 || urb.number_of_packets <= 0 {
            return Err(ax_errno::AxError::InvalidInput);
        }
        let supported_flags =
            descriptor::USBDEVFS_URB_ISO_ASAP | descriptor::USBDEVFS_URB_SHORT_NOT_OK;
        if urb.flags & !supported_flags != 0 {
            return Err(AxError::Unsupported);
        }
        if urb.flags & descriptor::USBDEVFS_URB_ISO_ASAP == 0 && urb.start_frame != 0 {
            return Err(AxError::Unsupported);
        }

        let packet_lengths = self.read_iso_packet_lengths(arg, urb.number_of_packets as usize)?;
        let total_length = packet_lengths.iter().try_fold(0usize, |acc, len| {
            acc.checked_add(*len).ok_or(AxError::OutOfRange)
        })?;
        if total_length > urb.buffer_length as usize {
            return Err(AxError::InvalidInput);
        }

        self.submit_endpoint_urb_async(
            arg,
            descriptor::USBDEVFS_URB_TYPE_ISO,
            EndpointTransferType::Isochronous,
            packet_lengths,
            total_length,
        )
    }

    fn submit_urb(&self, arg: usize) -> AxResult<usize> {
        self.collect_submitted_urbs(None);
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read()?;
        let type_ = urb.type_;
        match type_ {
            descriptor::USBDEVFS_URB_TYPE_CONTROL => {
                let _lifecycle_guard = self.lifecycle_lock.lock();
                self.submit_control_urb(arg)
            }
            descriptor::USBDEVFS_URB_TYPE_BULK => self.submit_bulk_urb(arg),
            descriptor::USBDEVFS_URB_TYPE_INTERRUPT => self.submit_interrupt_urb(arg),
            descriptor::USBDEVFS_URB_TYPE_ISO => self.submit_iso_urb(arg),
            _ => Err(ax_errno::AxError::Unsupported),
        }
    }

    fn reap_urb(&self, arg: usize, nonblocking: bool) -> AxResult<usize> {
        self.collect_submitted_urbs(None);
        if !nonblocking && self.pending_urbs.lock().is_empty() {
            crate::task::future::block_on(poll_fn(|cx| {
                if self.collect_submitted_urbs(None) || !self.pending_urbs.lock().is_empty() {
                    Poll::Ready(())
                } else {
                    // Registration happens from usbfs reap task context.
                    unsafe {
                        self.poll_urbs
                            .register(cx.waker(), IoEvents::IN | IoEvents::OUT)
                    };
                    if self.collect_submitted_urbs(Some(cx)) || !self.pending_urbs.lock().is_empty()
                    {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                }
            }));
        }
        let Some(completed) = self.pending_urbs.lock().pop_front() else {
            return Err(ax_errno::AxError::WouldBlock);
        };
        let user_urb_ptr = completed.user_urb_ptr;
        self.write_completed_urb(completed)?;
        (arg as *mut usize).vm_write(user_urb_ptr)?;
        if usbfs_should_log_urb() {
            debug!("usbfs: reap urb returns ptr={:#x}", user_urb_ptr);
        }
        Ok(0)
    }

    fn discard_urb(&self, arg: usize) -> AxResult<usize> {
        let submitted = self.drain_submitted_urb_by_ptr(arg)?;
        submitted.cancel()?;

        complete_urb(
            &self.pending_urbs,
            &self.poll_urbs,
            CompletedUrb {
                user_urb_ptr: submitted.user_urb_ptr,
                result: Err(AxError::from(LinuxError::ENOENT)),
                log: submitted.log,
            },
        );

        if !submitted.is_deferred() {
            let lease = self.lease.lock().clone();
            crate::task::spawn_kernel_thread(
                move || {
                    let _lease = lease;
                    cleanup_submitted_urbs(alloc::vec![submitted], None);
                },
                "usbfs-urb-discard".to_owned(),
            );
        }
        Ok(0)
    }
}

impl FileLike for UsbDeviceFile {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        self.base.read(dst)
    }

    fn write(&self, src: &mut IoSrc) -> AxResult<usize> {
        self.base.write(src)
    }

    fn stat(&self) -> AxResult<Kstat> {
        self.base.stat()
    }

    fn path(&self) -> alloc::borrow::Cow<'_, str> {
        self.base.path()
    }

    fn file_mmap(&self) -> AxResult<(ax_fs_ng::vfs::FileBackend, ax_fs_ng::vfs::FileFlags)> {
        self.base.file_mmap()
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        match cmd {
            descriptor::USBDEVFS_CONTROL => {
                let log = usbfs_should_log_urb();
                let ctrl = descriptor::read_usbdevfs_ctrltransfer(arg).ok();
                if let Some(ctrl) = ctrl
                    && log
                {
                    debug!(
                        "usbfs: control ioctl req_type={:#04x} req={:#04x} value={:#06x} \
                         index={:#06x} len={}",
                        ctrl.b_request_type,
                        ctrl.b_request,
                        ctrl.w_value,
                        ctrl.w_index,
                        ctrl.w_length
                    );
                }
                match manager::is_snapshot_control_ioctl(arg) {
                    Ok(true) => {
                        let result = self.manager.snapshot_device_ioctl(
                            self.bus_num,
                            self.device_num,
                            cmd,
                            arg,
                        );
                        if log {
                            debug!("usbfs: snapshot control ioctl result={:?}", result);
                        }
                        return result;
                    }
                    Ok(false) => {}
                    Err(err) => return Err(err),
                }
                let result = self.with_live_lease(|lease| lease.ioctl(cmd, arg));
                if log {
                    debug!("usbfs: control ioctl result={:?}", result);
                }
                result
            }
            descriptor::USBDEVFS_CLAIMINTERFACE => {
                let interface = descriptor::read_usbdevfs_u32(arg)?;
                if interface > u8::MAX as u32 {
                    return Err(AxError::InvalidInput);
                }
                self.claim_interface(interface as u8, 0, false)
            }
            descriptor::USBDEVFS_RELEASEINTERFACE => {
                let interface = descriptor::read_usbdevfs_u32(arg)?;
                if interface > u8::MAX as u32 {
                    return Err(AxError::InvalidInput);
                }
                self.release_interface(interface as u8)
            }
            descriptor::USBDEVFS_GETDRIVER => self.get_driver_ioctl(arg),
            descriptor::USBDEVFS_SETINTERFACE => {
                let set = descriptor::read_usbdevfs_setinterface(arg)?;
                if set.interface > u8::MAX as u32 || set.altsetting > u8::MAX as u32 {
                    return Err(AxError::InvalidInput);
                }
                self.claim_interface(set.interface as u8, set.altsetting as u8, true)
            }
            descriptor::USBDEVFS_SETCONFIGURATION => self.set_configuration_ioctl(arg),
            descriptor::USBDEVFS_IOCTL => self.kernel_driver_ioctl(arg),
            descriptor::USBDEVFS_DISCONNECT | descriptor::USBDEVFS_CONNECT => Ok(0),
            descriptor::USBDEVFS_DISCONNECT_CLAIM => self.disconnect_claim_ioctl(arg),
            descriptor::USBDEVFS_DISCARDURB => self.discard_urb(arg),
            descriptor::USBDEVFS_BULK => self.bulk_ioctl(arg),
            descriptor::USBDEVFS_SUBMITURB => self.submit_urb(arg),
            descriptor::USBDEVFS_REAPURB => self.reap_urb(arg, false),
            descriptor::USBDEVFS_REAPURBNDELAY => self.reap_urb(arg, true),
            descriptor::USBDEVFS_CONNECTINFO | descriptor::USBDEVFS_GET_CAPABILITIES => self
                .manager
                .snapshot_device_ioctl(self.bus_num, self.device_num, cmd, arg),
            _ => self.with_live_lease(|lease| lease.ioctl(cmd, arg)),
        }
    }

    fn open_flags(&self) -> u32 {
        self.base.open_flags()
    }

    fn nonblocking(&self) -> bool {
        self.base.nonblocking()
    }

    fn set_nonblocking(&self, flag: bool) -> AxResult {
        self.base.set_nonblocking(flag)
    }
}

impl Pollable for UsbDeviceFile {
    fn poll(&self) -> IoEvents {
        self.collect_submitted_urbs(None);
        if self.pending_urbs.lock().is_empty() {
            IoEvents::empty()
        } else {
            IoEvents::IN | IoEvents::OUT
        }
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.intersects(IoEvents::IN | IoEvents::OUT) {
            // Registration happens from usbfs poll task context.
            unsafe {
                self.poll_urbs
                    .register(context.waker(), events & (IoEvents::IN | IoEvents::OUT))
            };
            if self.collect_submitted_urbs(Some(context)) || !self.pending_urbs.lock().is_empty() {
                context.waker().wake_by_ref();
            }
        }
    }
}

impl Drop for UsbDeviceFile {
    fn drop(&mut self) {
        self.urb_worker.close();
        let lease = self.lease.lock().take();
        let submitted = self.drain_all_submitted_urbs();
        self.pending_urbs.lock().clear();
        if submitted.is_empty() {
            drop(lease);
            return;
        }

        crate::task::spawn_kernel_thread(
            move || {
                let _lease = lease;
                cleanup_submitted_urbs(submitted, None);
            },
            "usbfs-urb-cleanup".to_owned(),
        );
    }
}

fn complete_urb(
    pending_urbs: &Arc<Mutex<VecDeque<CompletedUrb>>>,
    poll_urbs: &Arc<PollSet>,
    completed: CompletedUrb,
) {
    {
        pending_urbs.lock().push_back(completed);
    }
    // Completed URB is queued before waking poll/reap waiters.
    unsafe { poll_urbs.wake(IoEvents::IN | IoEvents::OUT) };
}

fn completed_urb_from_result(
    user_urb_ptr: usize,
    log: bool,
    submitted: SubmittedUrb,
    result: AxResult<TransferCompletion>,
) -> CompletedUrb {
    CompletedUrb {
        user_urb_ptr,
        result: result
            .map(|completion| UsbDeviceFile::transfer_completion_to_result(submitted, completion)),
        log,
    }
}

fn cleanup_submitted_urbs(
    mut submitted_urbs: Vec<SubmittedUrb>,
    timeout: Option<Duration>,
) -> Vec<SubmittedUrb> {
    let deadline = timeout.map(|timeout| ax_runtime::hal::time::wall_time() + timeout);
    for submitted in &submitted_urbs {
        if let Err(err) = submitted.cancel() {
            debug!(
                "usbfs: failed to cancel submitted URB ptr={:#x} during cleanup: {err:?}",
                submitted.user_urb_ptr
            );
        }
    }

    while !submitted_urbs.is_empty() {
        let mut index = 0;
        while index < submitted_urbs.len() {
            if submitted_urbs[index].is_deferred() {
                submitted_urbs.swap_remove(index);
                continue;
            }
            match submitted_urbs[index].try_reclaim() {
                Ok(Some(_)) | Err(_) => {
                    submitted_urbs.swap_remove(index);
                }
                Ok(None) => {
                    index += 1;
                }
            }
        }

        if !submitted_urbs.is_empty() {
            if deadline.is_some_and(|deadline| ax_runtime::hal::time::wall_time() >= deadline) {
                break;
            }
            crate::task::sleep(Duration::from_millis(1));
        }
    }

    submitted_urbs
}

fn usbfs_should_log_urb() -> bool {
    USBFS_URB_LOG_BUDGET
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |budget| {
            budget.checked_sub(1)
        })
        .is_ok()
}

fn snapshot_has_interface(
    snapshot: &descriptor::UsbDeviceSnapshot,
    interface_number: u8,
    alternate_setting: u8,
) -> bool {
    let mut cursor = 18usize;
    while cursor + 2 <= snapshot.descriptor_blob.len() {
        let length = snapshot.descriptor_blob[cursor] as usize;
        if length < 2 || cursor + length > snapshot.descriptor_blob.len() {
            return false;
        }
        if snapshot.descriptor_blob[cursor + 1] == 0x04
            && length >= 9
            && snapshot.descriptor_blob[cursor + 2] == interface_number
            && snapshot.descriptor_blob[cursor + 3] == alternate_setting
        {
            return true;
        }
        cursor += length;
    }
    false
}

fn usbfs_quirk_for_interface(
    snapshot: &descriptor::UsbDeviceSnapshot,
    interface_number: u8,
    alternate_setting: u8,
) -> Option<UsbfsQuirk> {
    let mut cursor = 18usize;
    while cursor + 2 <= snapshot.descriptor_blob.len() {
        let length = snapshot.descriptor_blob[cursor] as usize;
        if length < 2 || cursor + length > snapshot.descriptor_blob.len() {
            return None;
        }
        if snapshot.descriptor_blob[cursor + 1] == 0x04
            && length >= 9
            && snapshot.descriptor_blob[cursor + 2] == interface_number
            && snapshot.descriptor_blob[cursor + 3] == alternate_setting
        {
            return (snapshot.descriptor_blob[cursor + 5] == 0x0e
                && snapshot.descriptor_blob[cursor + 6] == 0x01)
                .then_some(UsbfsQuirk::UserspaceClaimedUvcControlInterface);
        }
        cursor += length;
    }
    None
}

fn usbfs_quirk_for_interrupt_endpoint(
    snapshot: &descriptor::UsbDeviceSnapshot,
    endpoint: u8,
) -> Option<UsbfsQuirk> {
    let mut cursor = 18usize;
    let mut is_uvc_control_interface = false;

    while cursor + 2 <= snapshot.descriptor_blob.len() {
        let length = snapshot.descriptor_blob[cursor] as usize;
        if length < 2 || cursor + length > snapshot.descriptor_blob.len() {
            return None;
        }

        match snapshot.descriptor_blob[cursor + 1] {
            0x04 if length >= 9 => {
                is_uvc_control_interface = snapshot.descriptor_blob[cursor + 5] == 0x0e
                    && snapshot.descriptor_blob[cursor + 6] == 0x01;
            }
            0x05 if length >= 7 && snapshot.descriptor_blob[cursor + 2] == endpoint => {
                return (is_uvc_control_interface
                    && (snapshot.descriptor_blob[cursor + 3] & 0x03) == 3)
                    .then_some(UsbfsQuirk::DeferredStatusInterrupt);
            }
            _ => {}
        }
        cursor += length;
    }
    None
}

fn snapshot_claimed_endpoint(
    snapshot: &descriptor::UsbDeviceSnapshot,
    endpoint: u8,
    claimed_interfaces: &alloc::collections::BTreeMap<u8, u8>,
) -> Option<ClaimedEndpoint> {
    let mut cursor = 18usize;
    let mut current_interface = None;
    let mut current_alternate = 0u8;

    while cursor + 2 <= snapshot.descriptor_blob.len() {
        let length = snapshot.descriptor_blob[cursor] as usize;
        if length < 2 || cursor + length > snapshot.descriptor_blob.len() {
            return None;
        }

        match snapshot.descriptor_blob[cursor + 1] {
            0x04 if length >= 9 => {
                current_interface = Some(snapshot.descriptor_blob[cursor + 2]);
                current_alternate = snapshot.descriptor_blob[cursor + 3];
            }
            0x05 if length >= 7 && snapshot.descriptor_blob[cursor + 2] == endpoint => {
                let interface = current_interface?;
                if claimed_interfaces.get(&interface).copied() == Some(current_alternate) {
                    let transfer_type = match snapshot.descriptor_blob[cursor + 3] & 0x03 {
                        1 => EndpointTransferType::Isochronous,
                        2 => EndpointTransferType::Bulk,
                        3 => EndpointTransferType::Interrupt,
                        _ => return None,
                    };
                    return Some(ClaimedEndpoint {
                        transfer_type,
                        interface,
                    });
                }
            }
            _ => {}
        }

        cursor += length;
    }

    None
}

fn claimed_interface_endpoints(
    snapshot: &descriptor::UsbDeviceSnapshot,
    interface_number: u8,
) -> Vec<u8> {
    let mut endpoints = Vec::new();
    let mut cursor = 18usize;
    let mut current_interface = None;

    while cursor + 2 <= snapshot.descriptor_blob.len() {
        let length = snapshot.descriptor_blob[cursor] as usize;
        if length < 2 || cursor + length > snapshot.descriptor_blob.len() {
            break;
        }

        match snapshot.descriptor_blob[cursor + 1] {
            0x04 if length >= 9 => {
                current_interface = Some(snapshot.descriptor_blob[cursor + 2]);
            }
            0x05 if length >= 7 && current_interface == Some(interface_number) => {
                endpoints.push(snapshot.descriptor_blob[cursor + 2]);
            }
            _ => {}
        }

        cursor += length;
    }

    endpoints.sort_unstable();
    endpoints.dedup();
    endpoints
}

fn iso_copy_len(
    packet_lengths: &[usize],
    packet_results: &[crab_usb::usb_if::endpoint::IsoPacketResult],
) -> usize {
    if packet_results.len() != packet_lengths.len() {
        return packet_lengths.iter().sum();
    }

    let mut offset = 0usize;
    let mut copy_len = 0usize;
    for (requested, packet) in packet_lengths.iter().copied().zip(packet_results.iter()) {
        let actual = packet.actual_length.min(requested);
        if actual > 0 {
            copy_len = copy_len.max(offset.saturating_add(actual));
        }
        offset = offset.saturating_add(requested);
    }
    copy_len
}

fn iso_packet_actual_lengths(
    packet_lengths: &[usize],
    is_in: bool,
    completion: &TransferCompletion,
) -> Vec<usize> {
    if packet_lengths.is_empty() {
        return Vec::new();
    }

    if !is_in && completion.iso_packets.len() == packet_lengths.len() {
        return packet_lengths.to_vec();
    }

    completion
        .iso_packets
        .iter()
        .map(|packet| packet.actual_length)
        .collect()
}

fn iso_packet_descs_ptr(urb_ptr: usize) -> AxResult<*mut descriptor::UsbdevfsIsoPacketDesc> {
    urb_ptr
        .checked_add(size_of::<descriptor::UsbdevfsUrb>())
        .map(|offset| offset as *mut descriptor::UsbdevfsIsoPacketDesc)
        .ok_or(AxError::OutOfRange)
}

fn read_user_bytes(ptr: *const u8, len: usize) -> AxResult<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    vm_load(ptr, len).map_err(Into::into)
}

fn read_user_bytes_into(ptr: *const u8, dst: &mut [u8]) -> AxResult<()> {
    if dst.is_empty() {
        return Ok(());
    }
    let bytes = read_user_bytes(ptr, dst.len())?;
    dst.copy_from_slice(&bytes);
    Ok(())
}

fn read_iso_packet_descs(
    urb_ptr: usize,
    num_packets: usize,
) -> AxResult<Vec<descriptor::UsbdevfsIsoPacketDesc>> {
    let ptr = iso_packet_descs_ptr(urb_ptr)? as *const descriptor::UsbdevfsIsoPacketDesc;
    let mut descs = Vec::with_capacity(num_packets);
    for index in 0..num_packets {
        descs.push(unsafe { ptr.add(index) }.vm_read()?);
    }
    Ok(descs)
}

fn write_iso_packet_descs(
    urb_ptr: usize,
    descs: &[descriptor::UsbdevfsIsoPacketDesc],
) -> AxResult<()> {
    let ptr = iso_packet_descs_ptr(urb_ptr)?;
    if !descs.is_empty() {
        vm_write_slice(ptr, descs)?;
    }
    Ok(())
}
