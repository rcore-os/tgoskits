mod descriptor;
mod irq;
mod manager;
mod refresh;
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
    cell::Cell,
    future::poll_fn,
    mem::size_of,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll},
    time::Duration,
};

use ax_std::os::arceos::task::{executor::LocalExecutor, thread::current::current_thread_handle};
use axfs_ng_vfs::Filesystem;
use axpoll::{ExclusiveRegistrationSink, IoEvents, Pollable, SharedRegistrationSink};
use axpoll_set::PollSet;
use crab_usb::usb_if::endpoint::{TransferCompletion, TransferRequest};
#[cfg(feature = "uvc")]
pub(crate) use manager::{SubmittedTransfer, SubmittedTransferInner};

use self::{irq::manager, manager::UsbFsManager, tree::UsbRootDir};
use crate::{
    Errno, StarryError, StarryResult,
    file::{File as KernelFile, FileLike, IoDst, IoSrc, Kstat},
    mm::{VmMutPtr, VmPtr, vm_load, vm_write_slice},
    pseudofs::{SimpleDir, SimpleFs},
    sync::{IrqMutex, Mutex},
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
    // The fixed event worker must exist before any framework action is armed.
    // Controller initialization may await command completions delivered by it.
    irq::start_event_pump();

    let init_result = Arc::new(IrqMutex::new(None));
    let worker_result = init_result.clone();
    let worker_manager = manager.clone();
    let init_worker = crate::task::kernel_thread_builder("usbfs-init".to_owned())
        .spawn(move || {
            let report = manager::initialize_hosts(&worker_manager);
            *worker_result.lock() = Some(report);
        })
        .expect("failed to spawn kernel thread");
    let _exit_code = init_worker.join().expect("failed to join kernel thread");
    let report = init_result
        .lock()
        .take()
        .expect("joined USB initialization worker must publish a report");
    for failure in &report.failures {
        warn!(
            "usbfs: host {:?} on bus {} failed during {:?}",
            failure.device_id, failure.bus_num, failure.stage
        );
    }

    let initialized_hosts = report.initialized > 0;
    if !initialized_hosts {
        info!("usbfs: no USB host initialized, skip mounting usbfs");
        return Ok(None);
    }

    info!("usbfs: spawning refresh task");
    let refresh_manager = manager.clone();
    crate::task::kernel_thread_builder("usbfs-refresh".to_owned())
        .spawn(move || manager::usbfs_refresh_task(refresh_manager.clone()))
        .expect("failed to spawn kernel thread");

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
    #[cfg(feature = "uvc")]
    pub(crate) generation: u64,
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

    #[cfg(feature = "uvc")]
    pub(crate) fn submit_endpoint_transfer(
        &self,
        endpoint: u8,
        request: TransferRequest,
    ) -> StarryResult<SubmittedTransfer> {
        self.lease.submit_endpoint_transfer(endpoint, request)
    }
}

pub(crate) fn usb_device_snapshots() -> Vec<UsbDeviceSnapshotInfo> {
    let Some(manager) = manager() else {
        return Vec::new();
    };

    let mut snapshots = Vec::new();
    for bus_num in manager.bus_numbers() {
        for device_num in manager.device_numbers(bus_num) {
            let Some((snapshot, generation)) =
                manager.device_snapshot_with_generation(bus_num, device_num)
            else {
                continue;
            };
            #[cfg(not(feature = "uvc"))]
            let _ = generation;
            snapshots.push(UsbDeviceSnapshotInfo {
                bus_num,
                device_num,
                descriptor_blob: snapshot.descriptor_blob,
                #[cfg(feature = "uvc")]
                generation,
            });
        }
    }
    snapshots
}

pub(crate) fn acquire_usb_device(bus_num: u8, device_num: u8) -> StarryResult<UsbDeviceHandle> {
    let manager = manager().ok_or(StarryError::NoSuchDevice)?;
    manager
        .acquire_device(bus_num, device_num, Some("usb-serial"), None)
        .map(|lease| UsbDeviceHandle { lease })
}

#[cfg(feature = "uvc")]
pub(crate) fn acquire_usb_device_generation(
    bus_num: u8,
    device_num: u8,
    generation: u64,
) -> StarryResult<UsbDeviceHandle> {
    let manager = manager().ok_or(StarryError::NoSuchDevice)?;
    manager
        .acquire_device(bus_num, device_num, Some("uvcvideo"), Some(generation))
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
    let (snapshot, generation) = manager
        .device_snapshot_with_generation(ops.bus_num, ops.device_num)
        .ok_or(crate::StarryError::NoSuchDevice)?;
    if generation != ops.generation {
        return Err(StarryError::NoSuchDevice);
    }
    Ok(Arc::new(UsbDeviceFile {
        base: KernelFile::new(file, open_flags),
        manager,
        bus_num: ops.bus_num,
        device_num: ops.device_num,
        generation,
        snapshot,
        lease: Mutex::new(None),
        lifecycle_lock: Mutex::new(()),
        claimed_interfaces: IrqMutex::new(Default::default()),
        submitted_urbs: Arc::new(Mutex::new(VecDeque::new())),
        pending_urbs: Arc::new(IrqMutex::new(VecDeque::new())),
        poll_urbs: Arc::new(PollSet::new()),
        urb_worker: Arc::new(UrbWorker::new()),
    }))
}

static USBFS_URB_LOG_BUDGET: AtomicUsize = AtomicUsize::new(96);

struct UsbDeviceFile {
    base: KernelFile,
    manager: Arc<UsbFsManager>,
    bus_num: u8,
    device_num: u8,
    generation: u64,
    snapshot: descriptor::UsbDeviceSnapshot,
    lease: Mutex<Option<Arc<manager::UsbDeviceLease>>>,
    lifecycle_lock: Mutex<()>,
    claimed_interfaces: IrqMutex<alloc::collections::BTreeMap<u8, u8>>,
    submitted_urbs: Arc<Mutex<VecDeque<SubmittedUrb>>>,
    pending_urbs: Arc<IrqMutex<VecDeque<CompletedUrb>>>,
    poll_urbs: Arc<PollSet>,
    urb_worker: Arc<UrbWorker>,
}

struct UrbWorker {
    running: AtomicBool,
    closed: AtomicBool,
}

impl UrbWorker {
    fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    fn close(&self, manager: &UsbFsManager) {
        self.closed.store(true, Ordering::Release);
        manager.notify_urb_workers();
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
    fn ensure_current(&self) -> StarryResult<()> {
        if self
            .manager
            .device_snapshot_with_generation(self.bus_num, self.device_num)
            .is_some_and(|(_, generation)| generation == self.generation)
        {
            Ok(())
        } else {
            Err(StarryError::NoSuchDevice)
        }
    }

    fn live_lease(&self) -> StarryResult<Arc<manager::UsbDeviceLease>> {
        let mut lease = self.lease.lock();
        if let Some(lease) = lease.as_ref() {
            return Ok(lease.clone());
        }

        let new_lease =
            Arc::new(
                self.manager
                    .acquire_device(self.bus_num, self.device_num, None, Some(self.generation))?,
            );
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

    fn claim_control_recipient(&self, request_type: u8, index: u16) -> StarryResult<Option<u8>> {
        // Linux usbfs claims an interface on first use of an interface or
        // endpoint control request. Vendor requests are exempt.
        if request_type & 0x60 == 0x40 {
            return Ok(None);
        }
        let interface = match request_type & 0x1f {
            1 => Some(index as u8),
            2 if index as u8 & 0x0f != 0 => Some(
                snapshot_claimed_endpoint(
                    &self.snapshot,
                    index as u8,
                    &self.claimed_interfaces.lock(),
                )
                .map(|endpoint| endpoint.interface)
                .or_else(|| snapshot_endpoint_interface(&self.snapshot, index as u8))
                .ok_or(StarryError::NotFound)?,
            ),
            _ => None,
        };
        if let Some(interface) = interface {
            let already_claimed = self.claimed_interfaces.lock().contains_key(&interface);
            if !already_claimed {
                self.claim_interface(interface, 0, false)?;
            }
        }
        Ok(interface)
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
        if let Err(err) = self.with_live_lease(|lease| lease.claim_interface(interface, alternate)) {
            self.submitted_urbs.lock().extend(submitted);
            return Err(err);
        }
        self.claimed_interfaces.lock().insert(interface, alternate);
        // The alternate setting has changed and its old endpoints are quiesced.
        // Do not return while an old URB still owns a controller request.
        cleanup_submitted_urbs(submitted);
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
        if let Some(lease) = self.lease.lock().as_ref().cloned()
            && let Err(err) = lease.release_interface(interface)
        {
            self.submitted_urbs.lock().extend(submitted);
            return Err(err);
        }
        self.claimed_interfaces.lock().remove(&interface);
        // Release quiesces the endpoint. Keep this fd alive until every URB
        // reaches a terminal state so user buffers can be reused on return.
        cleanup_submitted_urbs(submitted);
        Ok(0)
    }

    fn set_configuration_ioctl(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> crate::StarryResult<usize> {
        let _lifecycle_guard = self.lifecycle_lock.lock();
        let configuration = descriptor::read_usbdevfs_u32(current, arg)?;
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

    fn get_driver_ioctl(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> crate::StarryResult<usize> {
        let mut get_driver = (arg as *const descriptor::UsbdevfsGetDriver).vm_read(current)?;
        if get_driver.interface > u8::MAX as u32 {
            return Err(StarryError::InvalidInput);
        }

        let name = self
            .manager
            .kernel_driver_name(
                self.bus_num,
                self.device_num,
                self.generation,
                get_driver.interface as u8,
            )
            .ok_or(StarryError::from(crate::Errno::ENODATA))?;
        get_driver.driver.fill(0);
        get_driver.driver[..name.len()].copy_from_slice(name.as_bytes());
        (arg as *mut descriptor::UsbdevfsGetDriver).vm_write(current, get_driver)?;
        Ok(0)
    }

    fn kernel_driver_ioctl(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> crate::StarryResult<usize> {
        let command = descriptor::read_usbdevfs_ioctl(current, arg)?;
        if command.ifno < 0 || command.ifno > u8::MAX as i32 {
            return Err(StarryError::InvalidInput);
        }
        match command.ioctl_code as u32 {
            descriptor::USBDEVFS_DISCONNECT => {
                if self
                    .manager
                    .kernel_driver_name(
                        self.bus_num,
                        self.device_num,
                        self.generation,
                        command.ifno as u8,
                    )
                    .is_some()
                {
                    Err(StarryError::ResourceBusy)
                } else {
                    Err(StarryError::from(crate::Errno::ENODATA))
                }
            }
            descriptor::USBDEVFS_CONNECT => Err(StarryError::Unsupported),
            _ => Err(StarryError::Unsupported),
        }
    }

    fn disconnect_claim_ioctl(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> crate::StarryResult<usize> {
        let claim = descriptor::read_usbdevfs_disconnect_claim(current, arg)?;
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

    fn claim_endpoint_if_needed(&self, endpoint: u8) -> StarryResult<()> {
        if self.claimed_endpoint(endpoint).is_ok() {
            return Ok(());
        }
        let interface = snapshot_endpoint_interface(&self.snapshot, endpoint)
            .ok_or(StarryError::NotFound)?;
        if !self.claimed_interfaces.lock().contains_key(&interface) {
            self.claim_interface(interface, 0, false)?;
        }
        Ok(())
    }

    fn run_endpoint_transfer(
        &self,
        current: &crate::task::UserTaskRef,
        endpoint: u8,
        transfer_type: EndpointTransferType,
        data: *mut u8,
        len: usize,
        iso_packet_lengths: &[usize],
    ) -> StarryResult<usize> {
        self.claim_endpoint_if_needed(endpoint)?;
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
                    vm_write_slice(current, data, &buffer[..actual])?;
                }
                Ok(actual)
            } else {
                let buffer = read_user_bytes(current, data as *const u8, len)?;
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

    fn bulk_ioctl(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> crate::StarryResult<usize> {
        let bulk = descriptor::read_usbdevfs_bulktransfer(current, arg)?;
        if bulk.ep > u8::MAX as u32 {
            return Err(StarryError::InvalidInput);
        }
        self.run_endpoint_transfer(
            current,
            bulk.ep as u8,
            EndpointTransferType::Bulk,
            bulk.data,
            bulk.len as usize,
            &[],
        )
    }

    fn read_iso_packet_lengths(
        &self,
        current: &crate::task::UserTaskRef,
        urb_ptr: usize,
        num_packets: usize,
    ) -> crate::StarryResult<Vec<usize>> {
        let packet_descs = read_iso_packet_descs(current, urb_ptr, num_packets)?;
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
        current: &crate::task::UserTaskRef,
        urb_ptr: usize,
        packet_lengths: &[usize],
        actual_total: usize,
        packet_actual_lengths: &[usize],
    ) -> crate::StarryResult<()> {
        let mut packet_descs = read_iso_packet_descs(current, urb_ptr, packet_lengths.len())?;
        if !packet_actual_lengths.is_empty() {
            if packet_actual_lengths.len() != packet_lengths.len() {
                return Err(StarryError::InvalidData);
            }
            for (packet_desc, packet_actual) in packet_descs.iter_mut().zip(packet_actual_lengths) {
                packet_desc.actual_length = (*packet_actual).min(u32::MAX as usize) as u32;
                packet_desc.status = 0;
            }
            return write_iso_packet_descs(current, urb_ptr, &packet_descs);
        }

        let mut remaining = actual_total;
        for (packet_desc, packet_length) in packet_descs.iter_mut().zip(packet_lengths.iter()) {
            let packet_actual = remaining.min(*packet_length);
            packet_desc.actual_length = packet_actual as u32;
            packet_desc.status = 0;
            remaining -= packet_actual;
        }
        write_iso_packet_descs(current, urb_ptr, &packet_descs)
    }

    fn write_completed_urb(
        &self,
        current: &crate::task::UserTaskRef,
        completed: CompletedUrb,
    ) -> crate::StarryResult<()> {
        let CompletedUrb {
            user_urb_ptr,
            result,
            log,
        } = completed;
        let mut urb = (user_urb_ptr as *const descriptor::UsbdevfsUrb).vm_read(current)?;
        let buffer = urb.buffer;
        let buffer_length = urb.buffer_length;

        match result {
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
                    vm_write_slice(current, buffer_ptr, &result.data[..copy_len])?;
                }
                if !result.packet_lengths.is_empty() {
                    self.write_iso_packet_results(
                        current,
                        user_urb_ptr,
                        &result.packet_lengths,
                        result.actual_length,
                        &result.packet_actual_lengths,
                    )?;
                }
                urb.status = 0;
                urb.actual_length = result.actual_length as i32;
                urb.error_count = 0;
                (user_urb_ptr as *mut descriptor::UsbdevfsUrb).vm_write(current, urb)?;
                if log {
                    debug!(
                        "usbfs: reap urb ptr={:#x} status=0 actual={} packets={}",
                        user_urb_ptr,
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
                (user_urb_ptr as *mut descriptor::UsbdevfsUrb).vm_write(current, urb)?;
                if log {
                    if matches!(
                        linux_error,
                        Errno::ECONNRESET | Errno::EINTR | Errno::ENOENT
                    ) {
                        debug!(
                            "usbfs: reap urb ptr={:#x} status={} err={:?}",
                            user_urb_ptr, status, err
                        );
                    } else {
                        warn!(
                            "usbfs: reap urb ptr={:#x} status={} err={:?}",
                            user_urb_ptr, status, err
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

    fn collect_submitted_urbs(&self, mut cx: Option<&mut Context<'_>>) {
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

        for (submitted, result) in ready {
            self.complete_submitted_urb(submitted, result);
        }
    }

    fn ensure_urb_worker(&self) {
        if !self.urb_worker.try_start() {
            self.manager.notify_urb_workers();
            return;
        }
        let submitted_urbs = self.submitted_urbs.clone();
        let pending_urbs = self.pending_urbs.clone();
        let poll_urbs = self.poll_urbs.clone();
        let worker = self.urb_worker.clone();
        let manager = self.manager.clone();
        crate::task::kernel_thread_builder("usbfs-urb-worker".to_owned())
            .spawn(move || {
                let current = current_thread_handle().expect("USB worker has no scheduler thread");
                let executor = LocalExecutor::new(current.wake_handle())
                    .expect("USB executor must belong to its worker");
                let observed = Cell::new(manager.usb_activity_seq());
                executor.run(
                    poll_fn(|cx| {
                        // Snapshot before inspecting transfers: a submit, close, or
                        // completion racing this poll must prevent the next park.
                        observed.set(manager.usb_activity_seq());
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
                                match submitted[index].poll_reclaim(cx) {
                                    Poll::Ready(result) => {
                                        ready.push((
                                            submitted
                                                .remove(index)
                                                .expect("submitted URB disappeared"),
                                            result,
                                        ));
                                    }
                                    Poll::Pending => {
                                        if let Some(queue_key) = queue_key {
                                            blocked_queues.insert(queue_key);
                                        }
                                        index += 1;
                                    }
                                }
                            }
                        }

                        // Completion callbacks and task wakes run outside the URB lock.
                        for (submitted, result) in ready {
                            if let Some(completed) = terminal_completed_urb(submitted, result) {
                                complete_urb(&pending_urbs, &poll_urbs, completed);
                            }
                        }
                        if worker.closed.load(Ordering::Acquire) {
                            Poll::Ready(())
                        } else {
                            Poll::Pending
                        }
                    }),
                    |condition| {
                        manager.wait_for_usb_activity(observed.get(), || condition.should_abort());
                    },
                );
                drop(executor);
                worker.stop();
            })
            .expect("failed to spawn kernel thread");
    }

    fn submit_endpoint_urb_async(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
        expected_urb_type: u8,
        transfer_type: EndpointTransferType,
        packet_lengths: Vec<usize>,
        total_length: usize,
    ) -> crate::StarryResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read(current)?;
        let (urb_type, endpoint, buffer, buffer_length) =
            (urb.type_, urb.endpoint, urb.buffer, urb.buffer_length);
        if urb_type != expected_urb_type {
            return Err(crate::StarryError::Unsupported);
        }
        if buffer_length < 0 || total_length > buffer_length as usize {
            return Err(crate::StarryError::InvalidInput);
        }

        self.claim_endpoint_if_needed(endpoint)?;
        let _lifecycle_guard = self.lifecycle_lock.lock();
        let claimed_endpoint = self.claimed_endpoint(endpoint)?;
        if claimed_endpoint.transfer_type != transfer_type {
            return Err(StarryError::InvalidInput);
        }

        let is_in = endpoint & 0x80 != 0;
        let mut buffer = if is_in {
            alloc::vec![0; total_length]
        } else {
            read_user_bytes(current, buffer as *const u8, total_length)?
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

    fn submit_control_urb(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> crate::StarryResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read(current)?;
        let (urb_type, urb_buffer, buffer_length) = (urb.type_, urb.buffer, urb.buffer_length);
        if urb_type != descriptor::USBDEVFS_URB_TYPE_CONTROL {
            return Err(crate::StarryError::Unsupported);
        }
        if buffer_length < 8 {
            return Err(crate::StarryError::InvalidInput);
        }

        let mut setup_bytes = [0u8; 8];
        read_user_bytes_into(current, urb_buffer as *const u8, &mut setup_bytes)?;
        let b_request_type = setup_bytes[0];
        let b_request = setup_bytes[1];
        let w_value = u16::from_le_bytes([setup_bytes[2], setup_bytes[3]]);
        let w_index = u16::from_le_bytes([setup_bytes[4], setup_bytes[5]]);
        let w_length = u16::from_le_bytes([setup_bytes[6], setup_bytes[7]]) as usize;
        if (buffer_length as usize) < 8 + w_length {
            return Err(crate::StarryError::InvalidInput);
        }
        let interface = self.claim_control_recipient(b_request_type, w_index)?;
        let _lifecycle_guard = self.lifecycle_lock.lock();

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
                .ok_or(crate::StarryError::InvalidInput)? as *const u8;
            read_user_bytes(current, data_ptr, w_length)?
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
            interface,
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

    fn submit_bulk_urb(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> crate::StarryResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read(current)?;
        if urb.type_ != descriptor::USBDEVFS_URB_TYPE_BULK {
            return Err(crate::StarryError::Unsupported);
        }
        if urb.buffer_length < 0 {
            return Err(crate::StarryError::InvalidInput);
        }

        self.submit_endpoint_urb_async(
            current,
            arg,
            descriptor::USBDEVFS_URB_TYPE_BULK,
            EndpointTransferType::Bulk,
            Vec::new(),
            urb.buffer_length as usize,
        )
    }

    fn submit_interrupt_urb(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> crate::StarryResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read(current)?;
        if urb.type_ != descriptor::USBDEVFS_URB_TYPE_INTERRUPT {
            return Err(crate::StarryError::Unsupported);
        }
        if urb.buffer_length < 0 {
            return Err(crate::StarryError::InvalidInput);
        }
        self.submit_endpoint_urb_async(
            current,
            arg,
            descriptor::USBDEVFS_URB_TYPE_INTERRUPT,
            EndpointTransferType::Interrupt,
            Vec::new(),
            urb.buffer_length as usize,
        )
    }

    fn submit_iso_urb(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> crate::StarryResult<usize> {
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read(current)?;
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

        let packet_lengths =
            self.read_iso_packet_lengths(current, arg, urb.number_of_packets as usize)?;
        let total_length = packet_lengths.iter().try_fold(0usize, |acc, len| {
            acc.checked_add(*len).ok_or(StarryError::OutOfRange)
        })?;
        if total_length > urb.buffer_length as usize {
            return Err(StarryError::InvalidInput);
        }

        self.submit_endpoint_urb_async(
            current,
            arg,
            descriptor::USBDEVFS_URB_TYPE_ISO,
            EndpointTransferType::Isochronous,
            packet_lengths,
            total_length,
        )
    }

    fn submit_urb(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
    ) -> crate::StarryResult<usize> {
        self.collect_submitted_urbs(None);
        let urb = (arg as *const descriptor::UsbdevfsUrb).vm_read(current)?;
        let type_ = urb.type_;
        match type_ {
            descriptor::USBDEVFS_URB_TYPE_CONTROL => self.submit_control_urb(current, arg),
            descriptor::USBDEVFS_URB_TYPE_BULK => self.submit_bulk_urb(current, arg),
            descriptor::USBDEVFS_URB_TYPE_INTERRUPT => self.submit_interrupt_urb(current, arg),
            descriptor::USBDEVFS_URB_TYPE_ISO => self.submit_iso_urb(current, arg),
            _ => Err(crate::StarryError::Unsupported),
        }
    }

    fn reap_urb(
        &self,
        current: &crate::task::UserTaskRef,
        arg: usize,
        nonblocking: bool,
    ) -> crate::StarryResult<usize> {
        let completed = if nonblocking {
            self.collect_submitted_urbs(None);
            self.pending_urbs
                .lock()
                .pop_front()
                .ok_or(crate::StarryError::WouldBlock)?
        } else {
            crate::task::future::block_on_user(
                current,
                crate::task::future::poll_exclusive(
                    || {
                        self.collect_submitted_urbs(None);
                        self.pending_urbs
                            .lock()
                            .pop_front()
                            .map_or(Poll::Pending, Poll::Ready)
                    },
                    |registrar| unsafe {
                        registrar.register_exclusive(&self.poll_urbs, IoEvents::IN | IoEvents::OUT)
                    },
                ),
            )
            .into_result()?
        };
        let user_urb_ptr = completed.user_urb_ptr;
        self.write_completed_urb(current, completed)?;
        (arg as *mut usize).vm_write(current, user_urb_ptr)?;
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
    fn validate_write_access(&self) -> StarryResult {
        self.base.validate_write_access()
    }

    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        self.ensure_current()?;
        self.base.read(dst)
    }

    fn write(&self, src: &mut IoSrc) -> StarryResult<usize> {
        self.ensure_current()?;
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

    fn ioctl(
        &self,
        current: &crate::task::UserTaskRef,
        cmd: u32,
        arg: usize,
    ) -> crate::StarryResult<usize> {
        self.ensure_current()?;
        match cmd {
            descriptor::USBDEVFS_CONTROL => {
                let log = usbfs_should_log_urb();
                let ctrl = descriptor::read_usbdevfs_ctrltransfer(current, arg)?;
                if log {
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
                match manager::is_snapshot_control_ioctl(current, arg) {
                    Ok(true) => {
                        let result = self.manager.snapshot_device_ioctl(
                            current,
                            self.bus_num,
                            self.device_num,
                            self.generation,
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
                self.claim_control_recipient(ctrl.b_request_type, ctrl.w_index)?;
                let _lifecycle_guard = self.lifecycle_lock.lock();
                let result = self.with_live_lease(|lease| lease.ioctl(current, cmd, arg));
                if log {
                    debug!("usbfs: control ioctl result={:?}", result);
                }
                result
            }
            descriptor::USBDEVFS_CLAIMINTERFACE => {
                let interface = descriptor::read_usbdevfs_u32(current, arg)?;
                if interface > u8::MAX as u32 {
                    return Err(StarryError::InvalidInput);
                }
                self.claim_interface(interface as u8, 0, false)
            }
            descriptor::USBDEVFS_RELEASEINTERFACE => {
                let interface = descriptor::read_usbdevfs_u32(current, arg)?;
                if interface > u8::MAX as u32 {
                    return Err(StarryError::InvalidInput);
                }
                self.release_interface(interface as u8)
            }
            descriptor::USBDEVFS_GETDRIVER => self.get_driver_ioctl(current, arg),
            descriptor::USBDEVFS_SETINTERFACE => {
                let set = descriptor::read_usbdevfs_setinterface(current, arg)?;
                if set.interface > u8::MAX as u32 || set.altsetting > u8::MAX as u32 {
                    return Err(StarryError::InvalidInput);
                }
                self.claim_interface(set.interface as u8, set.altsetting as u8, true)
            }
            descriptor::USBDEVFS_SETCONFIGURATION => self.set_configuration_ioctl(current, arg),
            descriptor::USBDEVFS_CLEAR_HALT => {
                let endpoint = descriptor::read_usbdevfs_u32(current, arg)?;
                if endpoint > u8::MAX as u32 {
                    return Err(StarryError::InvalidInput);
                }
                self.with_live_lease(|lease| lease.clear_halt(endpoint as u8))?;
                Ok(0)
            }
            descriptor::USBDEVFS_IOCTL => self.kernel_driver_ioctl(current, arg),
            descriptor::USBDEVFS_DISCONNECT | descriptor::USBDEVFS_CONNECT => {
                Err(StarryError::Unsupported)
            }
            descriptor::USBDEVFS_DISCONNECT_CLAIM => self.disconnect_claim_ioctl(current, arg),
            descriptor::USBDEVFS_DISCARDURB => self.discard_urb(arg),
            descriptor::USBDEVFS_BULK => self.bulk_ioctl(current, arg),
            descriptor::USBDEVFS_SUBMITURB => self.submit_urb(current, arg),
            descriptor::USBDEVFS_REAPURB => self.reap_urb(current, arg, false),
            descriptor::USBDEVFS_REAPURBNDELAY => self.reap_urb(current, arg, true),
            descriptor::USBDEVFS_CONNECTINFO | descriptor::USBDEVFS_GET_CAPABILITIES => self
                .manager
                .snapshot_device_ioctl(
                    current,
                    self.bus_num,
                    self.device_num,
                    self.generation,
                    cmd,
                    arg,
                ),
            _ => self.with_live_lease(|lease| lease.ioctl(current, cmd, arg)),
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

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        let interests = events & (IoEvents::IN | IoEvents::OUT);
        if interests.is_empty() {
            return;
        }
        unsafe { sink.register_shared(&self.poll_urbs, interests) };
        self.collect_submitted_urbs(None);
        if !self.pending_urbs.lock().is_empty() {
            unsafe { self.poll_urbs.wake(IoEvents::IN | IoEvents::OUT) };
        }
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        let interests = events & (IoEvents::IN | IoEvents::OUT);
        if interests.is_empty() {
            return;
        }
        unsafe { sink.register_exclusive(&self.poll_urbs, interests) };
        self.collect_submitted_urbs(None);
        if !self.pending_urbs.lock().is_empty() {
            unsafe { self.poll_urbs.wake(IoEvents::IN | IoEvents::OUT) };
        }
    }
}

impl Drop for UsbDeviceFile {
    fn drop(&mut self) {
        self.urb_worker.close(&self.manager);
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

        crate::task::kernel_thread_builder("usbfs-urb-cleanup".to_owned())
            .spawn(move || {
                let _lease = lease;
                cleanup_submitted_urbs(submitted);
            })
            .expect("failed to spawn kernel thread");
    }
}

fn complete_urb(
    pending_urbs: &Arc<IrqMutex<VecDeque<CompletedUrb>>>,
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

fn cleanup_submitted_urbs(mut submitted_urbs: Vec<SubmittedUrb>) {
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
            crate::task::sleep(Duration::from_millis(1));
        }
    }
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

fn snapshot_endpoint_interface(
    snapshot: &descriptor::UsbDeviceSnapshot,
    endpoint: u8,
) -> Option<u8> {
    let mut cursor = 18usize;
    let mut interface = None;
    let mut alternate = 0;
    while cursor + 2 <= snapshot.descriptor_blob.len() {
        let length = snapshot.descriptor_blob[cursor] as usize;
        if length < 2 || cursor + length > snapshot.descriptor_blob.len() {
            return None;
        }
        match snapshot.descriptor_blob[cursor + 1] {
            0x04 if length >= 9 => {
                interface = Some(snapshot.descriptor_blob[cursor + 2]);
                alternate = snapshot.descriptor_blob[cursor + 3];
            }
            0x05
                if length >= 7
                    && snapshot.descriptor_blob[cursor + 2] == endpoint
                    && alternate == 0 =>
            {
                return interface;
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

fn read_user_bytes(
    current: &crate::task::UserTaskRef,
    ptr: *const u8,
    len: usize,
) -> crate::StarryResult<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    vm_load(current, ptr, len).map_err(Into::into)
}

fn read_user_bytes_into(
    current: &crate::task::UserTaskRef,
    ptr: *const u8,
    dst: &mut [u8],
) -> crate::StarryResult<()> {
    if dst.is_empty() {
        return Ok(());
    }
    let bytes = read_user_bytes(current, ptr, dst.len())?;
    dst.copy_from_slice(&bytes);
    Ok(())
}

fn read_iso_packet_descs(
    current: &crate::task::UserTaskRef,
    urb_ptr: usize,
    num_packets: usize,
) -> StarryResult<Vec<descriptor::UsbdevfsIsoPacketDesc>> {
    let ptr = iso_packet_descs_ptr(urb_ptr)? as *const descriptor::UsbdevfsIsoPacketDesc;
    let mut descs = Vec::with_capacity(num_packets);
    for index in 0..num_packets {
        descs.push(unsafe { ptr.add(index) }.vm_read(current)?);
    }
    Ok(descs)
}

fn write_iso_packet_descs(
    current: &crate::task::UserTaskRef,
    urb_ptr: usize,
    descs: &[descriptor::UsbdevfsIsoPacketDesc],
) -> StarryResult<()> {
    let ptr = iso_packet_descs_ptr(urb_ptr)?;
    if !descs.is_empty() {
        vm_write_slice(current, ptr, descs)?;
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

#[cfg(all(test, axtest))]
mod wait_tests {
    use alloc::{sync::Arc, vec::Vec};
    use core::sync::atomic::{AtomicBool, Ordering};

    use ax_std::os::arceos::task::{
        sched::{CpuId, CpuSet, RtPriority, SchedulePolicy},
        thread::current,
    };

    use super::{UrbWorker, manager::UsbFsManager};

    #[axtest::axtest]
    fn usb_activity_publication_and_worker_close_do_not_strand_waiters() {
        let current = current::current_thread_handle().unwrap();
        let original_affinity = current.affinity().unwrap();
        let original_policy = current.base_policy();
        let mut affinity = CpuSet::empty(ax_hal::cpu_num());
        assert!(affinity.insert(CpuId::new(ax_hal::percpu::this_cpu_id() as u32)));
        current::set_current_thread_affinity(affinity.clone()).unwrap();
        current
            .set_policy(SchedulePolicy::fifo(RtPriority::new(10).unwrap()))
            .unwrap();

        let manager = Arc::new(UsbFsManager::new(Vec::new()));
        let observed = manager.usb_activity_seq();
        manager.notify_urb_workers();
        // Publication before waiter registration must remain observable.
        manager.wait_for_usb_activity(observed, || false);

        for close in [false, true] {
            let worker = Arc::new(UrbWorker::new());
            let entered = Arc::new(AtomicBool::new(false));
            let completed = Arc::new(AtomicBool::new(false));
            let waiter = crate::task::kernel_thread_builder("usb-activity-wait".into())
                .affinity(affinity.clone())
                .policy(SchedulePolicy::fifo(RtPriority::new(80).unwrap()))
                .spawn({
                    let manager = Arc::clone(&manager);
                    let worker = Arc::clone(&worker);
                    let entered = Arc::clone(&entered);
                    let completed = Arc::clone(&completed);
                    move || {
                        let observed = manager.usb_activity_seq();
                        manager.wait_for_usb_activity(observed, || {
                            entered.store(true, Ordering::Release);
                            worker.closed.load(Ordering::Acquire)
                        });
                        if close {
                            assert!(worker.closed.load(Ordering::Acquire));
                        }
                        completed.store(true, Ordering::Release);
                    }
                })
                .unwrap();

            // The higher-priority waiter shares this CPU. Once it has entered,
            // this lower-priority publisher can run only after it has parked.
            assert!(entered.load(Ordering::Acquire));
            assert!(!completed.load(Ordering::Acquire));
            if close {
                worker.close(&manager);
            } else {
                manager.notify_urb_workers();
            }
            // Wake may preempt the publisher before returning. No notification
            // lock may remain held while the waiter resumes or drops its wait.
            waiter.join().unwrap();
            assert!(completed.load(Ordering::Acquire));
        }

        let worker = UrbWorker::new();
        worker.close(&manager);
        // Even a generation sampled after close must not put the worker to sleep.
        manager.wait_for_usb_activity(manager.usb_activity_seq(), || {
            worker.closed.load(Ordering::Acquire)
        });
        manager.notify_urb_workers();

        current.set_policy(original_policy).unwrap();
        current::set_current_thread_affinity(original_affinity).unwrap();
    }
}
