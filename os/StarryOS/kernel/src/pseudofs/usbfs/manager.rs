use alloc::{collections::BTreeMap, sync::Arc, vec, vec::Vec};
use core::{
    future::poll_fn,
    task::{Context, Poll},
};

use ax_errno::{AxError, AxResult};
use crab_usb::{
    Device, DeviceInfo, Endpoint, EventHandler, ProbedDevice,
    usb_if::{
        endpoint::{RequestId, TransferCompletion, TransferRequest},
        err::{TransferError, USBError},
        host::ControlSetup,
        transfer::{Direction, Recipient, Request, RequestType},
    },
};
use event_listener::{Event as NotifyEvent, listener};
use rdrive::DeviceId as RDriveDeviceId;
use spin::{Mutex, RwLock};
use starry_vm::VmMutPtr;

use super::{
    descriptor::{
        USBDEVFS_BULK, USBDEVFS_CAP_BULK_CONTINUATION, USBDEVFS_CLAIMINTERFACE,
        USBDEVFS_CLEAR_HALT, USBDEVFS_CONNECTINFO, USBDEVFS_CONTROL, USBDEVFS_GET_CAPABILITIES,
        USBDEVFS_RELEASEINTERFACE, USBDEVFS_RESET, USBDEVFS_SETCONFIGURATION,
        USBDEVFS_SETINTERFACE, UsbDeviceSnapshot, UsbdevfsConnectInfo, read_usbdevfs_bulktransfer,
        read_usbdevfs_ctrltransfer, read_usbdevfs_setinterface, read_usbdevfs_u32,
        root_hub_snapshot, snapshot_probed_device,
    },
    irq::{self, PendingUsbIrqSlot},
};
use crate::mm::{UserConstPtr, UserPtr};

const ROOT_HUB_STABLE_DEVICE_ID: usize = usize::MAX;

pub(super) struct UsbHostState {
    pub(super) device_id: RDriveDeviceId,
    pub(super) bus_num: u8,
    pub(super) irq_num: Option<usize>,
    pub(super) needs_probe: bool,
    pub(super) next_device_num: u8,
    pub(super) stable_id_to_device_num: BTreeMap<usize, u8>,
}

#[derive(Default)]
struct UsbFsState {
    hosts: Vec<UsbHostState>,
    devices: BTreeMap<UsbStableId, UsbDeviceRecord>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct UsbStableId {
    host_device_id: RDriveDeviceId,
    device_id: usize,
}

struct UsbDeviceRecord {
    host_device_id: RDriveDeviceId,
    snapshot: UsbDeviceSnapshot,
    present: bool,
    unopened_info: Option<DeviceInfo>,
    live_device: Option<Arc<LiveDeviceState>>,
    open_count: usize,
    openable: bool,
    synthetic: bool,
}

type EndpointHandle = Arc<Mutex<Endpoint>>;

struct LiveDeviceState {
    device: Mutex<Device>,
    control: EndpointHandle,
    endpoints: RwLock<BTreeMap<u8, EndpointHandle>>,
}

pub(super) struct IsoTransferResult {
    pub(super) actual_length: usize,
}

#[derive(Clone)]
pub(super) struct SubmittedTransfer {
    endpoint: EndpointHandle,
    request_id: RequestId,
}

impl SubmittedTransfer {
    pub(super) fn try_reclaim(&self) -> AxResult<Option<TransferCompletion>> {
        self.endpoint
            .lock()
            .reclaim(self.request_id)
            .map_err(map_transfer_error)
    }

    pub(super) fn poll_reclaim(&self, cx: &mut Context<'_>) -> Poll<AxResult<TransferCompletion>> {
        match self.endpoint.lock().poll_request(self.request_id, cx) {
            Poll::Ready(result) => Poll::Ready(result.map_err(map_transfer_error)),
            Poll::Pending => Poll::Pending,
        }
    }

    pub(super) fn cancel(&self) -> AxResult<()> {
        self.endpoint
            .lock()
            .cancel(self.request_id)
            .map_err(map_transfer_error)
    }
}

fn wait_endpoint(
    endpoint: EndpointHandle,
    request: TransferRequest,
) -> AxResult<TransferCompletion> {
    let request_id = endpoint
        .lock()
        .submit(request)
        .map_err(map_transfer_error)?;
    ax_task::future::block_on(poll_fn(|cx| {
        match endpoint.lock().poll_request(request_id, cx) {
            Poll::Ready(result) => Poll::Ready(result.map_err(map_transfer_error)),
            Poll::Pending => Poll::Pending,
        }
    }))
}

pub(super) struct UsbFsManager {
    state: Mutex<UsbFsState>,
    open_lock: Mutex<()>,
    pub(super) refresh_event: NotifyEvent,
}

pub(super) struct UsbDeviceLease {
    manager: Arc<UsbFsManager>,
    stable_id: UsbStableId,
}

impl UsbDeviceLease {
    pub(super) fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        self.manager.opened_device_ioctl(self.stable_id, cmd, arg)
    }

    pub(super) fn claim_interface(&self, interface: u8, alternate: u8) -> AxResult<()> {
        self.manager
            .live_claim_interface(self.stable_id, interface, alternate)
    }

    pub(super) fn ensure_configured(&self) -> AxResult<()> {
        self.manager.live_ensure_configured(self.stable_id)
    }

    pub(super) fn set_configuration(&self, configuration: u8) -> AxResult<()> {
        self.manager
            .live_set_configuration(self.stable_id, configuration)
    }

    pub(super) fn bulk_in(&self, endpoint: u8, data: &mut [u8]) -> AxResult<usize> {
        self.manager.live_bulk_in(self.stable_id, endpoint, data)
    }

    pub(super) fn bulk_out(&self, endpoint: u8, data: &[u8]) -> AxResult<usize> {
        self.manager.live_bulk_out(self.stable_id, endpoint, data)
    }

    pub(super) fn interrupt_in(&self, endpoint: u8, data: &mut [u8]) -> AxResult<usize> {
        self.manager
            .live_interrupt_in(self.stable_id, endpoint, data)
    }

    pub(super) fn interrupt_out(&self, endpoint: u8, data: &[u8]) -> AxResult<usize> {
        self.manager
            .live_interrupt_out(self.stable_id, endpoint, data)
    }

    pub(super) fn iso_in(
        &self,
        endpoint: u8,
        data: &mut [u8],
        packet_lengths: &[usize],
    ) -> AxResult<IsoTransferResult> {
        self.manager
            .live_iso_in(self.stable_id, endpoint, data, packet_lengths)
    }

    pub(super) fn iso_out(
        &self,
        endpoint: u8,
        data: &[u8],
        packet_lengths: &[usize],
    ) -> AxResult<usize> {
        self.manager
            .live_iso_out(self.stable_id, endpoint, data, packet_lengths)
    }

    pub(super) fn submit_endpoint_transfer(
        &self,
        endpoint: u8,
        request: TransferRequest,
    ) -> AxResult<SubmittedTransfer> {
        self.manager
            .live_submit_endpoint_transfer(self.stable_id, endpoint, request)
    }

    pub(super) fn submit_control_transfer(
        &self,
        request: TransferRequest,
    ) -> AxResult<SubmittedTransfer> {
        self.manager
            .live_submit_control_transfer(self.stable_id, request)
    }
}

impl Drop for UsbDeviceLease {
    fn drop(&mut self) {
        self.manager.release_device(self.stable_id);
    }
}

impl UsbFsManager {
    pub(super) fn new(hosts: Vec<UsbHostState>) -> Self {
        let mut hosts = hosts;
        let mut devices = BTreeMap::new();
        for host in &mut hosts {
            host.next_device_num = host.next_device_num.max(2);
            devices.insert(
                UsbStableId {
                    host_device_id: host.device_id,
                    device_id: ROOT_HUB_STABLE_DEVICE_ID,
                },
                UsbDeviceRecord {
                    host_device_id: host.device_id,
                    snapshot: root_hub_snapshot(host.bus_num),
                    present: true,
                    unopened_info: None,
                    live_device: None,
                    open_count: 0,
                    openable: false,
                    synthetic: true,
                },
            );
        }

        Self {
            state: Mutex::new(UsbFsState { hosts, devices }),
            open_lock: Mutex::new(()),
            refresh_event: NotifyEvent::new(),
        }
    }

    pub(super) fn refresh_dirty_hosts(&self) {
        #[cfg(not(target_os = "none"))]
        return;
        #[cfg(target_os = "none")]
        {
            let pending_hosts = {
                let mut state = self.state.lock();
                let open_hosts = state
                    .devices
                    .values()
                    .filter(|record| record.open_count > 0)
                    .map(|record| record.host_device_id)
                    .collect::<Vec<_>>();
                let mut pending = Vec::new();
                for host in &mut state.hosts {
                    let irq_dirty = host.irq_num.map(irq::take_dirty).unwrap_or(false);
                    if open_hosts.contains(&host.device_id) {
                        host.needs_probe |= irq_dirty;
                        continue;
                    }
                    if host.needs_probe || irq_dirty {
                        host.needs_probe = false;
                        pending.push((host.device_id, host.bus_num));
                    }
                }
                pending
            };

            for (device_id, bus_num) in pending_hosts {
                let host = match rdrive::get::<axplat_dyn::drivers::usb::PlatformUsbHost>(device_id)
                {
                    Ok(host) => host,
                    Err(err) => {
                        warn!(
                            "usbfs: failed to reacquire USB host {:?}: {err:?}",
                            device_id
                        );
                        continue;
                    }
                };

                let mut guard = match host.lock() {
                    Ok(guard) => guard,
                    Err(err) => {
                        warn!("usbfs: failed to lock USB host {:?}: {err:?}", device_id);
                        continue;
                    }
                };

                let devices = match ax_task::future::block_on(guard.host_mut().probe_devices()) {
                    Ok(devices) => devices,
                    Err(err) => {
                        warn!("usbfs: refresh probe failed on bus {bus_num}: {err:?}");
                        continue;
                    }
                };
                drop(guard);
                self.apply_probe_results(device_id, bus_num, devices);
            }
        }
    }

    pub(super) fn bus_numbers(&self) -> Vec<u8> {
        let state = self.state.lock();
        state.hosts.iter().map(|host| host.bus_num).collect()
    }

    pub(super) fn device_numbers(&self, bus_num: u8) -> Vec<u8> {
        self.refresh_dirty_hosts();
        let state = self.state.lock();
        state
            .devices
            .values()
            .filter(|record| record.present && record.snapshot.bus_num == bus_num)
            .map(|record| record.snapshot.device_num)
            .collect()
    }

    pub(super) fn device_snapshot(&self, bus_num: u8, device_num: u8) -> Option<UsbDeviceSnapshot> {
        self.refresh_dirty_hosts();
        self.state.lock().devices.values().find_map(|record| {
            (record.present
                && record.snapshot.bus_num == bus_num
                && record.snapshot.device_num == device_num)
                .then(|| record.snapshot.clone())
        })
    }

    pub(super) fn acquire_device(
        self: &Arc<Self>,
        bus_num: u8,
        device_num: u8,
    ) -> AxResult<UsbDeviceLease> {
        let _open_guard = self.open_lock.lock();
        let stable_id = {
            let state = self.state.lock();
            state
                .devices
                .iter()
                .find_map(|(stable_id, record)| {
                    (record.present
                        && record.snapshot.bus_num == bus_num
                        && record.snapshot.device_num == device_num)
                        .then_some(*stable_id)
                })
                .ok_or(AxError::NotFound)?
        };

        self.ensure_live_device(stable_id)?;

        let mut state = self.state.lock();
        let record = state.devices.get_mut(&stable_id).ok_or(AxError::NotFound)?;
        record.open_count = record.open_count.saturating_add(1);
        Ok(UsbDeviceLease {
            manager: self.clone(),
            stable_id,
        })
    }

    fn opened_device_ioctl(&self, stable_id: UsbStableId, cmd: u32, arg: usize) -> AxResult<usize> {
        match cmd {
            USBDEVFS_CONTROL => self.handle_control(stable_id, arg),
            USBDEVFS_CONNECTINFO => {
                let snapshot = self.snapshot_by_id(stable_id)?;
                (arg as *mut UsbdevfsConnectInfo).vm_write(UsbdevfsConnectInfo {
                    devnum: snapshot.device_num as u32,
                    slow: 0,
                    _padding: [0; 3],
                })?;
                Ok(0)
            }
            USBDEVFS_GET_CAPABILITIES => {
                (arg as *mut u32).vm_write(USBDEVFS_CAP_BULK_CONTINUATION)?;
                Ok(0)
            }
            USBDEVFS_CLAIMINTERFACE | USBDEVFS_RELEASEINTERFACE => {
                let _ = read_usbdevfs_u32(arg)?;
                Err(AxError::Unsupported)
            }
            USBDEVFS_SETINTERFACE => {
                let _ = read_usbdevfs_setinterface(arg)?;
                Err(AxError::Unsupported)
            }
            USBDEVFS_BULK => {
                let bulk = read_usbdevfs_bulktransfer(arg)?;
                let len = bulk.len as usize;
                if len > 0 {
                    if bulk.ep & 0x80 != 0 {
                        let _ = UserPtr::<u8>::from(bulk.data).get_as_mut_slice(len)?;
                    } else {
                        let _ =
                            UserConstPtr::<u8>::from(bulk.data as *const u8).get_as_slice(len)?;
                    }
                }
                Err(AxError::Unsupported)
            }
            USBDEVFS_SETCONFIGURATION | USBDEVFS_CLEAR_HALT => {
                let _ = read_usbdevfs_u32(arg)?;
                Err(AxError::Unsupported)
            }
            USBDEVFS_RESET => Err(AxError::Unsupported),
            _ => Err(AxError::Unsupported),
        }
    }

    pub(super) fn snapshot_device_ioctl(
        &self,
        bus_num: u8,
        device_num: u8,
        cmd: u32,
        arg: usize,
    ) -> AxResult<usize> {
        let snapshot = self
            .device_snapshot(bus_num, device_num)
            .ok_or(AxError::NotFound)?;
        match cmd {
            USBDEVFS_CONTROL => {
                let ctrl = read_usbdevfs_ctrltransfer(arg)?;
                match (ctrl.b_request_type, ctrl.b_request, ctrl.w_value >> 8) {
                    (0x80, 0x06, 0x01) => {
                        let len = snapshot
                            .descriptor_blob
                            .len()
                            .min(18)
                            .min(ctrl.w_length as usize);
                        UserPtr::<u8>::from(ctrl.data)
                            .get_as_mut_slice(len)?
                            .copy_from_slice(&snapshot.descriptor_blob[..len]);
                        Ok(len)
                    }
                    (0x80, 0x06, 0x02) => {
                        let config_index = (ctrl.w_value & 0xff) as usize;
                        let config = snapshot_config_blob(&snapshot, config_index)
                            .ok_or(AxError::Unsupported)?;
                        let len = config.len().min(ctrl.w_length as usize);
                        UserPtr::<u8>::from(ctrl.data)
                            .get_as_mut_slice(len)?
                            .copy_from_slice(&config[..len]);
                        Ok(len)
                    }
                    (0x80, 0x08, _) if ctrl.w_length >= 1 => {
                        ctrl.data
                            .vm_write(snapshot_active_configuration(&snapshot))?;
                        Ok(1)
                    }
                    _ => Err(AxError::Unsupported),
                }
            }
            USBDEVFS_CONNECTINFO => {
                (arg as *mut UsbdevfsConnectInfo).vm_write(UsbdevfsConnectInfo {
                    devnum: snapshot.device_num as u32,
                    slow: 0,
                    _padding: [0; 3],
                })?;
                Ok(0)
            }
            USBDEVFS_GET_CAPABILITIES => {
                (arg as *mut u32).vm_write(USBDEVFS_CAP_BULK_CONTINUATION)?;
                Ok(0)
            }
            _ => Err(AxError::Unsupported),
        }
    }

    fn apply_probe_results(
        &self,
        device_id: RDriveDeviceId,
        bus_num: u8,
        devices: Vec<ProbedDevice>,
    ) {
        let mut state = self.state.lock();
        let Some(host_index) = state
            .hosts
            .iter()
            .position(|host| host.device_id == device_id)
        else {
            return;
        };

        let updates = {
            let host_state = &mut state.hosts[host_index];
            let mut updates = Vec::new();
            for device in devices {
                let stable_id = UsbStableId {
                    host_device_id: device_id,
                    device_id: device.id(),
                };
                let snapshot = snapshot_probed_device(
                    bus_num,
                    &mut host_state.next_device_num,
                    &mut host_state.stable_id_to_device_num,
                    &device,
                );
                let unopened_info = device.into_device_info();
                let openable = unopened_info.is_some();
                updates.push((stable_id, snapshot, unopened_info, openable));
            }
            updates
        };

        for (stable_id, snapshot, unopened_info, openable) in updates {
            let record = state
                .devices
                .entry(stable_id)
                .or_insert_with(|| UsbDeviceRecord {
                    host_device_id: device_id,
                    snapshot: snapshot.clone(),
                    present: true,
                    unopened_info: None,
                    live_device: None,
                    open_count: 0,
                    openable,
                    synthetic: false,
                });
            record.host_device_id = device_id;
            record.snapshot = snapshot;
            record.present = true;
            record.openable = openable;
            record.unopened_info = unopened_info;
        }
    }

    fn ensure_live_device(&self, stable_id: UsbStableId) -> AxResult<()> {
        loop {
            enum OpenAction {
                Ready,
                Open {
                    host_device_id: RDriveDeviceId,
                    info: DeviceInfo,
                },
                Refresh {
                    host_device_id: RDriveDeviceId,
                    bus_num: u8,
                },
            }

            let action = {
                let mut state = self.state.lock();
                let record = state.devices.get_mut(&stable_id).ok_or(AxError::NotFound)?;
                if record.live_device.is_some() {
                    OpenAction::Ready
                } else if let Some(info) = record.unopened_info.take() {
                    OpenAction::Open {
                        host_device_id: record.host_device_id,
                        info,
                    }
                } else if record.synthetic || !record.openable {
                    return Err(AxError::Unsupported);
                } else if record.present {
                    OpenAction::Refresh {
                        host_device_id: record.host_device_id,
                        bus_num: record.snapshot.bus_num,
                    }
                } else {
                    return Err(AxError::NoSuchDevice);
                }
            };

            match action {
                OpenAction::Ready => return Ok(()),
                OpenAction::Open {
                    host_device_id,
                    info,
                } => {
                    let mut live_device = self.open_device(host_device_id, &info)?;
                    let control = Arc::new(Mutex::new(
                        live_device.take_control_endpoint().map_err(map_usb_error)?,
                    ));
                    let mut state = self.state.lock();
                    let record = state.devices.get_mut(&stable_id).ok_or(AxError::NotFound)?;
                    record.live_device = Some(Arc::new(LiveDeviceState {
                        device: Mutex::new(live_device),
                        control,
                        endpoints: RwLock::new(BTreeMap::new()),
                    }));
                    return Ok(());
                }
                OpenAction::Refresh {
                    host_device_id,
                    bus_num,
                } => {
                    self.refresh_host(host_device_id, bus_num)?;
                }
            }
        }
    }

    #[cfg(target_os = "none")]
    fn open_device(&self, host_device_id: RDriveDeviceId, info: &DeviceInfo) -> AxResult<Device> {
        let host = rdrive::get::<axplat_dyn::drivers::usb::PlatformUsbHost>(host_device_id)
            .map_err(|_| AxError::NoSuchDevice)?;
        let mut guard = host.lock().map_err(|_| AxError::ResourceBusy)?;
        ax_task::future::block_on(guard.host_mut().open_device(info)).map_err(|err| {
            warn!(
                "usbfs: failed to open live device on host {:?} for USB device id {}: {:?}",
                host_device_id,
                info.id(),
                err
            );
            map_usb_error(err)
        })
    }

    #[cfg(not(target_os = "none"))]
    fn open_device(&self, host_device_id: RDriveDeviceId, info: &DeviceInfo) -> AxResult<Device> {
        let _ = (host_device_id, info);
        Err(AxError::Unsupported)
    }

    #[cfg(target_os = "none")]
    fn refresh_host(&self, host_device_id: RDriveDeviceId, bus_num: u8) -> AxResult<()> {
        let host = rdrive::get::<axplat_dyn::drivers::usb::PlatformUsbHost>(host_device_id)
            .map_err(|_| AxError::NoSuchDevice)?;
        let mut guard = host.lock().map_err(|_| AxError::ResourceBusy)?;
        let devices =
            ax_task::future::block_on(guard.host_mut().probe_devices()).map_err(map_usb_error)?;
        drop(guard);
        self.apply_probe_results(host_device_id, bus_num, devices);
        Ok(())
    }

    #[cfg(not(target_os = "none"))]
    fn refresh_host(&self, host_device_id: RDriveDeviceId, bus_num: u8) -> AxResult<()> {
        let _ = (host_device_id, bus_num);
        Err(AxError::Unsupported)
    }

    fn snapshot_by_id(&self, stable_id: UsbStableId) -> AxResult<UsbDeviceSnapshot> {
        self.state
            .lock()
            .devices
            .get(&stable_id)
            .map(|record| record.snapshot.clone())
            .ok_or(AxError::NotFound)
    }

    fn live_device_by_id(&self, stable_id: UsbStableId) -> AxResult<Arc<LiveDeviceState>> {
        self.state
            .lock()
            .devices
            .get(&stable_id)
            .and_then(|record| record.live_device.as_ref().cloned())
            .ok_or(AxError::NoSuchDevice)
    }

    fn live_endpoint(&self, stable_id: UsbStableId, endpoint: u8) -> AxResult<EndpointHandle> {
        let live_device = self.live_device_by_id(stable_id)?;
        live_device
            .endpoints
            .read()
            .get(&endpoint)
            .cloned()
            .ok_or(AxError::NotFound)
    }

    fn live_control_endpoint(&self, stable_id: UsbStableId) -> AxResult<EndpointHandle> {
        let live_device = self.live_device_by_id(stable_id)?;
        Ok(live_device.control.clone())
    }

    fn live_control_transfer(
        &self,
        stable_id: UsbStableId,
        b_request_type: u8,
        b_request: u8,
        w_value: u16,
        w_index: u16,
        data: &mut [u8],
    ) -> AxResult<usize> {
        let setup = control_setup_from_raw(b_request_type, b_request, w_value, w_index);
        let endpoint = self.live_control_endpoint(stable_id)?;
        match direction_from_raw(b_request_type) {
            Direction::In => wait_endpoint(endpoint, TransferRequest::control_in(setup, data))
                .map(|completion| completion.actual_length),
            Direction::Out => wait_endpoint(endpoint, TransferRequest::control_out(setup, data))
                .map(|completion| completion.actual_length),
        }
    }

    fn live_claim_interface(
        &self,
        stable_id: UsbStableId,
        interface: u8,
        alternate: u8,
    ) -> AxResult<()> {
        self.live_ensure_configured(stable_id)?;
        let live_device = self.live_device_by_id(stable_id)?;
        {
            let mut device = live_device.device.lock();
            let mut control_guard = live_device.control.lock();
            let control = &mut *control_guard;
            ax_task::future::block_on(
                device.claim_interface_with_control(control, interface, alternate),
            )
            .map_err(map_usb_error)?;
            let endpoints = device.take_endpoints().map_err(map_usb_error)?;
            let endpoints = endpoints
                .into_iter()
                .map(|(address, endpoint)| (address, Arc::new(Mutex::new(endpoint))))
                .collect();
            *live_device.endpoints.write() = endpoints;
        }
        Ok(())
    }

    fn live_ensure_configured(&self, stable_id: UsbStableId) -> AxResult<()> {
        let live_device = self.live_device_by_id(stable_id)?;
        let mut device = live_device.device.lock();
        let mut control_guard = live_device.control.lock();
        let control = &mut *control_guard;
        if ax_task::future::block_on(device.current_configuration_descriptor_with_control(control))
            .is_ok()
        {
            return Ok(());
        }

        let configuration_value = device
            .configurations()
            .first()
            .map(|config| config.configuration_value)
            .ok_or(AxError::NotFound)?;
        ax_task::future::block_on(
            device.set_configuration_with_control(control, configuration_value),
        )
        .map_err(map_usb_error)
    }

    fn live_set_configuration(&self, stable_id: UsbStableId, configuration: u8) -> AxResult<()> {
        let live_device = self.live_device_by_id(stable_id)?;
        let mut device = live_device.device.lock();
        let mut control_guard = live_device.control.lock();
        let control = &mut *control_guard;
        ax_task::future::block_on(device.set_configuration_with_control(control, configuration))
            .map_err(map_usb_error)?;
        live_device.endpoints.write().clear();
        Ok(())
    }

    fn live_bulk_in(
        &self,
        stable_id: UsbStableId,
        endpoint: u8,
        data: &mut [u8],
    ) -> AxResult<usize> {
        let endpoint = self.live_endpoint(stable_id, endpoint)?;
        wait_endpoint(endpoint, TransferRequest::bulk_in(data))
            .map(|completion| completion.actual_length)
    }

    fn live_bulk_out(&self, stable_id: UsbStableId, endpoint: u8, data: &[u8]) -> AxResult<usize> {
        let endpoint = self.live_endpoint(stable_id, endpoint)?;
        wait_endpoint(endpoint, TransferRequest::bulk_out(data))
            .map(|completion| completion.actual_length)
    }

    fn live_interrupt_in(
        &self,
        stable_id: UsbStableId,
        endpoint: u8,
        data: &mut [u8],
    ) -> AxResult<usize> {
        let endpoint = self.live_endpoint(stable_id, endpoint)?;
        wait_endpoint(endpoint, TransferRequest::interrupt_in(data))
            .map(|completion| completion.actual_length)
    }

    fn live_interrupt_out(
        &self,
        stable_id: UsbStableId,
        endpoint: u8,
        data: &[u8],
    ) -> AxResult<usize> {
        let endpoint = self.live_endpoint(stable_id, endpoint)?;
        wait_endpoint(endpoint, TransferRequest::interrupt_out(data))
            .map(|completion| completion.actual_length)
    }

    fn live_iso_in(
        &self,
        stable_id: UsbStableId,
        endpoint: u8,
        data: &mut [u8],
        packet_lengths: &[usize],
    ) -> AxResult<IsoTransferResult> {
        let endpoint = self.live_endpoint(stable_id, endpoint)?;
        wait_endpoint(endpoint, TransferRequest::iso_in(data, packet_lengths)).map(|completion| {
            IsoTransferResult {
                actual_length: completion.actual_length,
            }
        })
    }

    fn live_iso_out(
        &self,
        stable_id: UsbStableId,
        endpoint: u8,
        data: &[u8],
        packet_lengths: &[usize],
    ) -> AxResult<usize> {
        let endpoint = self.live_endpoint(stable_id, endpoint)?;
        wait_endpoint(endpoint, TransferRequest::iso_out(data, packet_lengths))
            .map(|completion| completion.actual_length)
    }

    fn live_submit_endpoint_transfer(
        &self,
        stable_id: UsbStableId,
        endpoint: u8,
        request: TransferRequest,
    ) -> AxResult<SubmittedTransfer> {
        let endpoint = self.live_endpoint(stable_id, endpoint)?;
        let request_id = endpoint
            .lock()
            .submit(request)
            .map_err(map_transfer_error)?;
        Ok(SubmittedTransfer {
            endpoint,
            request_id,
        })
    }

    fn live_submit_control_transfer(
        &self,
        stable_id: UsbStableId,
        request: TransferRequest,
    ) -> AxResult<SubmittedTransfer> {
        let endpoint = self.live_control_endpoint(stable_id)?;
        let request_id = endpoint
            .lock()
            .submit(request)
            .map_err(map_transfer_error)?;
        Ok(SubmittedTransfer {
            endpoint,
            request_id,
        })
    }

    fn handle_control(&self, stable_id: UsbStableId, arg: usize) -> AxResult<usize> {
        let ctrl = read_usbdevfs_ctrltransfer(arg)?;
        match direction_from_raw(ctrl.b_request_type) {
            Direction::In => {
                let mut data = vec![0; ctrl.w_length as usize];
                let actual = self.live_control_transfer(
                    stable_id,
                    ctrl.b_request_type,
                    ctrl.b_request,
                    ctrl.w_value,
                    ctrl.w_index,
                    &mut data,
                )?;
                UserPtr::<u8>::from(ctrl.data)
                    .get_as_mut_slice(actual)?
                    .copy_from_slice(&data[..actual]);
                Ok(actual)
            }
            Direction::Out => {
                let mut data = UserConstPtr::<u8>::from(ctrl.data as *const u8)
                    .get_as_slice(ctrl.w_length as usize)?
                    .to_vec();
                self.live_control_transfer(
                    stable_id,
                    ctrl.b_request_type,
                    ctrl.b_request,
                    ctrl.w_value,
                    ctrl.w_index,
                    &mut data,
                )
            }
        }
    }

    fn release_device(&self, stable_id: UsbStableId) {
        let _open_guard = self.open_lock.lock();
        let mut refresh = None;
        {
            let mut state = self.state.lock();
            let Some(record) = state.devices.get_mut(&stable_id) else {
                return;
            };
            if record.open_count > 0 {
                record.open_count -= 1;
            }
            if record.open_count != 0 {
                return;
            }

            record.live_device = None;
            if !record.present {
                state.devices.remove(&stable_id);
                return;
            }
            if record.openable && record.unopened_info.is_none() {
                refresh = Some((record.host_device_id, record.snapshot.bus_num));
            }
        }

        if let Some((host_device_id, bus_num)) = refresh
            && let Err(err) = self.refresh_host(host_device_id, bus_num)
        {
            warn!(
                "usbfs: failed to refresh bus {} after close: {:?}",
                bus_num, err
            );
        }
    }
}

fn snapshot_active_configuration(snapshot: &UsbDeviceSnapshot) -> u8 {
    if snapshot.descriptor_blob.len() > 23
        && snapshot.descriptor_blob[18] == 9
        && snapshot.descriptor_blob[19] == 0x02
    {
        snapshot.descriptor_blob[23]
    } else {
        0
    }
}

fn snapshot_config_blob(snapshot: &UsbDeviceSnapshot, index: usize) -> Option<&[u8]> {
    let mut cursor = 18usize;
    let mut current_index = 0usize;
    while cursor + 9 <= snapshot.descriptor_blob.len() {
        let length = snapshot.descriptor_blob[cursor] as usize;
        let desc_type = snapshot.descriptor_blob[cursor + 1];
        if length == 0 {
            return None;
        }
        if desc_type != 0x02 {
            cursor = cursor.checked_add(length)?;
            continue;
        }
        let total_length = u16::from_le_bytes([
            snapshot.descriptor_blob[cursor + 2],
            snapshot.descriptor_blob[cursor + 3],
        ]) as usize;
        let end = cursor.checked_add(total_length)?;
        if end > snapshot.descriptor_blob.len() {
            return None;
        }
        if current_index == index {
            return Some(&snapshot.descriptor_blob[cursor..end]);
        }
        current_index += 1;
        cursor = end;
    }
    None
}

pub(super) async fn usbfs_refresh_task(manager: Arc<UsbFsManager>) {
    loop {
        listener!(manager.refresh_event => refresh_listener);
        refresh_listener.await;
        manager.refresh_dirty_hosts();
    }
}

pub(super) fn initialize_hosts(manager: &UsbFsManager) -> usize {
    #[cfg(not(target_os = "none"))]
    {
        let _ = manager;
        0
    }
    #[cfg(target_os = "none")]
    {
        let hosts = {
            let state = manager.state.lock();
            state
                .hosts
                .iter()
                .map(|host| (host.device_id, host.bus_num, host.irq_num))
                .collect::<Vec<_>>()
        };

        let mut initialized = 0usize;
        let mut failed_device_ids = Vec::new();

        for (device_id, bus_num, irq_num) in hosts {
            let host = match rdrive::get::<axplat_dyn::drivers::usb::PlatformUsbHost>(device_id) {
                Ok(host) => host,
                Err(err) => {
                    warn!(
                        "usbfs: failed to reacquire USB host {:?} for init: {err:?}",
                        device_id
                    );
                    failed_device_ids.push((device_id, irq_num));
                    continue;
                }
            };

            let mut guard = match host.lock() {
                Ok(guard) => guard,
                Err(err) => {
                    warn!(
                        "usbfs: failed to lock USB host {:?} for init: {err:?}",
                        device_id
                    );
                    failed_device_ids.push((device_id, irq_num));
                    continue;
                }
            };

            info!("usbfs: initializing host on bus {}", bus_num);
            if let Err(err) = ax_task::future::block_on(guard.host_mut().init()) {
                warn!("usbfs: failed to initialize USB host on bus {bus_num}: {err:?}");
                failed_device_ids.push((device_id, irq_num));
                continue;
            }

            let devices = match ax_task::future::block_on(guard.host_mut().probe_devices()) {
                Ok(devices) => devices,
                Err(err) => {
                    warn!("usbfs: initial probe failed on bus {bus_num}: {err:?}");
                    failed_device_ids.push((device_id, irq_num));
                    continue;
                }
            };

            info!("usbfs: host on bus {} initialized", bus_num);
            initialized += 1;

            if let Some(irq_num) = irq_num {
                irq::bootstrap_irq(irq_num);
            }
            manager.apply_probe_results(device_id, bus_num, devices);
        }

        if !failed_device_ids.is_empty() {
            let mut state = manager.state.lock();
            state.hosts.retain(|host| {
                !failed_device_ids
                    .iter()
                    .any(|(failed_device_id, _)| *failed_device_id == host.device_id)
            });
        }

        for (_, irq_num) in failed_device_ids {
            if let Some(irq_num) = irq_num {
                let _ = ax_hal::irq::unregister(irq_num);
            }
        }

        info!("usbfs: {} host(s) ready", initialized);
        initialized
    }
}

fn map_transfer_error(err: TransferError) -> AxError {
    match err {
        TransferError::Timeout => AxError::TimedOut,
        TransferError::Cancelled => AxError::Interrupted,
        TransferError::Stall => AxError::BrokenPipe,
        TransferError::QueueFull => AxError::ResourceBusy,
        TransferError::InvalidEndpoint => AxError::InvalidInput,
        TransferError::NoDevice => AxError::NoSuchDevice,
        TransferError::NotSupported => AxError::Unsupported,
        TransferError::Other(_) => AxError::Io,
    }
}

fn map_usb_error(err: USBError) -> AxError {
    match err {
        USBError::Timeout => AxError::TimedOut,
        USBError::NoMemory => AxError::NoMemory,
        USBError::TransferError(err) => map_transfer_error(err),
        USBError::NotInitialized | USBError::ConfigurationNotSet => AxError::BadState,
        USBError::NotFound => AxError::NoSuchDevice,
        USBError::InvalidParameter => AxError::InvalidInput,
        USBError::SlotLimitReached => AxError::ResourceBusy,
        USBError::NotSupported => AxError::Unsupported,
        USBError::Other(_) => AxError::Io,
    }
}

fn direction_from_raw(raw: u8) -> Direction {
    if raw & 0x80 != 0 {
        Direction::In
    } else {
        Direction::Out
    }
}

pub(super) fn control_setup_from_raw(
    b_request_type: u8,
    b_request: u8,
    w_value: u16,
    w_index: u16,
) -> ControlSetup {
    ControlSetup {
        request_type: request_type_from_raw(b_request_type),
        recipient: recipient_from_raw(b_request_type),
        request: Request::Other(b_request),
        value: w_value,
        index: w_index,
    }
}

fn request_type_from_raw(raw: u8) -> RequestType {
    match (raw >> 5) & 0x03 {
        0 => RequestType::Standard,
        1 => RequestType::Class,
        2 => RequestType::Vendor,
        _ => RequestType::Reserved,
    }
}

fn recipient_from_raw(raw: u8) -> Recipient {
    match raw & 0x1f {
        0 => Recipient::Device,
        1 => Recipient::Interface,
        2 => Recipient::Endpoint,
        _ => Recipient::Other,
    }
}

pub(super) fn discover_hosts() -> (Vec<UsbHostState>, Vec<PendingUsbIrqSlot>) {
    #[cfg(not(target_os = "none"))]
    {
        (Vec::new(), Vec::new())
    }
    #[cfg(target_os = "none")]
    {
        let hosts = rdrive::get_list::<axplat_dyn::drivers::usb::PlatformUsbHost>();
        let mut initialized_hosts = Vec::new();
        let mut irq_slots = Vec::new();

        for (index, host) in hosts.into_iter().enumerate() {
            let device_id = host.descriptor().device_id();
            let bus_num = (index + 1) as u8;
            info!("usbfs: preparing host {:?} as bus {}", device_id, bus_num);

            let mut guard = match host.lock() {
                Ok(guard) => guard,
                Err(err) => {
                    warn!("usbfs: failed to lock USB host {device_id:?}: {err:?}");
                    continue;
                }
            };

            let irq_num = guard.irq_num();
            info!("usbfs: creating event handler for bus {}", bus_num);
            let event_handler: EventHandler = guard.host_mut().create_event_handler();
            drop(guard);

            if let Some(irq_num) = irq_num {
                irq_slots.push(PendingUsbIrqSlot {
                    irq_num,
                    device_id,
                    bus_num,
                    handler: event_handler,
                });
            }

            initialized_hosts.push(UsbHostState {
                device_id,
                bus_num,
                irq_num,
                needs_probe: true,
                next_device_num: 1,
                stable_id_to_device_num: BTreeMap::new(),
            });
        }

        info!("usbfs: discovered {} USB host(s)", initialized_hosts.len());
        (initialized_hosts, irq_slots)
    }
}
