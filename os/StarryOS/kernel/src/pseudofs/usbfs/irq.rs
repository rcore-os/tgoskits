//! One fixed-CPU maintenance owner for every hardware USB host.

use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    format,
    sync::Arc,
    vec::Vec,
};
use core::{
    future::Future,
    sync::atomic::{AtomicU8, Ordering},
    task::{Context, Poll, Waker},
};

use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_runtime::{
    hal::irq::{IrqContext, IrqError, IrqId, IrqReturn},
    maintenance::{
        DeviceMaintenanceHandle, LocalIrqWake, LocalIrqWakeError, LocalOwnerCell,
        LocalOwnerControl, LocalOwnerIrq, MaintenanceCauses, MaintenanceClosed, MaintenanceError,
        MaintenanceIrqAction, MaintenancePublishResult, MaintenanceRegistrar, MaintenanceSession,
        MaintenanceThread, spawn_maintenance_domain,
    },
    task::{LocalExecutor, WaitQueue, current_thread_handle, yield_current_cpu},
};
use crab_usb::{
    Device, DeviceInfo, Endpoint, ProbedDevice, UsbIrqEvent, UsbIrqFault,
    err::{Result as UsbResult, TransferError, USBError},
    usb_if::endpoint::{RequestId, TransferCompletion, TransferRequest},
};
use rdif_irq::{ContainmentCause, FaultContainment, IrqCapture, MaskedSource};
use rdrive::DeviceId as RDriveDeviceId;

use super::manager::UsbFsManager;

const USBFS_EVENT_BATCH_LIMIT: usize = 64;
const USB_OWNER_CPU: usize = 0;
const OWNER_STARTING: u8 = 0;
const OWNER_READY: u8 = 1;
const OWNER_FAILED: u8 = 2;

static USBFS_MANAGER: LazyInit<Arc<UsbFsManager>> = LazyInit::new();
static USBFS_MAINTENANCE: LazyInit<UsbMaintenanceRegistry> = LazyInit::new();

/// Discovery output whose device ownership is transferred to its owner thread.
pub(super) struct PendingUsbIrqSlot {
    pub(super) irq: IrqId,
    pub(super) device_id: RDriveDeviceId,
    pub(super) bus_num: u8,
    pub(super) host: ax_driver::usb::UsbHostDevice,
}

#[derive(Clone, Copy)]
struct UsbMaintenanceEvent {
    event: UsbIrqEvent,
    masked: Option<MaskedSource>,
}

/// Generation-bearing owner-local USB device identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct UsbDeviceId(u64);

/// Generation-bearing owner-local endpoint identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct UsbEndpointId(u64);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct UsbTransferId(u64);

type UsbClaimResult = UsbResult<Vec<(u8, UsbEndpointId)>>;

pub(super) struct UsbOpenedDevice {
    pub(super) id: UsbDeviceId,
}

/// Cross-thread observation ticket for a transfer owned by the maintenance thread.
#[derive(Clone)]
pub(super) struct UsbTransferTicket {
    runtime: Arc<UsbHostRuntime>,
    id: UsbTransferId,
    completion: Arc<TransferTicketCompletion>,
}

impl UsbTransferTicket {
    pub(super) fn try_reclaim(&self) -> Result<Option<TransferCompletion>, TransferError> {
        self.completion.try_reclaim()
    }

    pub(super) fn poll_reclaim(
        &self,
        context: &mut Context<'_>,
    ) -> Poll<Result<TransferCompletion, TransferError>> {
        self.completion.poll_reclaim(context)
    }

    pub(super) fn request_cancel(&self) -> Result<(), TransferError> {
        if self.completion.is_terminal() {
            return Ok(());
        }
        let completion = Arc::new(OwnerCompletion::new());
        let request = UsbOwnerRequest::CancelTransfer {
            transfer: self.id,
            completion: completion.clone(),
        };
        self.runtime
            .submit_request(request)
            .map_err(|_| TransferError::NoDevice)?;
        completion.wait()
    }
}

struct TransferTicketCompletion {
    state: SpinNoIrq<TransferTicketState>,
}

struct TransferTicketState {
    terminal: Option<Result<TransferCompletion, TransferError>>,
    waker: Option<Waker>,
}

impl TransferTicketCompletion {
    const fn new() -> Self {
        Self {
            state: SpinNoIrq::new(TransferTicketState {
                terminal: None,
                waker: None,
            }),
        }
    }

    fn complete(&self, terminal: Result<TransferCompletion, TransferError>) {
        let waker = {
            let mut state = self.state.lock();
            debug_assert!(state.terminal.is_none(), "USB transfer completed twice");
            state.terminal = Some(terminal);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn try_reclaim(&self) -> Result<Option<TransferCompletion>, TransferError> {
        match self.state.lock().terminal.take() {
            Some(Ok(completion)) => Ok(Some(completion)),
            Some(Err(error)) => Err(error),
            None => Ok(None),
        }
    }

    fn poll_reclaim(
        &self,
        context: &mut Context<'_>,
    ) -> Poll<Result<TransferCompletion, TransferError>> {
        let mut state = self.state.lock();
        match state.terminal.take() {
            Some(result) => Poll::Ready(result),
            None => {
                let replace = state
                    .waker
                    .as_ref()
                    .is_none_or(|waker| !waker.will_wake(context.waker()));
                if replace {
                    state.waker = Some(context.waker().clone());
                }
                Poll::Pending
            }
        }
    }

    fn is_terminal(&self) -> bool {
        self.state.lock().terminal.is_some()
    }
}

enum UsbOwnerRequest {
    Probe {
        completion: Arc<OwnerCompletion<UsbResult<Vec<ProbedDevice>>>>,
    },
    Open {
        info: DeviceInfo,
        completion: Arc<OwnerCompletion<UsbOpenCompletion>>,
    },
    EnsureConfigured {
        device: UsbDeviceId,
        completion: Arc<OwnerCompletion<UsbResult<()>>>,
    },
    SetConfiguration {
        device: UsbDeviceId,
        configuration: u8,
        completion: Arc<OwnerCompletion<UsbResult<()>>>,
    },
    ClaimInterface {
        device: UsbDeviceId,
        interface: u8,
        alternate: u8,
        completion: Arc<OwnerCompletion<UsbClaimResult>>,
    },
    ReleaseEndpoints {
        endpoints: Vec<UsbEndpointId>,
        completion: Arc<OwnerCompletion<UsbResult<()>>>,
    },
    SubmitEndpoint {
        endpoint: UsbEndpointId,
        request: TransferRequest,
        ticket: Arc<TransferTicketCompletion>,
        completion: Arc<OwnerCompletion<Result<UsbTransferId, TransferError>>>,
    },
    SubmitControl {
        device: UsbDeviceId,
        request: TransferRequest,
        ticket: Arc<TransferTicketCompletion>,
        completion: Arc<OwnerCompletion<Result<UsbTransferId, TransferError>>>,
    },
    CancelTransfer {
        transfer: UsbTransferId,
        completion: Arc<OwnerCompletion<Result<(), TransferError>>>,
    },
}

pub(super) struct UsbOpenCompletion {
    pub(super) info: DeviceInfo,
    pub(super) result: UsbResult<UsbOpenedDevice>,
}

struct UsbOwnedState {
    devices: BTreeMap<UsbDeviceId, Device>,
    endpoints: BTreeMap<UsbEndpointId, OwnedEndpoint>,
    transfers: BTreeMap<UsbTransferId, ActiveTransfer>,
    next_device_generation: u64,
    next_endpoint_generation: u64,
    next_transfer_generation: u64,
    next_transfer_cursor: u64,
    completion_checks_remaining: usize,
}

struct OwnedEndpoint {
    device: UsbDeviceId,
    interface: u8,
    endpoint: Endpoint,
}

struct ActiveTransfer {
    target: ActiveTransferTarget,
    request: RequestId,
    completion: Arc<TransferTicketCompletion>,
}

#[derive(Clone, Copy)]
enum ActiveTransferTarget {
    Endpoint(UsbEndpointId),
    Control(UsbDeviceId),
}

impl UsbOwnedState {
    fn new() -> Self {
        Self {
            devices: BTreeMap::new(),
            endpoints: BTreeMap::new(),
            transfers: BTreeMap::new(),
            next_device_generation: 1,
            next_endpoint_generation: 1,
            next_transfer_generation: 1,
            next_transfer_cursor: 1,
            completion_checks_remaining: 0,
        }
    }

    fn insert_device(&mut self, device: Device) -> UsbResult<UsbDeviceId> {
        let generation =
            take_generation(&mut self.next_device_generation).ok_or(USBError::SlotLimitReached)?;
        let id = UsbDeviceId(generation);
        self.devices.insert(id, device);
        Ok(id)
    }

    fn insert_endpoints(
        &mut self,
        device: UsbDeviceId,
        interface: u8,
        endpoints: BTreeMap<u8, Endpoint>,
    ) -> UsbResult<Vec<(u8, UsbEndpointId)>> {
        let stale = self
            .endpoints
            .iter()
            .filter_map(|(id, endpoint)| {
                (endpoint.device == device && endpoint.interface == interface).then_some(*id)
            })
            .collect::<Vec<_>>();
        if stale.iter().any(|id| self.endpoint_is_active(*id)) {
            return Err(TransferError::QueueFull.into());
        }
        self.remove_endpoints(&stale);

        let mut ids = Vec::with_capacity(endpoints.len());
        for (address, endpoint) in endpoints {
            let generation = take_generation(&mut self.next_endpoint_generation)
                .ok_or(USBError::SlotLimitReached)?;
            let id = UsbEndpointId(generation);
            self.endpoints.insert(
                id,
                OwnedEndpoint {
                    device,
                    interface,
                    endpoint,
                },
            );
            ids.push((address, id));
        }
        Ok(ids)
    }

    fn reserve_transfer_id(&mut self) -> Result<UsbTransferId, TransferError> {
        let generation =
            take_generation(&mut self.next_transfer_generation).ok_or(TransferError::QueueFull)?;
        Ok(UsbTransferId(generation))
    }

    fn insert_transfer(
        &mut self,
        id: UsbTransferId,
        target: ActiveTransferTarget,
        request: RequestId,
        completion: Arc<TransferTicketCompletion>,
    ) {
        self.transfers.insert(
            id,
            ActiveTransfer {
                target,
                request,
                completion,
            },
        );
    }

    fn remove_endpoints(&mut self, endpoints: &[UsbEndpointId]) {
        for endpoint in endpoints {
            self.endpoints.remove(endpoint);
        }
    }

    fn try_remove_endpoints(&mut self, endpoints: &[UsbEndpointId]) -> UsbResult<()> {
        if endpoints.iter().any(|id| self.endpoint_is_active(*id)) {
            return Err(TransferError::QueueFull.into());
        }
        self.remove_endpoints(endpoints);
        Ok(())
    }

    fn try_remove_device_endpoints(&mut self, device: UsbDeviceId) -> UsbResult<()> {
        if self.device_has_active_transfers(device) {
            return Err(TransferError::QueueFull.into());
        }
        self.endpoints
            .retain(|_, endpoint| endpoint.device != device);
        Ok(())
    }

    fn request_completion_scan(&mut self) {
        self.completion_checks_remaining =
            self.completion_checks_remaining.max(self.transfers.len());
    }

    fn endpoint_is_active(&self, endpoint: UsbEndpointId) -> bool {
        self.transfers.values().any(|transfer| {
            matches!(
                transfer.target,
                ActiveTransferTarget::Endpoint(active) if active == endpoint
            )
        })
    }

    fn device_has_active_transfers(&self, device: UsbDeviceId) -> bool {
        self.transfers
            .values()
            .any(|transfer| match transfer.target {
                ActiveTransferTarget::Endpoint(endpoint) => self
                    .endpoints
                    .get(&endpoint)
                    .is_some_and(|owned| owned.device == device),
                ActiveTransferTarget::Control(active) => active == device,
            })
    }

    fn interface_has_active_transfers(&self, device: UsbDeviceId, interface: u8) -> bool {
        self.endpoints.iter().any(|(id, endpoint)| {
            endpoint.device == device
                && endpoint.interface == interface
                && self.endpoint_is_active(*id)
        })
    }

    fn next_transfer_to_check(&self) -> Option<UsbTransferId> {
        let cursor = UsbTransferId(self.next_transfer_cursor);
        self.transfers
            .range(cursor..)
            .next()
            .or_else(|| self.transfers.first_key_value())
            .map(|(id, _)| *id)
    }
}

const fn take_generation(next: &mut u64) -> Option<u64> {
    let generation = *next;
    if generation == 0 {
        return None;
    }
    *next = generation.wrapping_add(1);
    Some(generation)
}

struct OwnerCompletion<T> {
    result: SpinNoIrq<Option<T>>,
    wait: WaitQueue,
}

impl<T> OwnerCompletion<T> {
    const fn new() -> Self {
        Self {
            result: SpinNoIrq::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn complete(&self, result: T) {
        let old = self.result.lock().replace(result);
        debug_assert!(old.is_none(), "USB owner completed one request twice");
        self.wait.notify_all();
    }

    fn wait(&self) -> T {
        self.wait.wait_until(|| self.result.lock().is_some());
        self.result
            .lock()
            .take()
            .unwrap_or_else(|| unreachable!("USB owner completion was published"))
    }
}

struct UsbHostRuntime {
    device_id: RDriveDeviceId,
    requests: SpinNoIrq<VecDeque<UsbOwnerRequest>>,
    maintenance: SpinNoIrq<Option<DeviceMaintenanceHandle<UsbMaintenanceEvent>>>,
    maintenance_thread: SpinNoIrq<Option<MaintenanceThread>>,
    state: AtomicU8,
    state_wait: WaitQueue,
}

impl UsbHostRuntime {
    fn new(device_id: RDriveDeviceId) -> Self {
        Self {
            device_id,
            requests: SpinNoIrq::new(VecDeque::new()),
            maintenance: SpinNoIrq::new(None),
            maintenance_thread: SpinNoIrq::new(None),
            state: AtomicU8::new(OWNER_STARTING),
            state_wait: WaitQueue::new(),
        }
    }

    fn install_maintenance(&self, maintenance: DeviceMaintenanceHandle<UsbMaintenanceEvent>) {
        let old = self.maintenance.lock().replace(maintenance);
        debug_assert!(old.is_none(), "USB maintenance handle installed twice");
    }

    fn install_maintenance_thread(&self, thread: MaintenanceThread) {
        let old = self.maintenance_thread.lock().replace(thread);
        debug_assert!(old.is_none(), "USB maintenance thread installed twice");
    }

    fn mark_ready(&self) {
        self.state.store(OWNER_READY, Ordering::Release);
        self.state_wait.notify_all();
    }

    fn mark_failed(&self) {
        self.state.store(OWNER_FAILED, Ordering::Release);
        self.state_wait.notify_all();
    }

    fn wait_until_started(&self) -> bool {
        self.state_wait
            .wait_until(|| self.state.load(Ordering::Acquire) != OWNER_STARTING);
        self.state.load(Ordering::Acquire) == OWNER_READY
    }

    fn submit_request(&self, request: UsbOwnerRequest) -> Result<(), UsbOwnerRequest> {
        if self.state.load(Ordering::Acquire) != OWNER_READY {
            return Err(request);
        }
        let maintenance = self
            .maintenance
            .lock()
            .as_ref()
            .and_then(|maintenance| maintenance.try_clone_task_context().ok());
        let Some(maintenance) = maintenance else {
            return Err(request);
        };

        let mut requests = self.requests.lock();
        requests.push_back(request);
        let published = maintenance.publish_cause(MaintenanceCauses::SUBMIT);
        if published.is_err() {
            return Err(requests
                .pop_back()
                .unwrap_or_else(|| unreachable!("USB request admission owns the queue tail")));
        }
        Ok(())
    }

    fn pop_request(&self) -> Option<UsbOwnerRequest> {
        self.requests.lock().pop_front()
    }

    fn has_requests(&self) -> bool {
        !self.requests.lock().is_empty()
    }

    fn fail_pending_requests(&self) {
        loop {
            let Some(request) = self.requests.lock().pop_front() else {
                break;
            };
            match request {
                UsbOwnerRequest::Probe { completion } => {
                    completion.complete(Err(owner_stopped_usb_error()));
                }
                UsbOwnerRequest::Open { info, completion } => {
                    completion.complete(UsbOpenCompletion {
                        info,
                        result: Err(owner_stopped_usb_error()),
                    });
                }
                UsbOwnerRequest::EnsureConfigured { completion, .. }
                | UsbOwnerRequest::SetConfiguration { completion, .. }
                | UsbOwnerRequest::ReleaseEndpoints { completion, .. } => {
                    completion.complete(Err(owner_stopped_usb_error()));
                }
                UsbOwnerRequest::ClaimInterface { completion, .. } => {
                    completion.complete(Err(owner_stopped_usb_error()));
                }
                UsbOwnerRequest::SubmitEndpoint {
                    ticket, completion, ..
                }
                | UsbOwnerRequest::SubmitControl {
                    ticket, completion, ..
                } => {
                    ticket.complete(Err(TransferError::NoDevice));
                    completion.complete(Err(TransferError::NoDevice));
                }
                UsbOwnerRequest::CancelTransfer { completion, .. } => {
                    completion.complete(Err(TransferError::NoDevice));
                }
            }
        }
    }
}

struct UsbMaintenanceRegistry {
    hosts: Box<[Arc<UsbHostRuntime>]>,
}

struct UsbOwnerServices<'owner> {
    manager: &'owner UsbFsManager,
    device_id: RDriveDeviceId,
    handler: &'owner LocalOwnerControl<ax_driver::usb::UsbHostIrqHandler>,
    session: &'owner MaintenanceSession<UsbMaintenanceEvent>,
    executor: &'owner LocalExecutor,
}

struct UsbOwnerResources {
    guard: ax_driver::usb::UsbHostDeviceGuard,
    owned: UsbOwnedState,
    handler_cell: core::pin::Pin<Box<LocalOwnerCell<ax_driver::usb::UsbHostIrqHandler>>>,
    handler_control: LocalOwnerControl<ax_driver::usb::UsbHostIrqHandler>,
}

impl UsbMaintenanceRegistry {
    fn find(&self, device_id: RDriveDeviceId) -> Option<Arc<UsbHostRuntime>> {
        self.hosts
            .iter()
            .find(|host| host.device_id == device_id)
            .map(Arc::clone)
    }
}

pub(super) fn manager() -> Option<Arc<UsbFsManager>> {
    USBFS_MANAGER.get().map(Arc::clone)
}

/// Spawns all fixed owners and waits until each host is either Ready or failed.
pub(super) fn init_globals(
    manager: Arc<UsbFsManager>,
    pending_slots: Vec<PendingUsbIrqSlot>,
) -> usize {
    let mut entries = Vec::with_capacity(pending_slots.len());
    for slot in pending_slots {
        let runtime = Arc::new(UsbHostRuntime::new(slot.device_id));
        entries.push((slot, runtime));
    }
    USBFS_MANAGER.init_once(manager.clone());
    USBFS_MAINTENANCE.init_once(UsbMaintenanceRegistry {
        hosts: entries
            .iter()
            .map(|(_, runtime)| Arc::clone(runtime))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    });

    for (slot, runtime) in entries {
        let owner_runtime = Arc::clone(&runtime);
        let owner_manager = manager.clone();
        let owner_name = format!("usb-host-{}", slot.bus_num);
        match spawn_maintenance_domain::<UsbMaintenanceEvent, _>(
            USB_OWNER_CPU,
            owner_name,
            move |registrar| usb_owner_loop(owner_manager, owner_runtime, slot, registrar),
        ) {
            Ok(thread) => runtime.install_maintenance_thread(thread),
            Err(error) => {
                warn!(
                    "usbfs: failed to spawn maintenance owner for host {:?}: {error}",
                    runtime.device_id
                );
                runtime.mark_failed();
            }
        }
    }

    let ready = USBFS_MAINTENANCE
        .get()
        .map(|registry| {
            registry
                .hosts
                .iter()
                .filter(|runtime| runtime.wait_until_started())
                .count()
        })
        .unwrap_or(0);
    info!("usbfs: {ready} host maintenance owner(s) ready");
    ready
}

pub(super) fn probe_host(device_id: RDriveDeviceId) -> UsbResult<Vec<ProbedDevice>> {
    let runtime = runtime_for(device_id)?;
    let completion = Arc::new(OwnerCompletion::new());
    let request = UsbOwnerRequest::Probe {
        completion: completion.clone(),
    };
    runtime
        .submit_request(request)
        .map_err(|_| USBError::NotInitialized)?;
    completion.wait()
}

pub(super) fn open_device(device_id: RDriveDeviceId, info: DeviceInfo) -> UsbOpenCompletion {
    let Some(runtime) = USBFS_MAINTENANCE
        .get()
        .and_then(|registry| registry.find(device_id))
    else {
        return UsbOpenCompletion {
            info,
            result: Err(USBError::NotFound),
        };
    };
    let completion = Arc::new(OwnerCompletion::new());
    let request = UsbOwnerRequest::Open {
        info,
        completion: completion.clone(),
    };
    if let Err(request) = runtime.submit_request(request) {
        let UsbOwnerRequest::Open { info, .. } = request else {
            unreachable!("open admission returns the submitted open request")
        };
        return UsbOpenCompletion {
            info,
            result: Err(USBError::NotInitialized),
        };
    }
    completion.wait()
}

pub(super) fn ensure_configured(
    host_device_id: RDriveDeviceId,
    device: UsbDeviceId,
) -> UsbResult<()> {
    let runtime = runtime_for(host_device_id)?;
    let completion = Arc::new(OwnerCompletion::new());
    runtime
        .submit_request(UsbOwnerRequest::EnsureConfigured {
            device,
            completion: completion.clone(),
        })
        .map_err(|_| USBError::NotInitialized)?;
    completion.wait()
}

pub(super) fn set_configuration(
    host_device_id: RDriveDeviceId,
    device: UsbDeviceId,
    configuration: u8,
) -> UsbResult<()> {
    let runtime = runtime_for(host_device_id)?;
    let completion = Arc::new(OwnerCompletion::new());
    runtime
        .submit_request(UsbOwnerRequest::SetConfiguration {
            device,
            configuration,
            completion: completion.clone(),
        })
        .map_err(|_| USBError::NotInitialized)?;
    completion.wait()
}

pub(super) fn claim_interface(
    host_device_id: RDriveDeviceId,
    device: UsbDeviceId,
    interface: u8,
    alternate: u8,
) -> UsbResult<Vec<(u8, UsbEndpointId)>> {
    let runtime = runtime_for(host_device_id)?;
    let completion = Arc::new(OwnerCompletion::new());
    runtime
        .submit_request(UsbOwnerRequest::ClaimInterface {
            device,
            interface,
            alternate,
            completion: completion.clone(),
        })
        .map_err(|_| USBError::NotInitialized)?;
    completion.wait()
}

pub(super) fn release_endpoints(
    host_device_id: RDriveDeviceId,
    endpoints: Vec<UsbEndpointId>,
) -> UsbResult<()> {
    let runtime = runtime_for(host_device_id)?;
    let completion = Arc::new(OwnerCompletion::new());
    runtime
        .submit_request(UsbOwnerRequest::ReleaseEndpoints {
            endpoints,
            completion: completion.clone(),
        })
        .map_err(|_| USBError::NotInitialized)?;
    completion.wait()
}

pub(super) fn submit_endpoint_transfer(
    host_device_id: RDriveDeviceId,
    endpoint: UsbEndpointId,
    request: TransferRequest,
) -> Result<UsbTransferTicket, TransferError> {
    let runtime = runtime_for(host_device_id).map_err(|_| TransferError::NoDevice)?;
    let ticket = Arc::new(TransferTicketCompletion::new());
    let completion = Arc::new(OwnerCompletion::new());
    runtime
        .submit_request(UsbOwnerRequest::SubmitEndpoint {
            endpoint,
            request,
            ticket: ticket.clone(),
            completion: completion.clone(),
        })
        .map_err(|_| TransferError::NoDevice)?;
    let id = completion.wait()?;
    Ok(UsbTransferTicket {
        runtime,
        id,
        completion: ticket,
    })
}

pub(super) fn submit_control_transfer(
    host_device_id: RDriveDeviceId,
    device: UsbDeviceId,
    request: TransferRequest,
) -> Result<UsbTransferTicket, TransferError> {
    let runtime = runtime_for(host_device_id).map_err(|_| TransferError::NoDevice)?;
    let ticket = Arc::new(TransferTicketCompletion::new());
    let completion = Arc::new(OwnerCompletion::new());
    runtime
        .submit_request(UsbOwnerRequest::SubmitControl {
            device,
            request,
            ticket: ticket.clone(),
            completion: completion.clone(),
        })
        .map_err(|_| TransferError::NoDevice)?;
    let id = completion.wait()?;
    Ok(UsbTransferTicket {
        runtime,
        id,
        completion: ticket,
    })
}

pub(super) fn request_transfer_cancel(ticket: &UsbTransferTicket) -> Result<(), TransferError> {
    ticket.request_cancel()
}

pub(super) fn shutdown_host(device_id: RDriveDeviceId) {
    let runtime = USBFS_MAINTENANCE
        .get()
        .and_then(|registry| registry.find(device_id));
    let Some(runtime) = runtime else {
        return;
    };
    runtime.state.store(OWNER_FAILED, Ordering::Release);
    let maintenance = runtime
        .maintenance
        .lock()
        .as_ref()
        .and_then(|maintenance| maintenance.try_clone_task_context().ok());
    if let Some(maintenance) = maintenance {
        let _result = maintenance.request_shutdown();
    }
}

fn runtime_for(device_id: RDriveDeviceId) -> UsbResult<Arc<UsbHostRuntime>> {
    let runtime = USBFS_MAINTENANCE
        .get()
        .and_then(|registry| registry.find(device_id))
        .ok_or(USBError::NotFound)?;
    if runtime.state.load(Ordering::Acquire) != OWNER_READY {
        return Err(USBError::NotInitialized);
    }
    Ok(runtime)
}

fn usb_owner_loop(
    manager: Arc<UsbFsManager>,
    runtime: Arc<UsbHostRuntime>,
    slot: PendingUsbIrqSlot,
    registrar: MaintenanceRegistrar<UsbMaintenanceEvent>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    let current = current_thread_handle()?;
    let executor = LocalExecutor::new(current.wake_handle())?;
    let mut guard = match slot.host.lock() {
        Ok(guard) => guard,
        Err(error) => {
            warn!(
                "usbfs: owner failed to acquire host {:?}: {error:?}",
                slot.device_id
            );
            runtime.mark_failed();
            return Err(controller_error());
        }
    };
    let Some(handler) = guard.take_event_handler() else {
        warn!(
            "usbfs: host {:?} did not provide one IRQ endpoint",
            slot.device_id
        );
        runtime.mark_failed();
        return Err(controller_error());
    };

    let handler_cell = LocalOwnerCell::pin(handler);
    let (handler_control, mut handler_irq) = registrar
        .local_owner_cell(handler_cell.as_ref())
        .map_err(|error| {
            warn!("usbfs: failed to bind owner-local IRQ endpoint: {error}");
            runtime.mark_failed();
            controller_error()
        })?;
    let irq_wake = registrar.local_irq_wake().inspect_err(|_error| {
        runtime.mark_failed();
    })?;
    let action = registrar.register_shared_disabled(
        format!("usb-host-{}", slot.bus_num),
        slot.irq,
        move |context| usb_irq_action(context, &mut handler_irq, &irq_wake),
    );
    let action = action.inspect_err(|_error| {
        runtime.mark_failed();
    })?;
    runtime.install_maintenance(registrar.remote_handle());
    let session = match registrar.activate() {
        Ok(session) => session,
        Err(error) => {
            runtime.mark_failed();
            if action.disable().is_err() || action.synchronize().is_err() {
                quarantine_unactivated_action(action, "disable or synchronize failed");
            }
            if let Err(failure) = action.close() {
                quarantine_unactivated_action(
                    failure.into_registration(),
                    "registration close failed",
                );
            }
            return Err(error);
        }
    };
    let mut owned = UsbOwnedState::new();
    if let Err(error) = initialize_host(
        &manager,
        &runtime,
        slot.device_id,
        slot.bus_num,
        &mut guard,
        &handler_control,
        &session,
        &executor,
        &action,
    ) {
        warn!(
            "usbfs: failed to initialize host {:?} on bus {}: {error}",
            slot.device_id, slot.bus_num
        );
        runtime.mark_failed();
        runtime.fail_pending_requests();
        return Ok(close_usb_maintenance(
            &manager,
            slot.device_id,
            session,
            action,
            UsbOwnerResources {
                guard,
                owned,
                handler_cell,
                handler_control,
            },
        ));
    }
    runtime.mark_ready();

    let owner_result = 'owner: loop {
        let mut service_error = None;
        let drain = match session.drain_owner(USBFS_EVENT_BATCH_LIMIT, |event| {
            if service_error.is_none()
                && let Err(error) =
                    service_host_events(&manager, slot.device_id, &handler_control, event)
            {
                service_error = Some(error);
            }
        }) {
            Ok(drain) => drain,
            Err(error) => break Err(error),
        };
        if let Some(error) = service_error {
            break Err(error);
        }
        if drain.drained() != 0 {
            owned.request_completion_scan();
        }
        if drain.causes().contains(MaintenanceCauses::OVERFLOW) {
            break Err(controller_error());
        }
        if drain.causes().contains(MaintenanceCauses::SHUTDOWN) {
            break Ok(());
        }

        let mut serviced = drain.drained();
        while serviced < USBFS_EVENT_BATCH_LIMIT {
            let Some(request) = runtime.pop_request() else {
                break;
            };
            serviced += 1;
            let services = UsbOwnerServices {
                manager: &manager,
                device_id: slot.device_id,
                handler: &handler_control,
                session: &session,
                executor: &executor,
            };
            if let Err(error) = service_owner_request(request, &mut guard, &mut owned, &services) {
                break 'owner Err(error);
            }
        }

        let completion_budget = USBFS_EVENT_BATCH_LIMIT.saturating_sub(serviced);
        let completions = service_transfer_completions(&mut owned, completion_budget);
        serviced += completions;

        if drain.pending()
            || runtime.has_requests()
            || owned.completion_checks_remaining != 0
            || serviced == USBFS_EVENT_BATCH_LIMIT
        {
            let _decision = yield_current_cpu();
            continue;
        }
        if let Err(error) = session.wait_for_pending() {
            break Err(error);
        }
    };

    if let Err(error) = owner_result {
        warn!(
            "usbfs: maintenance owner for host {:?} failed: {error}",
            slot.device_id
        );
        runtime.mark_failed();
    }
    runtime.fail_pending_requests();
    drop(executor);
    Ok(close_usb_maintenance(
        &manager,
        slot.device_id,
        session,
        action,
        UsbOwnerResources {
            guard,
            owned,
            handler_cell,
            handler_control,
        },
    ))
}

#[allow(clippy::too_many_arguments)]
fn initialize_host(
    manager: &UsbFsManager,
    _runtime: &UsbHostRuntime,
    device_id: RDriveDeviceId,
    bus_num: u8,
    guard: &mut ax_driver::usb::UsbHostDeviceGuard,
    handler: &LocalOwnerControl<ax_driver::usb::UsbHostIrqHandler>,
    session: &MaintenanceSession<UsbMaintenanceEvent>,
    executor: &LocalExecutor,
    action: &MaintenanceIrqAction,
) -> Result<(), MaintenanceError> {
    info!("usbfs: initializing host on bus {bus_num}");
    let mut completion_event = false;
    run_host_future(
        guard.host_mut().init(),
        executor,
        session,
        handler,
        manager,
        device_id,
        &mut completion_event,
    )?
    .map_err(|error| {
        warn!("usbfs: controller init failed on bus {bus_num}: {error:?}");
        controller_error()
    })?;

    action.enable()?;
    guard.enable_irq().map_err(|error| {
        warn!("usbfs: failed to enable device IRQ on bus {bus_num}: {error:?}");
        controller_error()
    })?;

    manager.begin_initial_probe(device_id);
    let devices = run_host_future(
        guard.host_mut().probe_devices(),
        executor,
        session,
        handler,
        manager,
        device_id,
        &mut completion_event,
    )?
    .map_err(|error| {
        warn!("usbfs: initial probe failed on bus {bus_num}: {error:?}");
        controller_error()
    })?;
    manager.apply_probe_results(device_id, bus_num, devices);
    manager.finish_initial_probe(device_id);
    info!("usbfs: host on bus {bus_num} initialized");
    Ok(())
}

fn service_owner_request(
    request: UsbOwnerRequest,
    guard: &mut ax_driver::usb::UsbHostDeviceGuard,
    owned: &mut UsbOwnedState,
    services: &UsbOwnerServices<'_>,
) -> Result<(), MaintenanceError> {
    let manager = services.manager;
    let device_id = services.device_id;
    let handler = services.handler;
    let session = services.session;
    let executor = services.executor;
    match request {
        UsbOwnerRequest::Probe { completion } => {
            let mut completion_event = false;
            let result = run_host_future(
                guard.host_mut().probe_devices(),
                executor,
                session,
                handler,
                manager,
                device_id,
                &mut completion_event,
            );
            if completion_event {
                owned.request_completion_scan();
            }
            match result {
                Ok(result) => completion.complete(result),
                Err(error) => {
                    completion.complete(Err(owner_stopped_usb_error()));
                    return Err(error);
                }
            }
        }
        UsbOwnerRequest::Open { info, completion } => {
            let mut completion_event = false;
            let result = run_host_future(
                guard.host_mut().open_device(&info),
                executor,
                session,
                handler,
                manager,
                device_id,
                &mut completion_event,
            );
            if completion_event {
                owned.request_completion_scan();
            }
            match result {
                Ok(Ok(device)) => completion.complete(UsbOpenCompletion {
                    info,
                    result: owned.insert_device(device).map(|id| UsbOpenedDevice { id }),
                }),
                Ok(Err(error)) => completion.complete(UsbOpenCompletion {
                    info,
                    result: Err(error),
                }),
                Err(error) => {
                    completion.complete(UsbOpenCompletion {
                        info,
                        result: Err(owner_stopped_usb_error()),
                    });
                    return Err(error);
                }
            }
        }
        UsbOwnerRequest::EnsureConfigured { device, completion } => {
            let Some(live) = owned.devices.get_mut(&device) else {
                completion.complete(Err(USBError::NotFound));
                return Ok(());
            };
            let mut completion_event = false;
            let current = run_host_future(
                live.current_configuration_descriptor(),
                executor,
                session,
                handler,
                manager,
                device_id,
                &mut completion_event,
            );
            if completion_event {
                owned.request_completion_scan();
            }
            let needs_configuration = match current {
                Ok(Ok(_)) => {
                    completion.complete(Ok(()));
                    return Ok(());
                }
                Ok(Err(USBError::NotFound | USBError::ConfigurationNotSet)) => true,
                Ok(Err(error)) => {
                    completion.complete(Err(error));
                    return Ok(());
                }
                Err(error) => {
                    completion.complete(Err(owner_stopped_usb_error()));
                    return Err(error);
                }
            };
            debug_assert!(needs_configuration);
            let configuration = owned
                .devices
                .get(&device)
                .and_then(|live| live.configurations().first())
                .map(|descriptor| descriptor.configuration_value);
            let Some(configuration) = configuration else {
                completion.complete(Err(USBError::NotFound));
                return Ok(());
            };
            let live = owned
                .devices
                .get_mut(&device)
                .expect("owner-local USB device vanished during one request");
            completion_event = false;
            let result = run_host_future(
                live.set_configuration(configuration),
                executor,
                session,
                handler,
                manager,
                device_id,
                &mut completion_event,
            );
            if completion_event {
                owned.request_completion_scan();
            }
            match result {
                Ok(result) => completion.complete(result),
                Err(error) => {
                    completion.complete(Err(owner_stopped_usb_error()));
                    return Err(error);
                }
            }
        }
        UsbOwnerRequest::SetConfiguration {
            device,
            configuration,
            completion,
        } => {
            if owned.device_has_active_transfers(device) {
                completion.complete(Err(TransferError::QueueFull.into()));
                return Ok(());
            }
            let Some(live) = owned.devices.get_mut(&device) else {
                completion.complete(Err(USBError::NotFound));
                return Ok(());
            };
            let mut completion_event = false;
            let result = run_host_future(
                live.set_configuration(configuration),
                executor,
                session,
                handler,
                manager,
                device_id,
                &mut completion_event,
            );
            if completion_event {
                owned.request_completion_scan();
            }
            match result {
                Ok(Ok(())) => completion.complete(owned.try_remove_device_endpoints(device)),
                Ok(Err(error)) => completion.complete(Err(error)),
                Err(error) => {
                    completion.complete(Err(owner_stopped_usb_error()));
                    return Err(error);
                }
            }
        }
        UsbOwnerRequest::ClaimInterface {
            device,
            interface,
            alternate,
            completion,
        } => {
            if owned.interface_has_active_transfers(device, interface) {
                completion.complete(Err(TransferError::QueueFull.into()));
                return Ok(());
            }
            let Some(live) = owned.devices.get_mut(&device) else {
                completion.complete(Err(USBError::NotFound));
                return Ok(());
            };
            let mut completion_event = false;
            let result = run_host_future(
                live.claim_interface(interface, alternate),
                executor,
                session,
                handler,
                manager,
                device_id,
                &mut completion_event,
            );
            if completion_event {
                owned.request_completion_scan();
            }
            match result {
                Ok(Ok(())) => {
                    let endpoints = owned
                        .devices
                        .get_mut(&device)
                        .expect("owner-local USB device vanished during one request")
                        .take_endpoints_for_interface(interface);
                    completion.complete(endpoints.and_then(|endpoints| {
                        owned.insert_endpoints(device, interface, endpoints)
                    }));
                }
                Ok(Err(error)) => completion.complete(Err(error)),
                Err(error) => {
                    completion.complete(Err(owner_stopped_usb_error()));
                    return Err(error);
                }
            }
        }
        UsbOwnerRequest::ReleaseEndpoints {
            endpoints,
            completion,
        } => completion.complete(owned.try_remove_endpoints(&endpoints)),
        UsbOwnerRequest::SubmitEndpoint {
            endpoint,
            request,
            ticket,
            completion,
        } => {
            let id = match owned.reserve_transfer_id() {
                Ok(id) => id,
                Err(error) => {
                    completion.complete(Err(error));
                    return Ok(());
                }
            };
            let Some(endpoint_state) = owned.endpoints.get_mut(&endpoint) else {
                completion.complete(Err(TransferError::InvalidEndpoint));
                return Ok(());
            };
            match endpoint_state.endpoint.submit(request) {
                Ok(request) => {
                    owned.insert_transfer(
                        id,
                        ActiveTransferTarget::Endpoint(endpoint),
                        request,
                        ticket,
                    );
                    completion.complete(Ok(id));
                }
                Err(error) => completion.complete(Err(error)),
            }
        }
        UsbOwnerRequest::SubmitControl {
            device,
            request,
            ticket,
            completion,
        } => {
            let id = match owned.reserve_transfer_id() {
                Ok(id) => id,
                Err(error) => {
                    completion.complete(Err(error));
                    return Ok(());
                }
            };
            let Some(device_state) = owned.devices.get_mut(&device) else {
                completion.complete(Err(TransferError::NoDevice));
                return Ok(());
            };
            match device_state.ctrl_ep_mut().submit(request) {
                Ok(request) => {
                    owned.insert_transfer(
                        id,
                        ActiveTransferTarget::Control(device),
                        request,
                        ticket,
                    );
                    completion.complete(Ok(id));
                }
                Err(error) => completion.complete(Err(error)),
            }
        }
        UsbOwnerRequest::CancelTransfer {
            transfer,
            completion,
        } => {
            let Some(active) = owned.transfers.remove(&transfer) else {
                completion.complete(Err(TransferError::NoDevice));
                return Ok(());
            };
            match cancel_active_transfer(owned, &active) {
                Ok(()) => match reclaim_active_transfer(owned, &active) {
                    Ok(Some(terminal)) => active.completion.complete(Ok(terminal)),
                    Ok(None) => {
                        owned.transfers.insert(transfer, active);
                    }
                    Err(error) => active.completion.complete(Err(error)),
                },
                Err(error) => {
                    owned.transfers.insert(transfer, active);
                    completion.complete(Err(error));
                    return Ok(());
                }
            }
            completion.complete(Ok(()));
        }
    }
    Ok(())
}

fn reclaim_active_transfer(
    owned: &mut UsbOwnedState,
    transfer: &ActiveTransfer,
) -> Result<Option<TransferCompletion>, TransferError> {
    match transfer.target {
        ActiveTransferTarget::Endpoint(endpoint) => owned
            .endpoints
            .get_mut(&endpoint)
            .ok_or(TransferError::NoDevice)?
            .endpoint
            .reclaim(transfer.request),
        ActiveTransferTarget::Control(device) => owned
            .devices
            .get_mut(&device)
            .ok_or(TransferError::NoDevice)?
            .ctrl_ep_mut()
            .reclaim(transfer.request),
    }
}

fn cancel_active_transfer(
    owned: &mut UsbOwnedState,
    transfer: &ActiveTransfer,
) -> Result<(), TransferError> {
    match transfer.target {
        ActiveTransferTarget::Endpoint(endpoint) => owned
            .endpoints
            .get_mut(&endpoint)
            .ok_or(TransferError::NoDevice)?
            .endpoint
            .cancel(transfer.request),
        ActiveTransferTarget::Control(device) => owned
            .devices
            .get_mut(&device)
            .ok_or(TransferError::NoDevice)?
            .ctrl_ep_mut()
            .cancel(transfer.request),
    }
}

/// Reclaims only queue facts made visible by a previously captured IRQ event.
fn service_transfer_completions(owned: &mut UsbOwnedState, limit: usize) -> usize {
    let mut checked = 0;
    while checked < limit && owned.completion_checks_remaining != 0 {
        let Some(id) = owned.next_transfer_to_check() else {
            owned.completion_checks_remaining = 0;
            break;
        };
        let transfer = owned
            .transfers
            .remove(&id)
            .expect("owner-local transfer cursor selected a missing request");
        owned.next_transfer_cursor = id.0.wrapping_add(1).max(1);
        owned.completion_checks_remaining -= 1;
        checked += 1;

        match reclaim_active_transfer(owned, &transfer) {
            Ok(Some(completion)) => transfer.completion.complete(Ok(completion)),
            Ok(None) => {
                owned.transfers.insert(id, transfer);
            }
            Err(error) => transfer.completion.complete(Err(error)),
        }
    }
    checked
}

fn run_host_future<F: Future>(
    future: F,
    executor: &LocalExecutor,
    session: &MaintenanceSession<UsbMaintenanceEvent>,
    handler: &LocalOwnerControl<ax_driver::usb::UsbHostIrqHandler>,
    manager: &UsbFsManager,
    device_id: RDriveDeviceId,
    completion_event: &mut bool,
) -> Result<F::Output, MaintenanceError> {
    executor.try_run(future, |condition| {
        loop {
            let mut service_error = None;
            let drain = match session.drain_owner(USBFS_EVENT_BATCH_LIMIT, |event| {
                if service_error.is_none() {
                    match service_host_events(manager, device_id, handler, event) {
                        Ok(()) => *completion_event = true,
                        Err(error) => service_error = Some(error),
                    }
                }
            }) {
                Ok(drain) => drain,
                Err(error) => return Err(error),
            };
            if let Some(error) = service_error {
                return Err(error);
            }
            if drain.causes().contains(MaintenanceCauses::OVERFLOW) {
                return Err(controller_error());
            }
            if condition.should_abort() {
                return Ok(());
            }
            if drain.pending() {
                let _decision = yield_current_cpu();
                continue;
            }
            session.wait_for_pending_or(|| condition.should_abort())?;
            return Ok(());
        }
    })
}

fn service_host_events(
    manager: &UsbFsManager,
    device_id: RDriveDeviceId,
    handler: &LocalOwnerControl<ax_driver::usb::UsbHostIrqHandler>,
    event: UsbMaintenanceEvent,
) -> Result<(), MaintenanceError> {
    let activity = handler
        .with_owner(|handler| handler.service_host_events(event.event))
        .map_err(|error| {
            warn!("usbfs: owner-local USB event access failed: {error}");
            controller_error()
        })?
        .map_err(|error| {
            warn!("usbfs: USB event service failed: {error}");
            controller_error()
        })?;
    if let Some(masked) = event.masked {
        handler
            .with_owner(|handler| handler.rearm_sources(masked))
            .map_err(|error| {
                warn!("usbfs: owner-local USB rearm access failed: {error}");
                controller_error()
            })?
            .map_err(|error| {
                warn!("usbfs: USB source rearm failed: {error}");
                controller_error()
            })?;
    }
    manager.record_host_event(device_id, activity);
    Ok(())
}

fn usb_irq_action(
    _context: IrqContext,
    endpoint: &mut LocalOwnerIrq<ax_driver::usb::UsbHostIrqHandler>,
    wake: &LocalIrqWake<UsbMaintenanceEvent>,
) -> IrqReturn {
    let capture = match endpoint.with_irq(|handler| handler.capture_irq()) {
        Ok(capture) => capture,
        Err(_) => return IrqReturn::MaskLineAndWake,
    };
    match capture {
        IrqCapture::Unhandled => IrqReturn::Unhandled,
        IrqCapture::Captured { event, masked } => {
            let publication = wake.publish_from_irq(
                MaintenanceCauses::IRQ,
                UsbMaintenanceEvent { event, masked },
            );
            match publication {
                Ok(MaintenancePublishResult::Published) => IrqReturn::Wake,
                Ok(MaintenancePublishResult::Overflowed) => {
                    contain_irq(endpoint, ContainmentCause::PublicationFull)
                }
                Err(LocalIrqWakeError::Closed) => {
                    contain_irq(endpoint, ContainmentCause::PublicationClosed)
                }
                Err(
                    LocalIrqWakeError::OwnerUnavailable { .. }
                    | LocalIrqWakeError::OwnerPlacementMismatch { .. }
                    | LocalIrqWakeError::OwnerIdentityMismatch,
                ) => contain_irq(endpoint, ContainmentCause::OwnerUnavailable),
                Err(LocalIrqWakeError::WrongCpu { .. } | LocalIrqWakeError::NotHardIrq) => {
                    IrqReturn::MaskLineAndWake
                }
            }
        }
        IrqCapture::Fault {
            reason: UsbIrqFault::SourceBusy(_),
            containment: FaultContainment::DeviceSourceMasked(_),
        } => IrqReturn::Handled,
        IrqCapture::Fault { containment, .. } => match containment {
            FaultContainment::DeviceSourceMasked(_) => IrqReturn::DisableActionAndWake,
            FaultContainment::Uncontained => contain_irq(endpoint, ContainmentCause::CaptureFault),
        },
    }
}

fn contain_irq(
    endpoint: &mut LocalOwnerIrq<ax_driver::usb::UsbHostIrqHandler>,
    cause: ContainmentCause,
) -> IrqReturn {
    match endpoint.with_irq(|handler| handler.contain(cause)) {
        Ok(Ok(_)) => IrqReturn::DisableActionAndWake,
        Ok(Err(_)) | Err(_) => IrqReturn::MaskLineAndWake,
    }
}

fn close_usb_maintenance(
    manager: &UsbFsManager,
    device_id: RDriveDeviceId,
    session: MaintenanceSession<UsbMaintenanceEvent>,
    action: MaintenanceIrqAction,
    resources: UsbOwnerResources,
) -> MaintenanceClosed {
    let UsbOwnerResources {
        mut guard,
        mut owned,
        handler_cell,
        handler_control,
    } = resources;
    if session.begin_close().is_err() {
        session.quarantine_and_park();
    }
    if guard.disable_irq().is_err() {
        session.quarantine_and_park();
    }
    if action.disable().is_err() {
        session.quarantine_and_park();
    }
    if action.synchronize().is_err() {
        session.quarantine_and_park();
    }

    if drain_close_evidence(manager, device_id, &session, &handler_control, &mut owned).is_err() {
        session.quarantine_and_park();
    }
    if quiesce_remaining_transfers(&mut owned).is_err() {
        session.quarantine_and_park();
    }

    if let Err(failure) = action.close() {
        let _retained_action = failure.into_registration();
        session.quarantine_and_park();
    }
    if session.try_begin_draining().is_err() {
        session.quarantine_and_park();
    }
    if drain_close_evidence(manager, device_id, &session, &handler_control, &mut owned).is_err() {
        session.quarantine_and_park();
    }
    if session.finish_close().is_err() {
        session.quarantine_and_park();
    }
    let closed = match session.try_into_closed() {
        Ok(closed) => closed,
        Err(failure) => failure.into_session().quarantine_and_park(),
    };
    let _handler = handler_cell
        .reclaim(handler_control, &closed)
        .unwrap_or_else(|_| panic!("closed USB owner retained an IRQ endpoint capability"));
    debug_assert!(owned.transfers.is_empty());
    drop(owned);
    drop(guard);
    closed
}

fn drain_close_evidence(
    manager: &UsbFsManager,
    device_id: RDriveDeviceId,
    session: &MaintenanceSession<UsbMaintenanceEvent>,
    handler: &LocalOwnerControl<ax_driver::usb::UsbHostIrqHandler>,
    owned: &mut UsbOwnedState,
) -> Result<(), MaintenanceError> {
    loop {
        let mut service_error = None;
        let drain = session.drain_owner(USBFS_EVENT_BATCH_LIMIT, |event| {
            if service_error.is_some() {
                return;
            }
            match handler.with_owner(|handler| handler.service_host_events(event.event)) {
                Ok(Ok(activity)) => manager.record_host_event(device_id, activity),
                Ok(Err(_)) | Err(_) => service_error = Some(()),
            }
        })?;
        if service_error.is_some() || drain.causes().contains(MaintenanceCauses::OVERFLOW) {
            return Err(controller_error());
        }
        if drain.drained() != 0 {
            owned.request_completion_scan();
        }
        let checked = service_transfer_completions(owned, USBFS_EVENT_BATCH_LIMIT);
        if !drain.pending() && owned.completion_checks_remaining == 0 {
            return Ok(());
        }
        if checked == USBFS_EVENT_BATCH_LIMIT || drain.pending() {
            let _decision = yield_current_cpu();
        }
    }
}

fn quiesce_remaining_transfers(owned: &mut UsbOwnedState) -> Result<(), TransferError> {
    let transfers = owned.transfers.keys().copied().collect::<Vec<_>>();
    for id in transfers {
        let transfer = owned
            .transfers
            .remove(&id)
            .expect("owner-local close selected a missing USB transfer");
        if let Err(error) = cancel_active_transfer(owned, &transfer) {
            owned.transfers.insert(id, transfer);
            return Err(error);
        }
        match reclaim_active_transfer(owned, &transfer) {
            Ok(Some(completion)) => transfer.completion.complete(Ok(completion)),
            Ok(None) => {
                owned.transfers.insert(id, transfer);
                return Err(TransferError::QueueFull);
            }
            Err(error) => {
                transfer.completion.complete(Err(error));
            }
        }
    }
    Ok(())
}

const fn controller_error() -> MaintenanceError {
    MaintenanceError::Irq(IrqError::Controller)
}

fn owner_stopped_usb_error() -> USBError {
    USBError::from("USB maintenance owner stopped")
}

fn quarantine_unactivated_action(action: MaintenanceIrqAction, reason: &str) -> ! {
    error!("usbfs: retaining an unactivated IRQ action in quarantine: {reason}");
    let _retained_action = action;
    loop {
        let _decision = yield_current_cpu();
    }
}
