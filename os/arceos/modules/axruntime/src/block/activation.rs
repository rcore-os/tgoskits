//! Controller initialization driven by its final CPU-pinned maintenance owner.

mod initialization;

use alloc::{string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_driver::block::RdifBlockDevice;
use initialization::{
    close_failed_registration, close_failed_session, close_owner_resources, drive_init_fsm,
    enable_irq_delivery, register_initial_sources,
};
use rdif_block::{ControllerInitEndpoint, IdList, InitError, MaskedSource};

use super::controller::{
    BlockController, BlockControllerError, OwnerHandoff, OwnerShutdown, OwnerShutdownProgress,
    source::{
        BlockIrqFaultSet, BlockMaintenanceEvent, RuntimeIrqRegistration, RuntimeIrqSource,
        quiesce_after_device_masked,
    },
};
use crate::{
    maintenance::{
        DeviceMaintenanceHandle, MaintenanceCauses, MaintenanceClosed, MaintenanceError,
        MaintenanceRegistrar, MaintenanceSession, spawn_maintenance_domain,
    },
    task::{WaitQueue, yield_current_cpu},
};

const DEFAULT_BLOCK_OWNER_CPU: usize = 0;

/// Runs one pre-publication portable driver transaction without same-CPU IRQ
/// reentry. Published controllers encode the same rule in their endpoint
/// leases; initialization still owns the device directly and uses this gate.
pub(super) fn with_owner_irq_excluded<R>(operation: impl FnOnce() -> R) -> R {
    let irq_guard = ax_kspin::IrqGuard::new();
    let result = operation();
    drop(irq_guard);
    result
}

struct ControllerActivationSlot {
    published: AtomicBool,
    result: ax_kspin::SpinNoPreempt<Option<Result<Arc<BlockController>, BlockControllerError>>>,
    wait: WaitQueue,
}

impl ControllerActivationSlot {
    const fn new() -> Self {
        Self {
            published: AtomicBool::new(false),
            result: ax_kspin::SpinNoPreempt::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn publish(&self, result: Result<Arc<BlockController>, BlockControllerError>) {
        assert!(
            !self.published.swap(true, Ordering::AcqRel),
            "block controller activation published twice"
        );
        let mut slot = self.result.lock();
        assert!(
            slot.is_none(),
            "block controller activation slot is occupied"
        );
        *slot = Some(result);
        drop(slot);
        self.wait.notify_all();
    }

    fn publish_owner_failure(&self, error: MaintenanceError) {
        if self
            .published
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let mut slot = self.result.lock();
        assert!(
            slot.is_none(),
            "unpublished block controller activation slot is occupied"
        );
        *slot = Some(Err(BlockControllerError::Maintenance(error)));
        drop(slot);
        self.wait.notify_all();
    }

    fn wait_result(&self) -> Result<Arc<BlockController>, BlockControllerError> {
        self.wait
            .try_wait_until(|| self.result.lock().is_some())
            .map_err(BlockControllerError::Task)?;
        self.result
            .lock()
            .take()
            .expect("activation result was observed before take")
    }
}

pub(super) fn activate_controller(
    device: RdifBlockDevice,
) -> Result<Arc<BlockController>, BlockControllerError> {
    let name = String::from(device.name());
    let slot = Arc::new(ControllerActivationSlot::new());
    let owner_slot = Arc::clone(&slot);
    let failure_slot = Arc::clone(&slot);
    let thread = spawn_maintenance_domain::<BlockMaintenanceEvent, _>(
        DEFAULT_BLOCK_OWNER_CPU,
        alloc::format!("blk-maint/{name}"),
        move |registrar| {
            let result = run_controller_owner(device, registrar, owner_slot);
            if let Err(error) = result.as_ref() {
                failure_slot.publish_owner_failure(*error);
            }
            result
        },
    )?;
    let controller = slot.wait_result()?;
    controller.install_maintenance_thread(thread);
    Ok(controller)
}

fn run_controller_owner(
    device: RdifBlockDevice,
    registrar: MaintenanceRegistrar<BlockMaintenanceEvent>,
    activation: Arc<ControllerActivationSlot>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    let initialized = match initialize_controller_on_owner(device, registrar)? {
        ControllerInitialization::Ready(owner) => owner,
        ControllerInitialization::Failed { error, closed } => {
            activation.publish(Err(error));
            return Ok(closed);
        }
    };
    let mut initialized = initialized;
    match retire_initial_sources(&mut initialized) {
        Ok(()) => {}
        Err(BindNormalSourcesError::Configuration(error)) => {
            let InitializedControllerOwner {
                device,
                session,
                sources,
                ..
            } = initialized;
            let closed = close_owner_resources(&device, session, sources)?;
            activation.publish(Err(error));
            return Ok(closed);
        }
        Err(BindNormalSourcesError::Close(failure)) => {
            let InitializedControllerOwner {
                device,
                session,
                sources,
                ..
            } = initialized;
            quarantine_unpublished_owner_after_close_failure(device, session, sources, failure);
        }
    }
    let InitializedControllerOwner {
        device,
        session,
        mut sources,
        faults,
        remote,
    } = initialized;
    let mut device = Some(device);
    let prepared = match BlockController::prepare_on_owner(&mut device, remote) {
        Ok(prepared) => prepared,
        Err(error) => {
            let device = device
                .take()
                .expect("failed controller preparation retains its unpublished device owner");
            let closed = close_owner_resources(&device, session, sources)?;
            activation.publish(Err(error));
            return Ok(closed);
        }
    };
    let bound_sources = match bind_normal_sources(
        device
            .as_mut()
            .expect("normal source binding requires its unpublished device owner"),
        &session,
        &mut sources,
        &faults,
        prepared.declared_sources(),
    ) {
        Ok(bound) => bound,
        Err(error) => {
            prepared.abort();
            let device = device
                .take()
                .expect("failed normal source binding retains its unpublished device owner");
            let closed = close_owner_resources(&device, session, sources)?;
            activation.publish(Err(error));
            return Ok(closed);
        }
    };
    let controller = match prepared.commit_on_owner(&mut device, bound_sources) {
        Ok(controller) => controller,
        Err(error) => {
            let device = device
                .take()
                .expect("failed controller build retains its unpublished device owner");
            let closed = close_owner_resources(&device, session, sources)?;
            activation.publish(Err(error));
            return Ok(closed);
        }
    };
    if let Err(error) = enable_runtime_sources(&controller, &sources) {
        let closed = close_controller_resources(&controller, session, sources)?;
        activation.publish(Err(error));
        return Ok(closed);
    }

    activation.publish(Ok(Arc::clone(&controller)));
    run_owner_loop(controller, session, sources, faults)
}

fn retire_initial_sources(
    owner: &mut InitializedControllerOwner,
) -> Result<(), BindNormalSourcesError> {
    if !owner.sources.is_empty() {
        with_owner_irq_excluded(|| owner.device.disable_irq())?;
        quiesce_after_device_masked(&owner.sources)?;
        let initial_sources = core::mem::take(&mut owner.sources);
        close_irq_sources(initial_sources).map_err(BindNormalSourcesError::Close)?;
        drain_retired_initial_events(owner)?;
    }
    Ok(())
}

/// Discards facts captured by the initialization action only after that
/// action is disabled, synchronized, and closed.
///
/// This is the linearization boundary between two action incarnations for the
/// same logical source. Without it, a late initialization event whose local
/// epoch starts at one could be mistaken for the first normal-I/O event after
/// replacement.
fn drain_retired_initial_events(
    owner: &mut InitializedControllerOwner,
) -> Result<(), BindNormalSourcesError> {
    loop {
        let drain = owner
            .session
            .drain_owner(crate::maintenance::MAINTENANCE_BATCH_LIMIT, |_| {})?;
        if drain.causes().contains(MaintenanceCauses::OVERFLOW) {
            return Err(BlockControllerError::Initialization(InitError::Hardware(
                "initialization IRQ mailbox overflowed at the phase boundary",
            ))
            .into());
        }
        if !drain.pending() {
            break;
        }
    }
    if !owner.faults.take().is_empty() {
        return Err(BlockControllerError::Initialization(InitError::Hardware(
            "initialization IRQ capture failed at the phase boundary",
        ))
        .into());
    }
    Ok(())
}

fn bind_normal_sources(
    device: &mut RdifBlockDevice,
    session: &MaintenanceSession<BlockMaintenanceEvent>,
    sources: &mut Vec<RuntimeIrqSource>,
    faults: &Arc<BlockIrqFaultSet>,
    declared: &[rdif_block::IrqSourceInfo],
) -> Result<IdList, BlockControllerError> {
    let mut bound = IdList::none();
    for source_info in declared {
        let source_id = source_info.id;
        let binding = device
            .irq_for_source(source_id)
            .cloned()
            .ok_or(BlockControllerError::MissingIrqBinding(source_id))?;
        let irq = crate::irq::resolve_binding_irq(binding)?;
        let source = device
            .bundle_mut()
            .take_irq_source(source_id)
            .ok_or(BlockControllerError::MissingIrqHandler(source_id))?;
        let wake = session.local_irq_wake()?;
        sources.push(RuntimeIrqSource::register_replacement_disabled(
            session,
            RuntimeIrqRegistration {
                controller_name: device.name().into(),
                source_id,
                irq,
                source,
                wake,
                faults: Arc::clone(faults),
            },
        )?);
        bound.insert(source_id);
    }
    Ok(bound)
}

enum BindNormalSourcesError {
    Configuration(BlockControllerError),
    Close(CloseIrqSourcesFailure),
}

impl From<BlockControllerError> for BindNormalSourcesError {
    fn from(error: BlockControllerError) -> Self {
        Self::Configuration(error)
    }
}

impl From<rdif_block::BlkError> for BindNormalSourcesError {
    fn from(error: rdif_block::BlkError) -> Self {
        Self::Configuration(error.into())
    }
}

impl From<ax_hal::irq::IrqError> for BindNormalSourcesError {
    fn from(error: ax_hal::irq::IrqError) -> Self {
        Self::Configuration(error.into())
    }
}

impl From<MaintenanceError> for BindNormalSourcesError {
    fn from(error: MaintenanceError) -> Self {
        Self::Configuration(error.into())
    }
}

fn enable_runtime_sources(
    controller: &BlockController,
    sources: &[RuntimeIrqSource],
) -> Result<(), BlockControllerError> {
    let mut enabled = 0;
    for source in sources {
        if let Err(error) = source.enable() {
            for rollback in &sources[..enabled] {
                let _ = rollback.disable();
            }
            return Err(error.into());
        }
        enabled += 1;
    }
    if !sources.is_empty()
        && let Err(error) = controller.enable_device_irq_on_owner()
    {
        for source in &sources[..enabled] {
            let _ = source.disable();
        }
        return Err(error);
    }
    Ok(())
}

fn run_owner_loop(
    controller: Arc<BlockController>,
    session: MaintenanceSession<BlockMaintenanceEvent>,
    mut sources: Vec<RuntimeIrqSource>,
    faults: Arc<BlockIrqFaultSet>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    let mut masked = [None; 64];
    let mut handoff = OwnerHandoff::new();
    let mut shutdown = OwnerShutdown::new();
    let mut shutdown_requested = false;
    loop {
        let mut service_error = None;
        let drain =
            match session.drain_owner(crate::maintenance::MAINTENANCE_BATCH_LIMIT, |event| {
                match event {
                    BlockMaintenanceEvent::Irq {
                        source_id,
                        source_epoch,
                        facts,
                        masked: token,
                    } => {
                        if let Err(error) =
                            controller.route_owner_irq(source_id, source_epoch, facts)
                        {
                            service_error = Some(error);
                        }
                        if source_id < masked.len() && token.is_some() {
                            masked[source_id] = token;
                        }
                    }
                    BlockMaintenanceEvent::Fault {
                        source_id,
                        containment,
                        ..
                    } => {
                        if let rdif_block::FaultContainment::DeviceSourceMasked(token) = containment
                            && source_id < masked.len()
                        {
                            masked[source_id] = Some(token);
                        }
                        service_error = Some(super::HardwareQueueError::Offline);
                    }
                }
            }) {
                Ok(drain) => drain,
                Err(error) => quarantine_controller_owner(controller, session, sources, error),
            };
        if !faults.take().is_empty() || drain.causes().contains(MaintenanceCauses::OVERFLOW) {
            service_error = Some(super::HardwareQueueError::Capacity);
        }
        shutdown_requested |= drain.causes().contains(MaintenanceCauses::SHUTDOWN);
        if shutdown_requested {
            match controller.service_owner_shutdown(&sources, &mut shutdown) {
                Ok(OwnerShutdownProgress::Pending { .. }) => {}
                Ok(OwnerShutdownProgress::Complete) => {
                    return close_controller_resources(&controller, session, sources);
                }
                Err(error) => {
                    error!(
                        "block controller {} shutdown failed: {error}",
                        controller.name()
                    );
                    controller.quarantine_queue_endpoints(rdif_block::BlkError::Quarantined);
                    controller.mark_offline();
                    return close_controller_resources(&controller, session, sources);
                }
            }
            if controller.owner_shutdown_is_offline() {
                controller.quarantine_queue_endpoints(rdif_block::BlkError::Quarantined);
                return close_controller_resources(&controller, session, sources);
            }
        }
        let mut irq_ingress_pending = false;
        let mut watchdog_cutoff = None;
        if controller.normal_irq_service_active() {
            let now_ns = ax_hal::time::monotonic_time_nanos();
            if controller
                .next_owner_deadline_ns()
                .is_some_and(|deadline_ns| deadline_ns <= now_ns)
            {
                match faults.try_begin_watchdog_cutoff() {
                    Some(cutoff) => match session.has_irq_pending() {
                        Ok(true) => irq_ingress_pending = true,
                        Ok(false) => {
                            for source in &sources {
                                match source.status() {
                                    Ok(status) => warn!(
                                        "block watchdog reached controller={} source={} \
                                         irq_status={status:?} device_source={:?}",
                                        controller.name(),
                                        source.source_id(),
                                        source.device_state()
                                    ),
                                    Err(error) => warn!(
                                        "block watchdog reached controller={} source={} \
                                         irq_status_error={error} device_source={:?}",
                                        controller.name(),
                                        source.source_id(),
                                        source.device_state()
                                    ),
                                }
                            }
                            controller.raise_due_watchdogs(now_ns);
                            watchdog_cutoff = Some(cutoff);
                        }
                        Err(error) => {
                            drop(cutoff);
                            quarantine_controller_owner(controller, session, sources, error);
                        }
                    }
                    None => irq_ingress_pending = true,
                }
            }
        }
        let mut more = match service_error {
            Some(error) => {
                controller.record_owner_service_failure(&error);
                false
            }
            None => match controller.service_owner_queues() {
                Ok(more) => more,
                Err(error) => {
                    controller.record_owner_service_failure(&error);
                    false
                }
            },
        };
        // `service_owner_queues` either claimed every due timeout under this
        // cutoff or deferred it behind queue-local IRQ evidence. IRQ callbacks
        // remained non-blocking and any event ordered after the cutoff is
        // consumed by the next owner pass as late recovery evidence.
        drop(watchdog_cutoff);
        more |= irq_ingress_pending;
        if controller.normal_irq_service_active() {
            rearm_runtime_sources(&mut sources, &mut masked, &controller);
        }
        more |= controller.service_owner_return(&mut sources);
        match controller.service_owner_recovery(&mut sources, &mut masked) {
            Ok(recovery_more) => more |= recovery_more,
            Err(error) => {
                error!(
                    "block controller {} recovery failed: {error}",
                    controller.name()
                );
                controller.mark_offline();
            }
        }
        more |= controller.service_owner_return(&mut sources);
        more |= controller.service_owner_handoff(&mut sources, &mut handoff);

        if shutdown_requested {
            match controller.service_owner_shutdown(&sources, &mut shutdown) {
                Ok(OwnerShutdownProgress::Pending { run_again }) => more |= run_again,
                Ok(OwnerShutdownProgress::Complete) => {
                    return close_controller_resources(&controller, session, sources);
                }
                Err(error) => {
                    error!(
                        "block controller {} shutdown failed: {error}",
                        controller.name()
                    );
                    controller.quarantine_queue_endpoints(rdif_block::BlkError::Quarantined);
                    controller.mark_offline();
                    return close_controller_resources(&controller, session, sources);
                }
            }
            if controller.owner_shutdown_is_offline() {
                controller.quarantine_queue_endpoints(rdif_block::BlkError::Quarantined);
                return close_controller_resources(&controller, session, sources);
            }
        }
        if more || drain.pending() {
            if let Err(error) = yield_current_cpu() {
                quarantine_controller_owner(controller, session, sources, error.into());
            }
            continue;
        }
        if let Some(deadline) = controller.next_owner_deadline_ns() {
            if let Err(error) = session.wait_for_pending_until(deadline) {
                quarantine_controller_owner(controller, session, sources, error);
            }
        } else if let Err(error) = session.wait_for_pending() {
            quarantine_controller_owner(controller, session, sources, error);
        }
    }
}

fn rearm_runtime_sources(
    sources: &mut [RuntimeIrqSource],
    masked: &mut [Option<MaskedSource>; 64],
    controller: &BlockController,
) {
    for source in sources {
        let source_id = source.source_id();
        let Some(token) = masked.get_mut(source_id).and_then(Option::take) else {
            continue;
        };
        if source.rearm(token).is_err() {
            controller.record_owner_service_failure(&super::HardwareQueueError::Offline);
        }
    }
}

fn quarantine_controller_owner(
    controller: Arc<BlockController>,
    session: MaintenanceSession<BlockMaintenanceEvent>,
    sources: Vec<RuntimeIrqSource>,
    error: MaintenanceError,
) -> ! {
    error!(
        "block controller {} owner failed and will remain CPU-pinned in quarantine: {error}",
        controller.name()
    );
    controller.mark_offline();
    controller.quarantine_queue_endpoints(rdif_block::BlkError::Quarantined);
    let retained_sources = sources;
    match controller.disable_device_irq_on_owner() {
        Ok(()) => {
            if let Err(quiesce_error) = quiesce_after_device_masked(&retained_sources) {
                error!(
                    "block controller {} could not quiesce IRQ actions before owner quarantine: \
                     {quiesce_error:?}",
                    controller.name()
                );
            }
        }
        Err(mask_error) => {
            error!(
                "block controller {} could not mask device IRQs before owner quarantine: \
                 {mask_error}",
                controller.name()
            );
            // Without a device-side mask proof the line quench must remain in
            // force. Disable and drain only the owner action; never reopen a
            // shared backing line around an uncontained source.
            for source in &retained_sources {
                let _ = source.disable();
            }
            for source in &retained_sources {
                let _ = source.synchronize();
            }
        }
    }
    // `retained_sources` remains in this non-returning stack frame. Any action
    // that could not be disabled is still paired with the pinned owner lease;
    // late dispatch observes the closed lifecycle and contains its source.
    session.quarantine_and_park()
}

fn close_controller_resources(
    controller: &BlockController,
    session: MaintenanceSession<BlockMaintenanceEvent>,
    sources: Vec<RuntimeIrqSource>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    if let Err(error) = session.begin_close() {
        error!("block controller close could not cut off publication: {error}");
        session.quarantine_and_park();
    }
    controller.mark_offline();
    if let Err(error) = controller.disable_device_irq_on_owner() {
        error!(
            "block controller {} could not mask device IRQs during close: {error}",
            controller.name()
        );
        controller.quarantine_queue_endpoints(rdif_block::BlkError::Quarantined);
        session.quarantine_and_park();
    }
    if let Err(error) = quiesce_after_device_masked(&sources) {
        error!(
            "block controller {} could not drain IRQ source during close: {error:?}",
            controller.name()
        );
        session.quarantine_and_park();
    }
    if let Err(failure) = close_irq_sources(sources) {
        error!(
            "block controller {} could not close an IRQ action: {:?}",
            controller.name(),
            failure.reason()
        );
        quarantine_source_close_failure(session, failure);
    }
    controller.clear_owner_link_after_drain();
    finish_maintenance_close(session)
}

struct CloseIrqSourcesFailure {
    reason: MaintenanceError,
    _retained: Vec<RuntimeIrqSource>,
}

impl CloseIrqSourcesFailure {
    const fn reason(&self) -> MaintenanceError {
        self.reason
    }
}

fn close_irq_sources(sources: Vec<RuntimeIrqSource>) -> Result<(), CloseIrqSourcesFailure> {
    let mut first_error = None;
    let mut retained = Vec::new();
    for source in sources {
        if let Err(failure) = source.close() {
            let (reason, source) = failure.into_parts();
            first_error.get_or_insert(reason);
            retained.push(source);
        }
    }
    match first_error {
        None => Ok(()),
        Some(reason) => Err(CloseIrqSourcesFailure {
            reason,
            _retained: retained,
        }),
    }
}

fn quarantine_source_close_failure(
    session: MaintenanceSession<BlockMaintenanceEvent>,
    _failure: CloseIrqSourcesFailure,
) -> ! {
    session.quarantine_and_park()
}

fn quarantine_unpublished_owner_after_close_failure(
    _device: RdifBlockDevice,
    session: MaintenanceSession<BlockMaintenanceEvent>,
    _sources: Vec<RuntimeIrqSource>,
    _failure: CloseIrqSourcesFailure,
) -> ! {
    error!("unpublished block owner retained an IRQ action after close failure");
    session.quarantine_and_park()
}

fn finish_maintenance_close(
    session: MaintenanceSession<BlockMaintenanceEvent>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    if let Err(error) = session.try_begin_draining() {
        error!("block maintenance domain could not begin final drain: {error}");
        session.quarantine_and_park();
    }
    loop {
        match session.drain_owner(crate::maintenance::MAINTENANCE_BATCH_LIMIT, |_| {}) {
            Ok(drain) if drain.pending() => {}
            Ok(_) => break,
            Err(error) => {
                error!("block maintenance domain could not drain accepted events: {error}");
                session.quarantine_and_park();
            }
        }
    }
    if let Err(error) = session.finish_close() {
        error!("block maintenance domain could not commit close: {error}");
        session.quarantine_and_park();
    }
    match session.try_into_closed() {
        Ok(closed) => Ok(closed),
        Err(failure) => {
            let error = failure.error();
            error!("block maintenance domain lost its close proof: {error}");
            failure.into_session().quarantine_and_park();
        }
    }
}

/// Live owner state after the portable initialization FSM reaches Ready.
pub(super) struct InitializedControllerOwner {
    pub(super) device: RdifBlockDevice,
    pub(super) session: MaintenanceSession<BlockMaintenanceEvent>,
    pub(super) sources: Vec<RuntimeIrqSource>,
    pub(super) faults: Arc<BlockIrqFaultSet>,
    pub(super) remote: Arc<DeviceMaintenanceHandle<BlockMaintenanceEvent>>,
}

/// Terminal result of owner-thread initialization.
pub(super) enum ControllerInitialization {
    Ready(InitializedControllerOwner),
    Failed {
        error: BlockControllerError,
        closed: MaintenanceClosed,
    },
}

/// Runs discovery-to-ready only after the final owner holds its CPU lease and
/// every declared action has been registered disabled on that same CPU.
pub(super) fn initialize_controller_on_owner(
    mut device: RdifBlockDevice,
    registrar: MaintenanceRegistrar<BlockMaintenanceEvent>,
) -> Result<ControllerInitialization, MaintenanceError> {
    let declared = match device.bundle_mut().controller_init() {
        ControllerInitEndpoint::Ready => IdList::none(),
        ControllerInitEndpoint::Pending(initializer) => initializer.irq_sources(),
    };
    if !matches!(
        device.bundle_mut().controller_init(),
        ControllerInitEndpoint::Ready
    ) && declared.is_empty()
    {
        let error = BlockControllerError::Initialization(InitError::MissingInterrupt);
        return close_failed_registration(device, registrar, Vec::new(), error);
    }

    let faults = Arc::new(BlockIrqFaultSet::new());
    let mut sources =
        match register_initial_sources(&mut device, &registrar, declared, Arc::clone(&faults)) {
            Ok(sources) => sources,
            Err(failure) => {
                let (error, sources) = failure.into_parts();
                return close_failed_registration(device, registrar, sources, error);
            }
        };
    let remote = Arc::new(registrar.remote_handle());
    let session = registrar.activate()?;

    if let Err(error) = enable_irq_delivery(&device, &sources) {
        return close_failed_session(device, session, sources, error);
    }
    if declared.is_empty() {
        return Ok(ControllerInitialization::Ready(
            InitializedControllerOwner {
                device,
                session,
                sources,
                faults,
                remote,
            },
        ));
    }

    let mut pending = IdList::none();
    let mut masked = [None; 64];
    let init_result = drive_init_fsm(
        &mut device,
        &session,
        &mut sources,
        &faults,
        &mut pending,
        &mut masked,
    );
    match init_result {
        Ok(()) => Ok(ControllerInitialization::Ready(
            InitializedControllerOwner {
                device,
                session,
                sources,
                faults,
                remote,
            },
        )),
        Err(error) => close_failed_session(
            device,
            session,
            sources,
            BlockControllerError::Initialization(error),
        ),
    }
}
