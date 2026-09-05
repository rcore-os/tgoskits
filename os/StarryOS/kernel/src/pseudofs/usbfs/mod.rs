mod descriptor;
mod irq;
mod manager;
mod sysfs;
mod tree;

use alloc::{
    borrow::ToOwned,
    collections::{BTreeSet, VecDeque},
    sync::Arc,
    vec::Vec,
};
use core::{
    any::Any,
    future::{Future, poll_fn},
    mem::size_of,
    pin::pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll},
    time::Duration,
};

use axfs_ng_vfs::Filesystem;
use axpoll::{IoEvents, PollSet, Pollable};
use crab_usb::usb_if::endpoint::{TransferCompletion, TransferRequest};
use event_listener::Event as NotifyEvent;
use starry_vm::{VmMutPtr, VmPtr, vm_load, vm_write_slice};

use self::{irq::manager, manager::UsbFsManager, tree::UsbRootDir};
use crate::{
    Errno, StarryError, StarryResult,
    file::{File as KernelFile, FileLike, IoDst, IoSrc, Kstat},
    pseudofs::{SimpleDir, SimpleFs},
    sync::{IrqMutex as Mutex, Mutex as BlockingMutex},
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

pub(crate) fn new_usbfs() -> StarryResult<Option<Filesystem>> {
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
    ax_task::spawn_with_name(
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
    pub(crate) fn claim_interface(&self, interface: u8, alternate: u8) -> StarryResult<()> {
        self.lease.claim_interface(interface, alternate)
    }

    pub(crate) fn release_interface(&self, interface: u8) -> StarryResult<()> {
        self.lease.release_interface(interface)
    }

    pub(crate) fn control_transfer(
        &self,
        b_request_type: u8,
        b_request: u8,
        w_value: u16,
        w_index: u16,
        data: &mut [u8],
    ) -> StarryResult<usize> {
        self.lease
            .control_transfer(b_request_type, b_request, w_value, w_index, data)
    }

    pub(crate) fn bulk_in(&self, endpoint: u8, data: &mut [u8]) -> StarryResult<usize> {
        self.lease.bulk_in(endpoint, data)
    }

    pub(crate) fn bulk_out(&self, endpoint: u8, data: &[u8]) -> StarryResult<usize> {
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

pub(crate) fn acquire_usb_device(bus_num: u8, device_num: u8) -> StarryResult<UsbDeviceHandle> {
    let manager = manager().ok_or(StarryError::NoSuchDevice)?;
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
) -> StarryResult<Arc<dyn FileLike>> {
    let ops = inner
        .downcast_ref::<tree::UsbDeviceOps>()
        .ok_or(crate::StarryError::InvalidInput)?;
    let manager = manager().ok_or(crate::StarryError::NoSuchDevice)?;
    let snapshot = manager
        .device_snapshot(ops.bus_num, ops.device_num)
        .ok_or(crate::StarryError::NoSuchDevice)?;
    Ok(Arc::new(UsbDeviceFile {
        base: KernelFile::new(file, open_flags),
        manager,
        bus_num: ops.bus_num,
        device_num: ops.device_num,
        snapshot,
        lease: BlockingMutex::new(None),
        lifecycle_lock: BlockingMutex::new(()),
        claimed_interfaces: Mutex::new(Default::default()),
        submitted_urbs: Arc::new(BlockingMutex::new(VecDeque::new())),
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
    lease: BlockingMutex<Option<Arc<manager::UsbDeviceLease>>>,
    lifecycle_lock: BlockingMutex<()>,
    claimed_interfaces: Mutex<alloc::collections::BTreeMap<u8, u8>>,
    submitted_urbs: Arc<BlockingMutex<VecDeque<SubmittedUrb>>>,
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
    discarded: bool,
    buffer: Vec<u8>,
    is_in: bool,
    data_offset: usize,
    packet_lengths: Vec<usize>,
    log: bool,
}

enum SubmittedUrbTransfer {
    Live(manager::SubmittedTransfer),
    #[cfg(all(test, not(axtest)))]
    Test(tests::TestSubmittedTransfer),
}

impl SubmittedUrb {
    fn queue_key(&self) -> Option<manager::SubmittedTransferQueue> {
        match &self.transfer {
            SubmittedUrbTransfer::Live(transfer) => Some(transfer.queue_key()),
            #[cfg(all(test, not(axtest)))]
            SubmittedUrbTransfer::Test(_) => None,
        }
    }

    fn try_reclaim(&self) -> StarryResult<Option<TransferCompletion>> {
        match &self.transfer {
            SubmittedUrbTransfer::Live(transfer) => transfer.try_reclaim(),
            #[cfg(all(test, not(axtest)))]
            SubmittedUrbTransfer::Test(transfer) => transfer.try_reclaim(),
        }
    }

    fn poll_reclaim(&self, cx: &mut Context<'_>) -> Poll<StarryResult<TransferCompletion>> {
        match &self.transfer {
            SubmittedUrbTransfer::Live(transfer) => transfer.poll_reclaim(cx),
            #[cfg(all(test, not(axtest)))]
            SubmittedUrbTransfer::Test(_) => Poll::Pending,
        }
    }

    fn cancel(&self) -> StarryResult<()> {
        match &self.transfer {
            SubmittedUrbTransfer::Live(transfer) => transfer.cancel(),
            #[cfg(all(test, not(axtest)))]
            SubmittedUrbTransfer::Test(_) => Ok(()),
        }
    }
}

struct CompletedUrb {
    user_urb_ptr: usize,
    result: StarryResult<UrbTransferResult>,
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
    fn live_lease(&self) -> StarryResult<Arc<manager::UsbDeviceLease>> {
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
        f: impl FnOnce(&manager::UsbDeviceLease) -> StarryResult<R>,
    ) -> StarryResult<R> {
        let lease = self.live_lease()?;
        f(&lease)
    }

    fn claim_interface(
        &self,
        interface: u8,
        alternate: u8,
        force_reconfigure: bool,
    ) -> StarryResult<usize> {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        if !snapshot_has_interface(&self.snapshot, interface, alternate) {
            return Err(StarryError::NotFound);
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

        let submitted = self.drain_submitted_urbs_for_interface(interface);
        if let Err(err) = self.with_live_lease(|lease| lease.claim_interface(interface, alternate))
        {
            self.submitted_urbs.lock().extend(submitted);
            return Err(err);
        }
        let remaining = reclaim_quiesced_urbs(submitted);
        if !remaining.is_empty() {
            self.submitted_urbs.lock().extend(remaining);
            return Err(StarryError::ResourceBusy);
        }
        self.claimed_interfaces.lock().insert(interface, alternate);
        Ok(0)
    }

    fn release_interface(&self, interface: u8) -> StarryResult<usize> {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        self.claimed_interfaces
            .lock()
            .get(&interface)
            .copied()
            .ok_or(StarryError::InvalidInput)?;
        let submitted = self.drain_submitted_urbs_for_interface(interface);
        if let Some(lease) = self.lease.lock().as_ref().cloned() {
            if let Err(err) = lease.release_interface(interface) {
                self.submitted_urbs.lock().extend(submitted);
                return Err(err);
            }
            let remaining = reclaim_quiesced_urbs(submitted);
            if !remaining.is_empty() {
                self.submitted_urbs.lock().extend(remaining);
                return Err(StarryError::ResourceBusy);
            }
        } else {
            let remaining = cleanup_submitted_urbs(submitted, Some(USBFS_URB_CANCEL_TIMEOUT));
            if !remaining.is_empty() {
                self.submitted_urbs.lock().extend(remaining);
                return Err(StarryError::ResourceBusy);
            }
        }
        self.claimed_interfaces.lock().remove(&interface);
        Ok(0)
    }

    fn set_configuration_ioctl(&self, arg: usize) -> StarryResult<usize> {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        let configuration = descriptor::read_usbdevfs_u32(arg)?;
        if configuration > u8::MAX as u32 {
            return Err(StarryError::InvalidInput);
        }
        self.collect_submitted_urbs(None);
        if !self.claimed_interfaces.lock().is_empty()
            || !self.submitted_urbs.lock().is_empty()
            || !self.pending_urbs.lock().is_empty()
        {
            return Err(StarryError::ResourceBusy);
        }
        self.with_live_lease(|lease| lease.set_configuration(configuration as u8))?;
        Ok(0)
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

    fn drain_submitted_urb_by_ptr(&self, user_urb_ptr: usize) -> StarryResult<SubmittedUrb> {
        let mut submitted_urbs = self.submitted_urbs.lock();
        let index = submitted_urbs
            .iter()
            .position(|submitted| !submitted.discarded && submitted.user_urb_ptr == user_urb_ptr)
            .ok_or(StarryError::InvalidInput)?;
        submitted_urbs
            .remove(index)
            .ok_or(StarryError::InvalidInput)
    }

    fn get_driver_ioctl(&self, arg: usize) -> StarryResult<usize> {
        let mut get_driver = (arg as *const descriptor::UsbdevfsGetDriver).vm_read()?;
        if get_driver.interface > u8::MAX as u32 {
            return Err(StarryError::InvalidInput);
        }

        get_driver.driver.fill(0);
        get_driver.driver[..5].copy_from_slice(b"usbfs");
        (arg as *mut descriptor::UsbdevfsGetDriver).vm_write(get_driver)?;
        Ok(0)
    }

    fn kernel_driver_ioctl(&self, arg: usize) -> StarryResult<usize> {
        let command = descriptor::read_usbdevfs_ioctl(arg)?;
        if command.ifno < 0 || command.ifno > u8::MAX as i32 {
            return Err(StarryError::InvalidInput);
        }
        match command.ioctl_code as u32 {
            descriptor::USBDEVFS_DISCONNECT | descriptor::USBDEVFS_CONNECT => Ok(0),
            _ => Err(StarryError::Unsupported),
        }
    }

    fn disconnect_claim_ioctl(&self, arg: usize) -> StarryResult<usize> {
        let claim = descriptor::read_usbdevfs_disconnect_claim(arg)?;
        if claim.interface > u8::MAX as u32 {
            return Err(StarryError::InvalidInput);
        }
        self.claim_interface(claim.interface as u8, 0, false)
    }

    fn claimed_endpoint(&self, endpoint: u8) -> StarryResult<ClaimedEndpoint> {
        let claimed = self.claimed_interfaces.lock();
        snapshot_claimed_endpoint(&self.snapshot, endpoint, &claimed)
            .ok_or(StarryError::OperationNotPermitted)
    }

    fn run_endpoint_transfer(
        &self,
        endpoint: u8,
        transfer_type: EndpointTransferType,
        data: *mut u8,
        len: usize,
        iso_packet_lengths: &[usize],
    ) -> StarryResult<usize> {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        let claimed_endpoint = self.claimed_endpoint(endpoint)?;
        if claimed_endpoint.transfer_type != transfer_type {
            return Err(StarryError::InvalidInput);
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
                    return Err(StarryError::InvalidData);
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

    fn bulk_ioctl(&self, arg: usize) -> StarryResult<usize> {
        let bulk = descriptor::read_usbdevfs_bulktransfer(arg)?;
        if bulk.ep > u8::MAX as u32 {
            return Err(StarryError::InvalidInput);
        }
        self.run_endpoint_transfer(
            bulk.ep as u8,
            EndpointTransferType::Bulk,
            bulk.data,
            bulk.len as usize,
            &[],
        )
    }

    fn read_iso_packet_lengths(
        &self,
        urb_ptr: usize,
        num_packets: usize,
    ) -> StarryResult<Vec<usize>> {
        let packet_descs = read_iso_packet_descs(urb_ptr, num_packets)?;
        let mut total_length = 0usize;
        let mut packet_lengths = Vec::with_capacity(num_packets);
        for packet_desc in &packet_descs {
            let packet_length = packet_desc.length as usize;
            total_length = total_length
                .checked_add(packet_length)
                .ok_or(StarryError::OutOfRange)?;
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
    ) -> StarryResult<()> {
        let mut packet_descs = read_iso_packet_descs(urb_ptr, packet_lengths.len())?;
        if !packet_actual_lengths.is_empty() {
            if packet_actual_lengths.len() != packet_lengths.len() {
                return Err(StarryError::InvalidData);
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

    fn write_completed_urb(&self, completed: CompletedUrb) -> StarryResult<()> {
        let mut urb = (completed.user_urb_ptr as *const descriptor::UsbdevfsUrb).vm_read()?;
        let buffer = urb.buffer;
        let buffer_length = urb.buffer_length;

        match completed.result {
            Ok(result) => {
                if !result.data.is_empty() {
                    let user_len = buffer_length.max(0) as usize;
                    if result.data_offset > user_len {
                        return Err(StarryError::InvalidInput);
                    }
                    let copy_len = result.data.len().min(user_len - result.data_offset);
                    let buffer_ptr = (buffer as usize)
                        .checked_add(result.data_offset)
                        .ok_or(StarryError::InvalidInput)?
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
                let linux_error = err.linux_errno();
                let status = -linux_error.into_raw();
                urb.status = status;
                urb.actual_length = 0;
                urb.error_count = 1;
                (completed.user_urb_ptr as *mut descriptor::UsbdevfsUrb).vm_write(urb)?;
                if completed.log {
                    if matches!(
                        linux_error,
                        Errno::ECONNRESET | Errno::EINTR | Errno::ENOENT
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
        result: StarryResult<TransferCompletion>,
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
        if let Some(completed) = terminal_completed_urb(submitted, result) {
            complete_urb(&self.pending_urbs, &self.poll_urbs, completed);
        }
    }

    fn collect_submitted_urbs(&self, mut cx: Option<&mut Context<'_>>) -> bool {
        let mut ready = Vec::new();
        {
            let mut submitted_urbs = self.submitted_urbs.lock();
            let mut blocked_queues = BTreeSet::new();
            let mut index = 0;
            while index < submitted_urbs.len() {
                let queue_key = submitted_urbs[index].queue_key();
                if queue_key.is_some_and(|key| blocked_queues.contains(&key)) {
                    index += 1;
                    continue;
                }
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
                    if let Some(queue_key) = queue_key {
                        blocked_queues.insert(queue_key);
                    }
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
        ax_task::spawn_with_name(
            move || {
                ax_task::future::block_on(async {
                    loop {
                        let mut ready = Vec::new();
                        {
                            let mut submitted = submitted_urbs.lock();
                            let mut blocked_queues = BTreeSet::new();
                            let mut index = 0;
                            while index < submitted.len() {
                                let queue_key = submitted[index].queue_key();
                                if queue_key.is_some_and(|key| blocked_queues.contains(&key)) {
                                    index += 1;
                                    continue;
                                }
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
                                    if let Some(queue_key) = queue_key {
                                        blocked_queues.insert(queue_key);
                                    }
                                    index += 1;
                                }
                            }
                        }

                        for (submitted, result) in ready {
                            if let Some(completed) = terminal_completed_urb(submitted, result) {
                                complete_urb(&pending_urbs, &poll_urbs, completed);
                            }
                        }

                        if worker.closed.load(Ordering::Acquire) {
                            break;
                        }
                        let activity_seq = manager.usb_activity_seq();
                        let wake_listener = worker.wake_event.listen();
                        let activity_listener = manager.listen_usb_activity();
                        let mut wake_listener = pin!(wake_listener);
                        let mut activity_listener = pin!(activity_listener);
                        if submitted_urbs.lock().is_empty() {
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
                            let mut blocked_queues = BTreeSet::new();
                            let mut index = 0;
                            while index < submitted.len() {
                                let queue_key = submitted[index]
                                    .queue_key()
                                    .expect("submitted URB has no transfer queue");
                                if blocked_queues.contains(&queue_key) {
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
                                    Poll::Pending => {
                                        blocked_queues.insert(queue_key);
                                        index += 1;
                                    }
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
                            if submitted.discarded {
                                continue;
                            }
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
                            ax_task::yield_now();
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
    ) -> StarryResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read()?;
        let (urb_type, endpoint, buffer, buffer_length) =
            (urb.type_, urb.endpoint, urb.buffer, urb.buffer_length);
        if urb_type != expected_urb_type {
            return Err(crate::StarryError::Unsupported);
        }
        if buffer_length < 0 || total_length > buffer_length as usize {
            return Err(crate::StarryError::InvalidInput);
        }

        let claimed_endpoint = self.claimed_endpoint(endpoint)?;
        if claimed_endpoint.transfer_type != transfer_type {
            return Err(StarryError::InvalidInput);
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
        if matches!(&transfer, Err(StarryError::ResourceBusy)) {
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
            discarded: false,
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

    fn submit_control_urb(&self, arg: usize) -> StarryResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read()?;
        let (urb_type, urb_buffer, buffer_length) = (urb.type_, urb.buffer, urb.buffer_length);
        if urb_type != descriptor::USBDEVFS_URB_TYPE_CONTROL {
            return Err(crate::StarryError::Unsupported);
        }
        if buffer_length < 8 {
            return Err(crate::StarryError::InvalidInput);
        }

        let mut setup_bytes = [0u8; 8];
        read_user_bytes_into(urb_buffer as *const u8, &mut setup_bytes)?;
        let b_request_type = setup_bytes[0];
        let b_request = setup_bytes[1];
        let w_value = u16::from_le_bytes([setup_bytes[2], setup_bytes[3]]);
        let w_index = u16::from_le_bytes([setup_bytes[4], setup_bytes[5]]);
        let w_length = u16::from_le_bytes([setup_bytes[6], setup_bytes[7]]) as usize;
        if (buffer_length as usize) < 8 + w_length {
            return Err(crate::StarryError::InvalidInput);
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
                .ok_or(StarryError::InvalidInput)? as *const u8;
            read_user_bytes(data_ptr, w_length)?
        };
        let request = match is_in {
            true => TransferRequest::control_in(setup, &mut buffer),
            false => TransferRequest::control_out(setup, &buffer),
        };

        self.collect_submitted_urbs(None);
        let mut transfer = self.with_live_lease(|lease| lease.submit_control_transfer(request));
        if matches!(&transfer, Err(StarryError::ResourceBusy)) {
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
            discarded: false,
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

    fn submit_bulk_urb(&self, arg: usize) -> StarryResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read()?;
        if urb.type_ != descriptor::USBDEVFS_URB_TYPE_BULK {
            return Err(crate::StarryError::Unsupported);
        }
        if urb.buffer_length < 0 {
            return Err(crate::StarryError::InvalidInput);
        }

        self.submit_endpoint_urb_async(
            arg,
            descriptor::USBDEVFS_URB_TYPE_BULK,
            EndpointTransferType::Bulk,
            Vec::new(),
            urb.buffer_length as usize,
        )
    }

    fn submit_interrupt_urb(&self, arg: usize) -> StarryResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read()?;
        if urb.type_ != descriptor::USBDEVFS_URB_TYPE_INTERRUPT {
            return Err(crate::StarryError::Unsupported);
        }
        if urb.buffer_length < 0 {
            return Err(crate::StarryError::InvalidInput);
        }
        self.submit_endpoint_urb_async(
            arg,
            descriptor::USBDEVFS_URB_TYPE_INTERRUPT,
            EndpointTransferType::Interrupt,
            Vec::new(),
            urb.buffer_length as usize,
        )
    }

    fn submit_iso_urb(&self, arg: usize) -> StarryResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read()?;
        if urb.type_ != descriptor::USBDEVFS_URB_TYPE_ISO {
            return Err(crate::StarryError::Unsupported);
        }
        if urb.buffer_length < 0 || urb.number_of_packets <= 0 {
            return Err(crate::StarryError::InvalidInput);
        }
        let supported_flags =
            descriptor::USBDEVFS_URB_ISO_ASAP | descriptor::USBDEVFS_URB_SHORT_NOT_OK;
        if urb.flags & !supported_flags != 0 {
            return Err(StarryError::Unsupported);
        }
        if urb.flags & descriptor::USBDEVFS_URB_ISO_ASAP == 0 && urb.start_frame != 0 {
            return Err(StarryError::Unsupported);
        }

        let packet_lengths = self.read_iso_packet_lengths(arg, urb.number_of_packets as usize)?;
        let total_length = packet_lengths.iter().try_fold(0usize, |acc, len| {
            acc.checked_add(*len).ok_or(StarryError::OutOfRange)
        })?;
        if total_length > urb.buffer_length as usize {
            return Err(StarryError::InvalidInput);
        }

        self.submit_endpoint_urb_async(
            arg,
            descriptor::USBDEVFS_URB_TYPE_ISO,
            EndpointTransferType::Isochronous,
            packet_lengths,
            total_length,
        )
    }

    fn submit_urb(&self, arg: usize) -> StarryResult<usize> {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        self.collect_submitted_urbs(None);
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read()?;
        let type_ = urb.type_;
        match type_ {
            descriptor::USBDEVFS_URB_TYPE_CONTROL => self.submit_control_urb(arg),
            descriptor::USBDEVFS_URB_TYPE_BULK => self.submit_bulk_urb(arg),
            descriptor::USBDEVFS_URB_TYPE_INTERRUPT => self.submit_interrupt_urb(arg),
            descriptor::USBDEVFS_URB_TYPE_ISO => self.submit_iso_urb(arg),
            _ => Err(crate::StarryError::Unsupported),
        }
    }

    fn reap_urb(&self, arg: usize, nonblocking: bool) -> StarryResult<usize> {
        self.collect_submitted_urbs(None);
        if !nonblocking && self.pending_urbs.lock().is_empty() {
            ax_task::future::block_on(poll_fn(|cx| {
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
            return Err(crate::StarryError::WouldBlock);
        };
        let user_urb_ptr = completed.user_urb_ptr;
        self.write_completed_urb(completed)?;
        (arg as *mut usize).vm_write(user_urb_ptr)?;
        if usbfs_should_log_urb() {
            debug!("usbfs: reap urb returns ptr={:#x}", user_urb_ptr);
        }
        Ok(0)
    }

    fn discard_urb(&self, arg: usize) -> StarryResult<usize> {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        let mut submitted = self.drain_submitted_urb_by_ptr(arg)?;
        submitted.cancel()?;
        submitted.discarded = true;

        complete_urb(
            &self.pending_urbs,
            &self.poll_urbs,
            CompletedUrb {
                user_urb_ptr: submitted.user_urb_ptr,
                result: Err(StarryError::from(Errno::ENOENT)),
                log: submitted.log,
            },
        );

        self.submitted_urbs.lock().push_back(submitted);
        self.ensure_urb_worker();
        Ok(0)
    }
}

impl FileLike for UsbDeviceFile {
    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        self.base.read(dst)
    }

    fn write(&self, src: &mut IoSrc) -> StarryResult<usize> {
        self.base.write(src)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        self.base.stat()
    }

    fn path(&self) -> alloc::borrow::Cow<'_, str> {
        self.base.path()
    }

    fn file_mmap(&self) -> StarryResult<(ax_fs_ng::vfs::FileBackend, ax_fs_ng::vfs::FileFlags)> {
        self.base.file_mmap()
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> StarryResult<usize> {
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
                    return Err(StarryError::InvalidInput);
                }
                self.claim_interface(interface as u8, 0, false)
            }
            descriptor::USBDEVFS_RELEASEINTERFACE => {
                let interface = descriptor::read_usbdevfs_u32(arg)?;
                if interface > u8::MAX as u32 {
                    return Err(StarryError::InvalidInput);
                }
                self.release_interface(interface as u8)
            }
            descriptor::USBDEVFS_GETDRIVER => self.get_driver_ioctl(arg),
            descriptor::USBDEVFS_SETINTERFACE => {
                let set = descriptor::read_usbdevfs_setinterface(arg)?;
                if set.interface > u8::MAX as u32 || set.altsetting > u8::MAX as u32 {
                    return Err(StarryError::InvalidInput);
                }
                self.claim_interface(set.interface as u8, set.altsetting as u8, true)
            }
            descriptor::USBDEVFS_SETCONFIGURATION => self.set_configuration_ioctl(arg),
            descriptor::USBDEVFS_CLEAR_HALT => {
                let endpoint = descriptor::read_usbdevfs_u32(arg)?;
                if endpoint > u8::MAX as u32 {
                    return Err(StarryError::InvalidInput);
                }
                self.with_live_lease(|lease| lease.clear_halt(endpoint as u8))?;
                Ok(0)
            }
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

    fn set_nonblocking(&self, flag: bool) -> StarryResult {
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
        let mut submitted = self.drain_all_submitted_urbs();
        if let Some(lease) = lease.as_ref() {
            let interfaces = self
                .claimed_interfaces
                .lock()
                .keys()
                .copied()
                .collect::<Vec<_>>();
            for interface in interfaces {
                let mut interface_urbs = Vec::new();
                let mut index = 0;
                while index < submitted.len() {
                    if submitted[index].interface == Some(interface) {
                        interface_urbs.push(submitted.swap_remove(index));
                    } else {
                        index += 1;
                    }
                }
                if lease.release_interface(interface).is_ok() {
                    submitted.extend(reclaim_quiesced_urbs(interface_urbs));
                } else {
                    submitted.extend(interface_urbs);
                }
            }
        }
        self.pending_urbs.lock().clear();
        if submitted.is_empty() {
            drop(lease);
            return;
        }

        ax_task::spawn_with_name(
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
    result: StarryResult<TransferCompletion>,
) -> CompletedUrb {
    CompletedUrb {
        user_urb_ptr,
        result: result
            .map(|completion| UsbDeviceFile::transfer_completion_to_result(submitted, completion)),
        log,
    }
}

fn terminal_completed_urb(
    submitted: SubmittedUrb,
    result: StarryResult<TransferCompletion>,
) -> Option<CompletedUrb> {
    if submitted.discarded {
        return None;
    }
    Some(completed_urb_from_result(
        submitted.user_urb_ptr,
        submitted.log,
        submitted,
        result,
    ))
}

fn cleanup_submitted_urbs(
    mut submitted_urbs: Vec<SubmittedUrb>,
    timeout: Option<Duration>,
) -> Vec<SubmittedUrb> {
    let deadline = timeout.map(|timeout| ax_runtime::hal::time::monotonic_time() + timeout);
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
            if deadline.is_some_and(|deadline| ax_runtime::hal::time::monotonic_time() >= deadline)
            {
                break;
            }
            ax_task::sleep(Duration::from_millis(1));
        }
    }

    submitted_urbs
}

fn reclaim_quiesced_urbs(submitted_urbs: Vec<SubmittedUrb>) -> Vec<SubmittedUrb> {
    let mut remaining = Vec::new();
    for submitted in submitted_urbs {
        match submitted.try_reclaim() {
            Ok(None) => remaining.push(submitted),
            Ok(Some(_)) | Err(_) => continue,
        }
    }
    remaining
}

fn usbfs_should_log_urb() -> bool {
    USBFS_URB_LOG_BUDGET
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |budget| {
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

fn iso_packet_descs_ptr(urb_ptr: usize) -> StarryResult<*mut descriptor::UsbdevfsIsoPacketDesc> {
    urb_ptr
        .checked_add(size_of::<descriptor::UsbdevfsUrb>())
        .map(|offset| offset as *mut descriptor::UsbdevfsIsoPacketDesc)
        .ok_or(StarryError::OutOfRange)
}

fn read_user_bytes(ptr: *const u8, len: usize) -> StarryResult<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    Ok(vm_load(ptr, len)?)
}

fn read_user_bytes_into(ptr: *const u8, dst: &mut [u8]) -> StarryResult<()> {
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
) -> StarryResult<Vec<descriptor::UsbdevfsIsoPacketDesc>> {
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
) -> StarryResult<()> {
    let ptr = iso_packet_descs_ptr(urb_ptr)?;
    if !descs.is_empty() {
        vm_write_slice(ptr, descs)?;
    }
    Ok(())
}

#[cfg(all(test, not(axtest)))]
mod tests {
    extern crate std;

    use alloc::{sync::Arc, vec};

    use crab_usb::usb_if::endpoint::{RequestId, TransferStatus};

    use self::std::sync::Mutex as TestMutex;
    use super::*;

    struct TestTransferState {
        inflight_requests: usize,
        completion_pending: bool,
        completion_reclaims: usize,
    }

    struct TestUsbfsAdapter(Arc<TestMutex<TestTransferState>>);

    impl TestUsbfsAdapter {
        fn new() -> Self {
            Self(Arc::new(TestMutex::new(TestTransferState {
                inflight_requests: 0,
                completion_pending: false,
                completion_reclaims: 0,
            })))
        }

        fn submit_async_urb(&self, interface: u8) -> SubmittedUrb {
            self.0.lock().unwrap().inflight_requests += 1;
            SubmittedUrb {
                user_urb_ptr: 1,
                transfer: SubmittedUrbTransfer::Test(TestSubmittedTransfer(self.0.clone())),
                interface: Some(interface),
                discarded: false,
                buffer: Vec::new(),
                is_in: false,
                data_offset: 0,
                packet_lengths: Vec::new(),
                log: false,
            }
        }

        fn publish_terminal_completion(&self) {
            let mut state = self.0.lock().unwrap();
            assert_eq!(state.inflight_requests, 1);
            assert!(!state.completion_pending);
            state.completion_pending = true;
        }
    }

    pub(super) struct TestSubmittedTransfer(Arc<TestMutex<TestTransferState>>);

    impl TestSubmittedTransfer {
        pub(super) fn try_reclaim(&self) -> StarryResult<Option<TransferCompletion>> {
            let mut state = self.0.lock().unwrap();
            if !state.completion_pending {
                return Ok(None);
            }
            state.completion_pending = false;
            state.inflight_requests -= 1;
            state.completion_reclaims += 1;
            Ok(Some(TransferCompletion {
                request_id: RequestId::new(1),
                status: TransferStatus::Completed,
                actual_length: 0,
                iso_packets: Vec::new(),
            }))
        }
    }

    #[test]
    fn quiesced_urb_is_removed_only_after_terminal_completion() {
        const INTERFACE: u8 = 1;
        let adapter = TestUsbfsAdapter::new();
        let remaining = reclaim_quiesced_urbs(vec![adapter.submit_async_urb(INTERFACE)]);
        assert_eq!(remaining.len(), 1);

        adapter.publish_terminal_completion();
        assert!(reclaim_quiesced_urbs(remaining).is_empty());
        let state = adapter.0.lock().unwrap();
        assert_eq!(state.inflight_requests, 0);
        assert!(!state.completion_pending);
        assert_eq!(state.completion_reclaims, 1);
    }

    #[test]
    fn discard_reports_enoent_once_then_reclaims_terminal_transfer() {
        let adapter = TestUsbfsAdapter::new();
        let mut submitted = adapter.submit_async_urb(1);
        submitted.cancel().unwrap();
        submitted.discarded = true;

        let mut pending = VecDeque::from([CompletedUrb {
            user_urb_ptr: submitted.user_urb_ptr,
            result: Err(StarryError::from(Errno::ENOENT)),
            log: false,
        }]);
        let discarded = pending
            .pop_front()
            .expect("DISCARDURB must immediately publish one completion");
        assert_eq!(discarded.user_urb_ptr, 1);
        match discarded.result {
            Err(err) => assert_eq!(err.linux_errno(), Errno::ENOENT),
            Ok(_) => panic!("DISCARDURB must report ENOENT"),
        }

        adapter.publish_terminal_completion();
        let terminal = submitted
            .try_reclaim()
            .unwrap()
            .expect("the HCD terminal completion must reclaim the transfer");
        assert!(terminal_completed_urb(submitted, Ok(terminal)).is_none());
        assert!(pending.pop_front().is_none());

        let state = adapter.0.lock().unwrap();
        assert_eq!(state.inflight_requests, 0);
        assert_eq!(state.completion_reclaims, 1);
    }
}
