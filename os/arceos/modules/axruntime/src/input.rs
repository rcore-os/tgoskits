//! CPU-pinned input owner and immutable event-facade publication.

use alloc::{boxed::Box, string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ax_input::{
    ABS_AXIS_COUNT, INPUT_PROPERTY_COUNT, InputDeviceFacade, InputDeviceSnapshot,
    InputEventPublisher, InputRuntimeService,
};
use ax_lazyinit::LazyInit;
use rdif_input::{
    InputError as RdifInputError, InputExecution, InputIrqFault, Interface, IrqEvent,
};
use rdif_irq::{ContainmentCause, FaultContainment, IrqCapture, MaskedSource};
use thiserror::Error;

use crate::{
    irq::resolve_binding_irq,
    maintenance::{
        DeviceMaintenanceHandle, LocalIrqWake, LocalIrqWakeError, MaintenanceCauses,
        MaintenanceClosed, MaintenanceError, MaintenanceIrqAction, MaintenancePublishResult,
        MaintenanceRegistrar, MaintenanceSession, MaintenanceState, MaintenanceThread,
        spawn_maintenance_domain,
    },
    task::WaitQueue,
};

const INPUT_OWNER_CPU: usize = 0;
const INPUT_BATCH_LIMIT: usize = 64;

/// Input activation failure before a ready facade can be published.
#[derive(Debug, Error)]
pub(crate) enum InputActivationError {
    #[error(transparent)]
    Maintenance(#[from] MaintenanceError),
    #[error("input IRQ binding failed: {0:?}")]
    Irq(irq_framework::IrqError),
    #[error("input driver activation failed: {0}")]
    Driver(RdifInputError),
    #[error("interrupt-driven input has no resolved IRQ binding")]
    MissingIrq,
    #[error("interrupt-driven input did not provide an IRQ endpoint")]
    MissingIrqEndpoint,
}

impl From<irq_framework::IrqError> for InputActivationError {
    fn from(error: irq_framework::IrqError) -> Self {
        Self::Irq(error)
    }
}

impl From<RdifInputError> for InputActivationError {
    fn from(error: RdifInputError) -> Self {
        Self::Driver(error)
    }
}

#[derive(Clone, Copy, Debug)]
enum InputMaintenanceEvent {
    Irq {
        event: IrqEvent,
        masked: Option<MaskedSource>,
    },
    Fault {
        reason: InputIrqFault,
        masked: Option<MaskedSource>,
    },
}

struct InputActivationSlot {
    result: ax_kspin::SpinNoIrq<Option<Result<InputDeviceSnapshot, InputActivationError>>>,
    wait: WaitQueue,
}

impl InputActivationSlot {
    const fn new() -> Self {
        Self {
            result: ax_kspin::SpinNoIrq::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn publish(&self, result: Result<InputDeviceSnapshot, InputActivationError>) {
        let mut slot = self.result.lock();
        if slot.is_some() {
            return;
        }
        *slot = Some(result);
        drop(slot);
        self.wait.notify_all();
    }

    fn publish_owner_failure(&self, error: MaintenanceError) {
        self.publish(Err(InputActivationError::Maintenance(error)));
    }

    fn wait_result(&self) -> Result<InputDeviceSnapshot, InputActivationError> {
        self.wait
            .try_wait_until(|| self.result.lock().is_some())
            .map_err(MaintenanceError::from)?;
        self.result
            .lock()
            .take()
            .expect("input activation result disappeared after publication")
    }
}

struct InputRemote {
    maintenance: LazyInit<DeviceMaintenanceHandle<InputMaintenanceEvent>>,
    maintenance_thread: LazyInit<MaintenanceThread>,
}

impl InputRemote {
    const fn new() -> Self {
        Self {
            maintenance: LazyInit::new(),
            maintenance_thread: LazyInit::new(),
        }
    }

    fn install_maintenance(&self, maintenance: DeviceMaintenanceHandle<InputMaintenanceEvent>) {
        self.maintenance.init_once(maintenance);
    }

    fn install_thread(&self, thread: MaintenanceThread) {
        self.maintenance_thread.init_once(thread);
    }
}

impl InputRuntimeService for InputRemote {
    fn request_shutdown(&self) -> ax_input::InputResult {
        if !self.maintenance.is_inited() {
            return Err(ax_input::InputError::NotAvailable);
        }
        self.maintenance
            .request_shutdown()
            .map_err(|_| ax_input::InputError::BadState)
    }
}

struct InputIrqState {
    masked_generation: AtomicU64,
    masked_bitmap: AtomicU64,
    line_quenched: AtomicBool,
}

impl InputIrqState {
    const fn new() -> Self {
        Self {
            masked_generation: AtomicU64::new(0),
            masked_bitmap: AtomicU64::new(0),
            line_quenched: AtomicBool::new(false),
        }
    }

    fn record_masked(&self, source: MaskedSource) {
        self.masked_bitmap
            .store(source.bitmap().get(), Ordering::Relaxed);
        self.masked_generation
            .store(source.generation().get(), Ordering::Release);
    }

    fn take_masked(&self) -> Option<MaskedSource> {
        let generation = self.masked_generation.swap(0, Ordering::AcqRel);
        if generation == 0 {
            return None;
        }
        let bitmap = self.masked_bitmap.swap(0, Ordering::Relaxed);
        MaskedSource::try_new(generation, bitmap).ok()
    }
}

/// Activates one discovered driver and returns only its immutable facade.
pub(crate) fn activate_input(
    taken: ax_driver::input::TakenInputDevice,
) -> Result<InputDeviceFacade, InputActivationError> {
    let discovered_name = String::from(taken.device.name());
    let irq = taken.irq.map(resolve_binding_irq).transpose()?;
    let channel = InputDeviceFacade::event_channel();
    let (publisher, receiver) = channel.split();
    let remote = Arc::new(InputRemote::new());
    let activation = Arc::new(InputActivationSlot::new());
    let owner_activation = Arc::clone(&activation);
    let failure_activation = Arc::clone(&activation);
    let owner_remote = Arc::clone(&remote);
    let thread = spawn_maintenance_domain::<InputMaintenanceEvent, _>(
        INPUT_OWNER_CPU,
        alloc::format!("input-maint/{discovered_name}"),
        move |registrar| {
            let result = run_input_owner(
                taken.device,
                irq,
                publisher,
                owner_remote,
                owner_activation,
                registrar,
            );
            if let Err(error) = result.as_ref() {
                failure_activation.publish_owner_failure(*error);
            }
            result
        },
    )?;
    let snapshot = activation.wait_result()?;
    remote.install_thread(thread);
    let service: Arc<dyn InputRuntimeService> = remote;
    Ok(InputDeviceFacade::new(snapshot, receiver, service))
}

fn run_input_owner(
    mut device: Box<dyn Interface>,
    irq: Option<irq_framework::IrqId>,
    publisher: InputEventPublisher,
    remote: Arc<InputRemote>,
    activation: Arc<InputActivationSlot>,
    registrar: MaintenanceRegistrar<InputMaintenanceEvent>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    let maintenance = registrar.remote_handle();
    let irq_state = Arc::new(InputIrqState::new());
    let registration = match prepare_input_irq_action(&mut *device, irq, &irq_state, &registrar) {
        Ok(registration) => registration,
        Err(error) => {
            let session = registrar.activate()?;
            activation.publish(Err(error));
            return close_input_owner(device, publisher, remote, &irq_state, session, None);
        }
    };
    let session = match registrar.activate() {
        Ok(session) => session,
        Err(error) => {
            if let Some(registration) = registration
                && let Err(failure) = registration.close()
            {
                warn!(
                    "failed to close input IRQ after activation failure: {}",
                    failure.reason()
                );
            }
            return Err(error);
        }
    };

    let snapshot = match initialize_input_owner(&mut *device, registration.as_ref()) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            activation.publish(Err(error));
            return close_input_owner(device, publisher, remote, &irq_state, session, registration);
        }
    };
    remote.install_maintenance(maintenance);
    activation.publish(Ok(snapshot));

    let owner_result = input_owner_loop(
        &mut *device,
        &publisher,
        &irq_state,
        &session,
        registration.as_ref(),
    );
    if let Err(error) = owner_result {
        warn!("input maintenance owner entered contained shutdown: {error}");
    }
    close_input_owner(device, publisher, remote, &irq_state, session, registration)
}

fn prepare_input_irq_action(
    device: &mut dyn Interface,
    irq: Option<irq_framework::IrqId>,
    irq_state: &Arc<InputIrqState>,
    registrar: &MaintenanceRegistrar<InputMaintenanceEvent>,
) -> Result<Option<MaintenanceIrqAction>, InputActivationError> {
    match device.execution() {
        InputExecution::Inline => Ok(None),
        InputExecution::Interrupt => {
            let irq = irq.ok_or(InputActivationError::MissingIrq)?;
            let mut endpoint = device
                .take_irq_endpoint()
                .ok_or(InputActivationError::MissingIrqEndpoint)?;
            let wake = registrar.local_irq_wake()?;
            let owner_cpu = registrar.owner_cpu();
            let irq_state = Arc::clone(irq_state);
            let action = registrar.register_shared_disabled(
                alloc::format!("{}/input", device.name()),
                irq,
                move |context| {
                    input_irq_action(context.cpu.0, owner_cpu, &irq_state, &wake, &mut *endpoint)
                },
            )?;
            Ok(Some(action))
        }
    }
}

fn initialize_input_owner(
    device: &mut dyn Interface,
    action: Option<&MaintenanceIrqAction>,
) -> Result<InputDeviceSnapshot, InputActivationError> {
    device.initialize()?;
    let snapshot = snapshot_input(device);
    if let Some(action) = action {
        action.enable()?;
        if let Err(error) = device.enable_irq() {
            let _ = action.disable();
            return Err(InputActivationError::Driver(error));
        }
    }
    Ok(snapshot)
}

fn input_irq_action(
    actual_cpu: usize,
    owner_cpu: usize,
    irq_state: &InputIrqState,
    wake: &LocalIrqWake<InputMaintenanceEvent>,
    endpoint: &mut dyn rdif_irq::IrqEndpoint<Event = IrqEvent, Fault = InputIrqFault>,
) -> irq_framework::IrqReturn {
    if actual_cpu != owner_cpu {
        return contain_input_irq(endpoint, irq_state, ContainmentCause::OwnerUnavailable);
    }
    match endpoint.capture() {
        IrqCapture::Unhandled => irq_framework::IrqReturn::Unhandled,
        IrqCapture::Captured { event, masked } => {
            let publication = wake.publish_from_irq(
                MaintenanceCauses::IRQ,
                InputMaintenanceEvent::Irq { event, masked },
            );
            match publication {
                Ok(MaintenancePublishResult::Published) => irq_framework::IrqReturn::Wake,
                Ok(MaintenancePublishResult::Overflowed) => {
                    contain_input_irq(endpoint, irq_state, ContainmentCause::PublicationFull)
                }
                Err(error) => contain_input_irq(endpoint, irq_state, containment_cause(error)),
            }
        }
        IrqCapture::Fault {
            reason,
            containment,
        } => {
            let masked = match containment {
                FaultContainment::DeviceSourceMasked(source) => Some(source),
                FaultContainment::Uncontained => None,
            };
            let publication = wake.publish_from_irq(
                MaintenanceCauses::IRQ,
                InputMaintenanceEvent::Fault { reason, masked },
            );
            record_unpublished_fault_containment(irq_state, &publication, containment);
            match containment {
                FaultContainment::DeviceSourceMasked(_) => {
                    irq_framework::IrqReturn::DisableActionAndWake
                }
                FaultContainment::Uncontained => irq_framework::IrqReturn::MaskLineAndWake,
            }
        }
    }
}

fn record_unpublished_fault_containment(
    irq_state: &InputIrqState,
    publication: &Result<MaintenancePublishResult, LocalIrqWakeError>,
    containment: FaultContainment,
) {
    match containment {
        FaultContainment::DeviceSourceMasked(source) => {
            let event_owns_source = matches!(
                publication,
                Ok(MaintenancePublishResult::Published)
                    | Err(LocalIrqWakeError::OwnerUnavailable {
                        publication: MaintenancePublishResult::Published,
                        ..
                    })
            );
            if !event_owns_source {
                irq_state.record_masked(source);
            }
        }
        FaultContainment::Uncontained => {
            // `MaskLineAndWake` transfers a controller-line quench to this
            // owner independently of whether the diagnostic event fitted in
            // the mailbox. The close transaction must always release it after
            // disabling and synchronizing this action, otherwise an unrelated
            // peer on the shared line remains permanently masked.
            irq_state.line_quenched.store(true, Ordering::Release);
        }
    }
}

fn containment_cause(error: LocalIrqWakeError) -> ContainmentCause {
    match error {
        LocalIrqWakeError::Closed => ContainmentCause::PublicationClosed,
        LocalIrqWakeError::NotHardIrq
        | LocalIrqWakeError::WrongCpu { .. }
        | LocalIrqWakeError::OwnerIdentityMismatch
        | LocalIrqWakeError::OwnerPlacementMismatch { .. }
        | LocalIrqWakeError::OwnerUnavailable { .. } => ContainmentCause::OwnerUnavailable,
    }
}

fn contain_input_irq(
    endpoint: &mut dyn rdif_irq::IrqEndpoint<Event = IrqEvent, Fault = InputIrqFault>,
    irq_state: &InputIrqState,
    cause: ContainmentCause,
) -> irq_framework::IrqReturn {
    match endpoint.contain(cause) {
        Ok(source) => {
            irq_state.record_masked(source);
            irq_framework::IrqReturn::DisableActionAndWake
        }
        Err(_) => {
            irq_state.line_quenched.store(true, Ordering::Release);
            irq_framework::IrqReturn::MaskLineAndWake
        }
    }
}

fn ensure_input_line_serviceable(irq_state: &InputIrqState) -> Result<(), MaintenanceError> {
    if irq_state.line_quenched.load(Ordering::Acquire) {
        // A complete-line quench means the endpoint could not prove that its
        // exact source was quiet. The normal service loop must never reopen
        // that line: only close/recovery may do so after task-context source
        // masking and action synchronization.
        return Err(MaintenanceError::Irq(irq_framework::IrqError::Controller));
    }
    Ok(())
}

fn input_owner_loop(
    device: &mut dyn Interface,
    publisher: &InputEventPublisher,
    irq_state: &InputIrqState,
    session: &MaintenanceSession<InputMaintenanceEvent>,
    action: Option<&MaintenanceIrqAction>,
) -> Result<(), MaintenanceError> {
    let mut continuation = false;
    let mut pending_masked = None;
    loop {
        if !continuation {
            session.wait_for_pending()?;
        }
        let mut input_ready = continuation;
        let mut fault = None;
        let drain = session.drain_owner(INPUT_BATCH_LIMIT, |event| match event {
            InputMaintenanceEvent::Irq { event, masked } => {
                input_ready |= event.input_ready;
                pending_masked = pending_masked.or(masked);
            }
            InputMaintenanceEvent::Fault { reason, masked } => {
                pending_masked = pending_masked.or(masked);
                fault = Some(reason);
            }
        })?;
        let causes = drain.causes();
        if causes.contains(MaintenanceCauses::SHUTDOWN) {
            return Ok(());
        }
        ensure_input_line_serviceable(irq_state)?;
        if causes.contains(MaintenanceCauses::OVERFLOW) {
            input_ready = true;
        }
        if let Some(reason) = fault {
            warn!("input IRQ capture failed: {reason}");
            return Err(MaintenanceError::Irq(irq_framework::IrqError::Controller));
        }

        continuation = false;
        if input_ready {
            continuation = drain_input_events(device, publisher).map_err(|error| {
                warn!("input event service failed: {error}");
                MaintenanceError::Irq(irq_framework::IrqError::Controller)
            })?;
        }

        pending_masked = pending_masked.or_else(|| irq_state.take_masked());
        if !continuation && let Some(source) = pending_masked.take() {
            let action =
                action.ok_or(MaintenanceError::Irq(irq_framework::IrqError::Controller))?;
            action.enable()?;
            device.rearm_irq(source).map_err(|error| {
                warn!("input source rearm failed: {error}");
                MaintenanceError::Irq(irq_framework::IrqError::Controller)
            })?;
        }
        if drain.pending() || continuation {
            crate::task::yield_current_cpu()?;
        }
    }
}

fn drain_input_events(
    device: &mut dyn Interface,
    publisher: &InputEventPublisher,
) -> Result<bool, RdifInputError> {
    let mut events = [None; INPUT_BATCH_LIMIT];
    let mut count = 0;
    while count < events.len() {
        match device.read_event() {
            Ok(event) => {
                events[count] = Some(map_input_event(event));
                count += 1;
            }
            Err(RdifInputError::Again) => break,
            Err(error) => return Err(error),
        }
    }
    let mut ready = [ax_input::Event {
        event_type: 0,
        code: 0,
        value: 0,
    }; INPUT_BATCH_LIMIT];
    for (destination, event) in ready.iter_mut().zip(events.into_iter().flatten()) {
        *destination = event;
    }
    let publication = publisher.publish(&ready[..count]);
    if publication.dropped() != 0 {
        warn!(
            "input facade dropped {} oldest events under backpressure",
            publication.dropped()
        );
    }
    Ok(count == INPUT_BATCH_LIMIT)
}

fn close_input_owner(
    mut device: Box<dyn Interface>,
    publisher: InputEventPublisher,
    remote: Arc<InputRemote>,
    irq_state: &InputIrqState,
    session: MaintenanceSession<InputMaintenanceEvent>,
    action: Option<MaintenanceIrqAction>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    if let Some(action) = action {
        if let Err(error) = device.disable_irq() {
            warn!("failed to mask input source during close: {error}");
            let _retained = (device, publisher, remote, action);
            session.quarantine_and_park();
        }
        if let Err(error) = action.disable().and_then(|()| action.synchronize()) {
            warn!("failed to disable input IRQ action during close: {error}");
            let _retained = (device, publisher, remote, action);
            session.quarantine_and_park();
        }
        if irq_state.line_quenched.swap(false, Ordering::AcqRel)
            && let Err(error) = action.release_quench()
        {
            warn!("failed to release contained input IRQ line during close: {error}");
            let _retained = (device, publisher, remote, action);
            session.quarantine_and_park();
        }
        if let Err(failure) = action.close() {
            let (reason, action) = failure.into_parts();
            warn!("failed to destroy input IRQ action: {reason}");
            let _retained = (device, publisher, remote, action);
            session.quarantine_and_park();
        }
    }
    session.begin_close()?;
    while session.state() == MaintenanceState::Closing {
        let drain = session.drain_owner(INPUT_BATCH_LIMIT, |_| {})?;
        if !drain.pending() {
            break;
        }
    }
    session.try_begin_draining()?;
    session.finish_close()?;
    session.try_into_closed().map_err(|failure| failure.error())
}

fn snapshot_input(device: &mut dyn Interface) -> InputDeviceSnapshot {
    let mut event_bits = core::array::from_fn(|_| Vec::new());
    for index in 0..ax_input::EventType::COUNT {
        let Some(event_type) = ax_input::EventType::from_repr(index) else {
            continue;
        };
        let mut bits = vec![0; event_type.bits_count().div_ceil(8)];
        match device.get_event_bits(map_event_type(event_type), &mut bits) {
            Ok(true) => event_bits[index as usize] = bits,
            Ok(false) => {}
            Err(error) => warn!("failed to snapshot {event_type:?} input bits: {error}"),
        }
    }

    let mut property_bits = vec![0; INPUT_PROPERTY_COUNT.div_ceil(8)];
    if let Err(error) = device.get_prop_bits(&mut property_bits) {
        warn!("failed to snapshot input property bits: {error}");
        property_bits.fill(0);
    }

    let absolute_bits = &event_bits[ax_input::EventType::Absolute as usize];
    let mut absolute_info = [None; ABS_AXIS_COUNT];
    for (axis, slot) in absolute_info.iter_mut().enumerate() {
        if absolute_bits
            .get(axis / 8)
            .is_none_or(|byte| byte & (1 << (axis % 8)) == 0)
        {
            continue;
        }
        match device.get_abs_info(axis as u8) {
            Ok(info) => *slot = Some(map_abs_info(info)),
            Err(error) => warn!("failed to snapshot input axis {axis}: {error}"),
        }
    }

    InputDeviceSnapshot::new(
        String::from(device.name()),
        String::from(device.physical_location()),
        String::from(device.unique_id()),
        map_device_id(device.device_id()),
        event_bits,
        property_bits,
        absolute_info,
    )
}

fn map_event_type(event_type: ax_input::EventType) -> rdif_input::EventType {
    match event_type {
        ax_input::EventType::Synchronization => rdif_input::EventType::Synchronization,
        ax_input::EventType::Key => rdif_input::EventType::Key,
        ax_input::EventType::Relative => rdif_input::EventType::Relative,
        ax_input::EventType::Absolute => rdif_input::EventType::Absolute,
        ax_input::EventType::Misc => rdif_input::EventType::Misc,
        ax_input::EventType::Switch => rdif_input::EventType::Switch,
        ax_input::EventType::Led => rdif_input::EventType::Led,
        ax_input::EventType::Sound => rdif_input::EventType::Sound,
        ax_input::EventType::ForceFeedback => rdif_input::EventType::ForceFeedback,
    }
}

fn map_input_event(event: rdif_input::InputEvent) -> ax_input::Event {
    ax_input::Event {
        event_type: event.event_type,
        code: event.code,
        value: event.value,
    }
}

fn map_abs_info(info: rdif_input::AbsInfo) -> ax_input::AbsInfo {
    ax_input::AbsInfo {
        min: info.min,
        max: info.max,
        fuzz: info.fuzz,
        flat: info.flat,
        res: info.res,
    }
}

fn map_device_id(id: rdif_input::InputDeviceId) -> ax_input::InputDeviceId {
    ax_input::InputDeviceId {
        bus_type: id.bus_type,
        vendor: id.vendor,
        product: id.product,
        version: id.version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masked_source() -> MaskedSource {
        MaskedSource::try_new(7, 1).unwrap()
    }

    #[test]
    fn published_fault_token_has_one_owner_and_fallback_is_consumed_once() {
        let published = InputIrqState::new();
        record_unpublished_fault_containment(
            &published,
            &Ok(MaintenancePublishResult::Published),
            FaultContainment::DeviceSourceMasked(masked_source()),
        );
        assert_eq!(published.take_masked(), None);

        let overflowed = InputIrqState::new();
        record_unpublished_fault_containment(
            &overflowed,
            &Ok(MaintenancePublishResult::Overflowed),
            FaultContainment::DeviceSourceMasked(masked_source()),
        );
        assert_eq!(overflowed.take_masked(), Some(masked_source()));
        assert_eq!(overflowed.take_masked(), None);

        let closed = InputIrqState::new();
        record_unpublished_fault_containment(
            &closed,
            &Err(LocalIrqWakeError::Closed),
            FaultContainment::DeviceSourceMasked(masked_source()),
        );
        assert_eq!(closed.take_masked(), Some(masked_source()));
        assert_eq!(closed.take_masked(), None);
    }

    #[test]
    fn published_uncontained_fault_retains_line_quench_for_owner_close() {
        let state = InputIrqState::new();
        record_unpublished_fault_containment(
            &state,
            &Ok(MaintenancePublishResult::Published),
            FaultContainment::Uncontained,
        );

        assert!(state.line_quenched.load(Ordering::Acquire));
        assert!(ensure_input_line_serviceable(&state).is_err());
    }
}
