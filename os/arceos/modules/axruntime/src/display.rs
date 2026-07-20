//! CPU-pinned display owner and immutable framebuffer publication.

use alloc::{string::String, sync::Arc};
use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use ax_display::{DisplayFacade, DisplayFlushService};
use ax_driver::display::TakenDisplayDevice;
use ax_lazyinit::LazyInit;
use rdif_display::{
    DisplayError as RdifDisplayError, DisplayExecution, DisplayInfo as RdifDisplayInfo,
    DisplayIrqFault, Event as DisplayIrqEvent, Interface, PixelFormat as RdifPixelFormat,
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

const DISPLAY_OWNER_CPU: usize = 0;
const DISPLAY_COMPLETION_CAPACITY: usize = 64;
const DISPLAY_BATCH_LIMIT: usize = 64;

const COMPLETION_FREE: u8 = 0;
const COMPLETION_PENDING: u8 = 1;
const COMPLETION_OK: u8 = 2;
const COMPLETION_UNSUPPORTED: u8 = 3;
const COMPLETION_UNAVAILABLE: u8 = 4;
const COMPLETION_INVALID_FRAMEBUFFER: u8 = 5;
const COMPLETION_BAD_STATE: u8 = 6;

const IRQ_FALLBACK_EMPTY: u8 = 0;
const IRQ_FALLBACK_WRITING: u8 = 1;
const IRQ_FALLBACK_READY: u8 = 2;
const IRQ_FALLBACK_READING: u8 = 3;

/// Display activation failure before an immutable facade can be published.
#[derive(Debug, Error)]
pub(crate) enum DisplayActivationError {
    #[error(transparent)]
    Maintenance(#[from] MaintenanceError),
    #[error("display IRQ binding failed: {0:?}")]
    Irq(irq_framework::IrqError),
    #[error("display driver activation failed: {0}")]
    Driver(#[from] RdifDisplayError),
    #[error("interrupt-driven display has no resolved IRQ binding")]
    MissingIrq,
    #[error("interrupt-driven display did not provide an IRQ endpoint")]
    MissingIrqEndpoint,
    #[error("display framebuffer is shorter than its published layout")]
    InvalidFramebuffer,
}

impl From<irq_framework::IrqError> for DisplayActivationError {
    fn from(error: irq_framework::IrqError) -> Self {
        Self::Irq(error)
    }
}

#[derive(Clone, Copy, Debug)]
enum DisplayMaintenanceEvent {
    Irq {
        event: DisplayIrqEvent,
        masked: Option<MaskedSource>,
    },
    Fault {
        reason: DisplayIrqFault,
        masked: Option<MaskedSource>,
    },
    Flush {
        generation: u64,
    },
}

struct DisplayIrqFallback {
    state: AtomicU8,
    generation: AtomicU64,
    bitmap: AtomicU64,
    conflicted: AtomicU8,
    line_quenched: AtomicU8,
}

impl DisplayIrqFallback {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(IRQ_FALLBACK_EMPTY),
            generation: AtomicU64::new(0),
            bitmap: AtomicU64::new(0),
            conflicted: AtomicU8::new(0),
            line_quenched: AtomicU8::new(0),
        }
    }

    fn record_from_irq(&self, source: MaskedSource) -> DisplayIrqFallbackRecord {
        match self.state.compare_exchange(
            IRQ_FALLBACK_EMPTY,
            IRQ_FALLBACK_WRITING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.generation
                    .store(source.generation().get(), Ordering::Relaxed);
                self.bitmap.store(source.bitmap().get(), Ordering::Relaxed);
                self.state.store(IRQ_FALLBACK_READY, Ordering::Release);
                DisplayIrqFallbackRecord::Stored
            }
            Err(IRQ_FALLBACK_READY)
                if self.generation.load(Ordering::Acquire) == source.generation().get()
                    && self.bitmap.load(Ordering::Acquire) == source.bitmap().get() =>
            {
                DisplayIrqFallbackRecord::Coalesced
            }
            Err(_) => {
                self.conflicted.store(1, Ordering::Release);
                DisplayIrqFallbackRecord::Conflict
            }
        }
    }

    fn take_owner(&self) -> DisplayIrqFallbackTake {
        if self.conflicted.load(Ordering::Acquire) != 0 {
            return DisplayIrqFallbackTake::Conflict;
        }
        if self
            .state
            .compare_exchange(
                IRQ_FALLBACK_READY,
                IRQ_FALLBACK_READING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return DisplayIrqFallbackTake::Empty;
        }
        let generation = self.generation.load(Ordering::Relaxed);
        let bitmap = self.bitmap.load(Ordering::Relaxed);
        self.state.store(IRQ_FALLBACK_EMPTY, Ordering::Release);
        match MaskedSource::try_new(generation, bitmap) {
            Ok(source) => DisplayIrqFallbackTake::Source(source),
            Err(_) => {
                self.conflicted.store(1, Ordering::Release);
                DisplayIrqFallbackTake::Conflict
            }
        }
    }

    fn discard_after_irq_shutdown(&self) {
        self.generation.store(0, Ordering::Relaxed);
        self.bitmap.store(0, Ordering::Relaxed);
        self.conflicted.store(0, Ordering::Relaxed);
        self.line_quenched.store(0, Ordering::Relaxed);
        self.state.store(IRQ_FALLBACK_EMPTY, Ordering::Release);
    }

    fn record_line_quench_from_irq(&self) {
        self.line_quenched.store(1, Ordering::Release);
    }

    fn take_line_quench_owner(&self) -> bool {
        self.line_quenched.swap(0, Ordering::AcqRel) != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayIrqFallbackRecord {
    Stored,
    Coalesced,
    Conflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayIrqFallbackTake {
    Empty,
    Source(MaskedSource),
    Conflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContainedTokenRoute {
    Sideband,
    EventOnly,
}

struct DisplayReady {
    name: String,
    info: ax_display::DisplayInfo,
}

struct DisplayActivationSlot {
    result: ax_kspin::SpinNoIrq<Option<Result<DisplayReady, DisplayActivationError>>>,
    wait: WaitQueue,
}

impl DisplayActivationSlot {
    const fn new() -> Self {
        Self {
            result: ax_kspin::SpinNoIrq::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn publish(&self, result: Result<DisplayReady, DisplayActivationError>) {
        let mut slot = self.result.lock();
        if slot.is_some() {
            return;
        }
        *slot = Some(result);
        drop(slot);
        self.wait.notify_all();
    }

    fn publish_owner_failure(&self, error: MaintenanceError) {
        self.publish(Err(DisplayActivationError::Maintenance(error)));
    }

    fn wait_result(&self) -> Result<DisplayReady, DisplayActivationError> {
        self.wait
            .try_wait_until(|| self.result.lock().is_some())
            .map_err(MaintenanceError::from)?;
        self.result
            .lock()
            .take()
            .expect("display activation result disappeared after publication")
    }
}

struct DisplayCompletionSlot {
    generation: AtomicU64,
    state: AtomicU8,
}

impl DisplayCompletionSlot {
    const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            state: AtomicU8::new(COMPLETION_FREE),
        }
    }
}

struct DisplayRemote {
    maintenance: LazyInit<DeviceMaintenanceHandle<DisplayMaintenanceEvent>>,
    maintenance_thread: LazyInit<MaintenanceThread>,
    next_generation: AtomicU64,
    completions: [DisplayCompletionSlot; DISPLAY_COMPLETION_CAPACITY],
    completion_wait: WaitQueue,
}

impl DisplayRemote {
    fn new() -> Self {
        Self {
            maintenance: LazyInit::new(),
            maintenance_thread: LazyInit::new(),
            next_generation: AtomicU64::new(1),
            completions: [const { DisplayCompletionSlot::new() }; DISPLAY_COMPLETION_CAPACITY],
            completion_wait: WaitQueue::new(),
        }
    }

    fn install_maintenance(&self, maintenance: DeviceMaintenanceHandle<DisplayMaintenanceEvent>) {
        self.maintenance.init_once(maintenance);
    }

    fn install_thread(&self, thread: MaintenanceThread) {
        self.maintenance_thread.init_once(thread);
    }

    fn reserve_completion(&self) -> Option<(u64, &DisplayCompletionSlot)> {
        let generation = self.next_nonzero_generation();
        for slot in &self.completions {
            if slot
                .state
                .compare_exchange(
                    COMPLETION_FREE,
                    COMPLETION_PENDING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                slot.generation.store(generation, Ordering::Release);
                return Some((generation, slot));
            }
        }
        None
    }

    fn next_nonzero_generation(&self) -> u64 {
        loop {
            let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
            if generation != 0 {
                return generation;
            }
        }
    }

    fn complete(&self, generation: u64, result: Result<(), RdifDisplayError>) {
        let Some(slot) = self.completions.iter().find(|slot| {
            slot.generation.load(Ordering::Acquire) == generation
                && slot.state.load(Ordering::Acquire) == COMPLETION_PENDING
        }) else {
            warn!("display owner completed unknown flush generation {generation}");
            return;
        };
        slot.state
            .store(completion_state_from_result(result), Ordering::Release);
        self.completion_wait.notify_all();
    }

    fn fail_pending(&self) {
        for slot in &self.completions {
            if slot
                .state
                .compare_exchange(
                    COMPLETION_PENDING,
                    COMPLETION_BAD_STATE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                self.completion_wait.notify_all();
            }
        }
    }

    fn release_completion(slot: &DisplayCompletionSlot) {
        slot.generation.store(0, Ordering::Relaxed);
        slot.state.store(COMPLETION_FREE, Ordering::Release);
    }
}

impl DisplayFlushService for DisplayRemote {
    fn flush(&self) -> ax_display::DisplayResult {
        let (generation, slot) = self
            .reserve_completion()
            .ok_or(ax_display::DisplayError::NotAvailable)?;
        let publication = self.maintenance.submit_request(
            MaintenanceCauses::SUBMIT,
            DisplayMaintenanceEvent::Flush { generation },
        );
        match publication {
            Ok(MaintenancePublishResult::Published) => {}
            Ok(MaintenancePublishResult::Overflowed) => {
                Self::release_completion(slot);
                return Err(ax_display::DisplayError::NotAvailable);
            }
            Err(_) => {
                Self::release_completion(slot);
                return Err(ax_display::DisplayError::BadState);
            }
        }
        if self
            .completion_wait
            .try_wait_until(|| slot.state.load(Ordering::Acquire) != COMPLETION_PENDING)
            .is_err()
        {
            Self::release_completion(slot);
            return Err(ax_display::DisplayError::BadState);
        }
        let result = completion_result(slot.state.load(Ordering::Acquire));
        Self::release_completion(slot);
        result
    }
}

/// Activates the first display and returns its immutable public facade.
pub(crate) fn activate_display(
    taken: TakenDisplayDevice,
) -> Result<DisplayFacade, DisplayActivationError> {
    let name = String::from(taken.device.name());
    let irq = taken.irq.map(resolve_binding_irq).transpose()?;
    let remote = Arc::new(DisplayRemote::new());
    let activation = Arc::new(DisplayActivationSlot::new());
    let owner_activation = Arc::clone(&activation);
    let failure_activation = Arc::clone(&activation);
    let owner_remote = Arc::clone(&remote);
    let thread = spawn_maintenance_domain::<DisplayMaintenanceEvent, _>(
        DISPLAY_OWNER_CPU,
        alloc::format!("display-maint/{name}"),
        move |registrar| {
            let result =
                run_display_owner(taken.device, irq, owner_remote, owner_activation, registrar);
            if let Err(error) = result.as_ref() {
                failure_activation.publish_owner_failure(*error);
            }
            result
        },
    )?;
    let ready = activation.wait_result()?;
    remote.install_thread(thread);
    let service: Arc<dyn DisplayFlushService> = remote;
    Ok(DisplayFacade::new(ready.name, ready.info, service))
}

fn run_display_owner(
    mut device: alloc::boxed::Box<dyn Interface>,
    irq: Option<irq_framework::IrqId>,
    remote: Arc<DisplayRemote>,
    activation: Arc<DisplayActivationSlot>,
    registrar: MaintenanceRegistrar<DisplayMaintenanceEvent>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    let maintenance = registrar.remote_handle();
    let irq_fallback = Arc::new(DisplayIrqFallback::new());
    let registration = match prepare_irq_action(&mut *device, irq, &registrar, &irq_fallback) {
        Ok(registration) => registration,
        Err(error) => {
            let session = registrar.activate()?;
            activation.publish(Err(error));
            return close_display_owner(device, remote, session, None, irq_fallback);
        }
    };
    let session = match registrar.activate() {
        Ok(session) => session,
        Err(error) => {
            if let Some(registration) = registration
                && let Err(failure) = registration.close()
            {
                warn!(
                    "failed to close display IRQ after activation failure: {}",
                    failure.reason()
                );
            }
            return Err(error);
        }
    };
    if let Err(error) = initialize_display_owner(&mut *device, registration.as_ref()) {
        activation.publish(Err(error));
        return close_display_owner(device, remote, session, registration, irq_fallback);
    }
    let ready = match snapshot_display(&mut *device) {
        Ok(ready) => ready,
        Err(error) => {
            activation.publish(Err(error));
            return close_display_owner(device, remote, session, registration, irq_fallback);
        }
    };
    remote.install_maintenance(maintenance);
    activation.publish(Ok(ready));

    let owner_result = display_owner_loop(
        &mut *device,
        &remote,
        &session,
        registration.as_ref(),
        &irq_fallback,
    );
    if let Err(error) = owner_result {
        warn!("display maintenance owner entered contained shutdown: {error}");
    }
    close_display_owner(device, remote, session, registration, irq_fallback)
}

fn prepare_irq_action(
    device: &mut dyn Interface,
    irq: Option<irq_framework::IrqId>,
    registrar: &MaintenanceRegistrar<DisplayMaintenanceEvent>,
    irq_fallback: &Arc<DisplayIrqFallback>,
) -> Result<Option<MaintenanceIrqAction>, DisplayActivationError> {
    match device.execution() {
        DisplayExecution::Inline => Ok(None),
        DisplayExecution::Interrupt => {
            let irq = irq.ok_or(DisplayActivationError::MissingIrq)?;
            let mut endpoint = device
                .take_irq_endpoint()
                .ok_or(DisplayActivationError::MissingIrqEndpoint)?;
            let wake = registrar.local_irq_wake()?;
            let owner_cpu = registrar.owner_cpu();
            let irq_fallback = Arc::clone(irq_fallback);
            let action = registrar.register_shared_disabled(
                alloc::format!("{}/display", device.name()),
                irq,
                move |context| {
                    display_irq_action(
                        context.cpu.0,
                        owner_cpu,
                        &wake,
                        &mut *endpoint,
                        &irq_fallback,
                    )
                },
            )?;
            Ok(Some(action))
        }
    }
}

fn initialize_display_owner(
    device: &mut dyn Interface,
    action: Option<&MaintenanceIrqAction>,
) -> Result<(), DisplayActivationError> {
    device.initialize()?;
    if let Some(action) = action {
        action.enable()?;
        if let Err(error) = device.enable_irq() {
            let _ = action.disable();
            return Err(DisplayActivationError::Driver(error));
        }
    }
    Ok(())
}

fn snapshot_display(device: &mut dyn Interface) -> Result<DisplayReady, DisplayActivationError> {
    let info = device.info()?;
    let framebuffer = device.framebuffer()?;
    if framebuffer.len() < info.fb_size {
        return Err(DisplayActivationError::InvalidFramebuffer);
    }
    let base = framebuffer.as_ptr() as usize;
    Ok(DisplayReady {
        name: String::from(device.name()),
        info: map_display_info(info, base),
    })
}

fn display_irq_action(
    actual_cpu: usize,
    owner_cpu: usize,
    wake: &LocalIrqWake<DisplayMaintenanceEvent>,
    endpoint: &mut dyn rdif_irq::IrqEndpoint<Event = DisplayIrqEvent, Fault = DisplayIrqFault>,
    irq_fallback: &DisplayIrqFallback,
) -> irq_framework::IrqReturn {
    if actual_cpu != owner_cpu {
        return contain_display_irq(
            endpoint,
            ContainmentCause::OwnerUnavailable,
            None,
            irq_fallback,
            ContainedTokenRoute::Sideband,
        );
    }
    match endpoint.capture() {
        IrqCapture::Unhandled => irq_framework::IrqReturn::Unhandled,
        IrqCapture::Captured { event, masked } => {
            let publication = wake.publish_from_irq(
                MaintenanceCauses::IRQ,
                DisplayMaintenanceEvent::Irq { event, masked },
            );
            match publication {
                Ok(MaintenancePublishResult::Published) => irq_framework::IrqReturn::Wake,
                Ok(MaintenancePublishResult::Overflowed) => contain_display_irq(
                    endpoint,
                    ContainmentCause::PublicationFull,
                    masked,
                    irq_fallback,
                    ContainedTokenRoute::Sideband,
                ),
                Err(error) => {
                    let route = containment_token_route(error);
                    contain_display_irq(
                        endpoint,
                        containment_cause(error),
                        masked,
                        irq_fallback,
                        route,
                    )
                }
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
                DisplayMaintenanceEvent::Fault { reason, masked },
            );
            finish_display_fault(irq_fallback, containment, publication)
        }
    }
}

fn finish_display_fault(
    irq_fallback: &DisplayIrqFallback,
    containment: FaultContainment,
    publication: Result<MaintenancePublishResult, LocalIrqWakeError>,
) -> irq_framework::IrqReturn {
    match containment {
        FaultContainment::DeviceSourceMasked(source) => {
            let route = match publication {
                Ok(MaintenancePublishResult::Published) => ContainedTokenRoute::EventOnly,
                Ok(MaintenancePublishResult::Overflowed) => ContainedTokenRoute::Sideband,
                Err(error) => containment_token_route(error),
            };
            if route == ContainedTokenRoute::Sideband {
                irq_fallback.record_from_irq(source);
            }
            irq_framework::IrqReturn::DisableActionAndWake
        }
        FaultContainment::Uncontained => {
            irq_fallback.record_line_quench_from_irq();
            irq_framework::IrqReturn::MaskLineAndWake
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

fn containment_token_route(error: LocalIrqWakeError) -> ContainedTokenRoute {
    match error {
        LocalIrqWakeError::OwnerUnavailable {
            publication: MaintenancePublishResult::Published,
            ..
        } => ContainedTokenRoute::EventOnly,
        LocalIrqWakeError::NotHardIrq
        | LocalIrqWakeError::WrongCpu { .. }
        | LocalIrqWakeError::Closed
        | LocalIrqWakeError::OwnerIdentityMismatch
        | LocalIrqWakeError::OwnerPlacementMismatch { .. }
        | LocalIrqWakeError::OwnerUnavailable {
            publication: MaintenancePublishResult::Overflowed,
            ..
        } => ContainedTokenRoute::Sideband,
    }
}

fn contain_display_irq(
    endpoint: &mut dyn rdif_irq::IrqEndpoint<Event = DisplayIrqEvent, Fault = DisplayIrqFault>,
    cause: ContainmentCause,
    already_masked: Option<MaskedSource>,
    irq_fallback: &DisplayIrqFallback,
    route: ContainedTokenRoute,
) -> irq_framework::IrqReturn {
    let containment = match already_masked {
        Some(source) => Ok(source),
        None => endpoint.contain(cause),
    };
    match containment {
        Ok(source) => {
            if route == ContainedTokenRoute::Sideband {
                irq_fallback.record_from_irq(source);
            }
            irq_framework::IrqReturn::DisableActionAndWake
        }
        Err(_) => {
            irq_fallback.record_line_quench_from_irq();
            irq_framework::IrqReturn::MaskLineAndWake
        }
    }
}

fn display_owner_loop(
    device: &mut dyn Interface,
    remote: &DisplayRemote,
    session: &MaintenanceSession<DisplayMaintenanceEvent>,
    action: Option<&MaintenanceIrqAction>,
    irq_fallback: &DisplayIrqFallback,
) -> Result<(), MaintenanceError> {
    let mut pending = true;
    loop {
        if !pending {
            session.wait_for_pending()?;
        }
        let mut events = [None; DISPLAY_BATCH_LIMIT];
        let mut count = 0;
        let drain = session.drain_owner(DISPLAY_BATCH_LIMIT, |event| {
            events[count] = Some(event);
            count += 1;
        })?;
        pending = drain.pending();
        if drain.causes().contains(MaintenanceCauses::SHUTDOWN) {
            return Ok(());
        }
        let overflowed = drain.causes().contains(MaintenanceCauses::OVERFLOW);

        for event in events.into_iter().flatten() {
            if let DisplayMaintenanceEvent::Fault { reason, masked } = event {
                warn!("display IRQ capture fault: {reason}; masked={masked:?}");
                remote.fail_pending();
                return Err(MaintenanceError::Irq(ax_hal::irq::IrqError::Controller));
            }
        }
        for event in events.into_iter().flatten() {
            if let DisplayMaintenanceEvent::Irq { event, masked } = event {
                if device.service_irq(event).is_err() {
                    remote.fail_pending();
                    return Err(MaintenanceError::Irq(ax_hal::irq::IrqError::Controller));
                }
                if let Some(source) = masked
                    && device.rearm_irq(source).is_err()
                {
                    remote.fail_pending();
                    return Err(MaintenanceError::Irq(ax_hal::irq::IrqError::Controller));
                }
            }
        }
        for event in events.into_iter().flatten() {
            if let DisplayMaintenanceEvent::Flush { generation } = event {
                let result = if device.need_flush() {
                    device.flush()
                } else {
                    Ok(())
                };
                remote.complete(generation, result);
            }
        }

        let recovered_fallback = match irq_fallback.take_owner() {
            DisplayIrqFallbackTake::Empty => false,
            DisplayIrqFallbackTake::Source(source) => {
                rearm_display_irq_fallback(device, action, source)?;
                true
            }
            DisplayIrqFallbackTake::Conflict => {
                remote.fail_pending();
                return Err(MaintenanceError::Irq(ax_hal::irq::IrqError::Controller));
            }
        };
        if overflowed && !recovered_fallback {
            remote.fail_pending();
            return Err(MaintenanceError::Irq(ax_hal::irq::IrqError::Busy));
        }

        if pending {
            crate::task::yield_current_cpu()?;
        }
    }
}

fn rearm_display_irq_fallback(
    device: &mut dyn Interface,
    action: Option<&MaintenanceIrqAction>,
    source: MaskedSource,
) -> Result<(), MaintenanceError> {
    let action = action.ok_or(MaintenanceError::Irq(ax_hal::irq::IrqError::Controller))?;
    action.enable()?;
    if device.rearm_irq(source).is_err() {
        let _ = action.disable();
        return Err(MaintenanceError::Irq(ax_hal::irq::IrqError::Controller));
    }
    Ok(())
}

fn close_display_owner(
    mut device: alloc::boxed::Box<dyn Interface>,
    remote: Arc<DisplayRemote>,
    session: MaintenanceSession<DisplayMaintenanceEvent>,
    mut action: Option<MaintenanceIrqAction>,
    irq_fallback: Arc<DisplayIrqFallback>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    if device.disable_irq().is_err() {
        if let Some(action) = action.as_ref() {
            let _ = action.disable();
            let _ = action.synchronize();
        }
        let _retained = (device, action, remote, irq_fallback);
        session.quarantine_and_park();
    }
    if let Some(action) = action.as_ref()
        && let Err(error) = action.disable().and_then(|()| action.synchronize())
    {
        warn!("failed to disable display IRQ action during close: {error}");
        let _retained = (device, action, remote, irq_fallback);
        session.quarantine_and_park();
    }
    if irq_fallback.take_line_quench_owner() {
        let Some(action) = action.as_ref() else {
            warn!("display IRQ line quench lost its owning action during close");
            let _retained = (device, action, remote, irq_fallback);
            session.quarantine_and_park();
        };
        if let Err(error) = action.release_quench() {
            warn!("failed to release contained display IRQ line during close: {error}");
            let _retained = (device, action, remote, irq_fallback);
            session.quarantine_and_park();
        }
    }
    session.begin_close()?;
    remote.fail_pending();
    if let Some(action) = action.take()
        && let Err(failure) = action.close()
    {
        let registration = failure.into_registration();
        let _retained = (device, registration, remote, irq_fallback);
        session.quarantine_and_park();
    }
    irq_fallback.discard_after_irq_shutdown();
    while session.state() == MaintenanceState::Closing {
        let drain = session.drain_owner(DISPLAY_BATCH_LIMIT, |_| {})?;
        if !drain.pending() {
            break;
        }
    }
    session.try_begin_draining()?;
    session.finish_close()?;
    session.try_into_closed().map_err(|failure| failure.error())
}

fn completion_state_from_result(result: Result<(), RdifDisplayError>) -> u8 {
    match result {
        Ok(()) => COMPLETION_OK,
        Err(RdifDisplayError::NotSupported) => COMPLETION_UNSUPPORTED,
        Err(RdifDisplayError::NotAvailable) => COMPLETION_UNAVAILABLE,
        Err(RdifDisplayError::InvalidFramebuffer) => COMPLETION_INVALID_FRAMEBUFFER,
        Err(RdifDisplayError::Other(_)) => COMPLETION_BAD_STATE,
    }
}

fn completion_result(state: u8) -> ax_display::DisplayResult {
    match state {
        COMPLETION_OK => Ok(()),
        COMPLETION_UNSUPPORTED => Err(ax_display::DisplayError::NotSupported),
        COMPLETION_UNAVAILABLE => Err(ax_display::DisplayError::NotAvailable),
        COMPLETION_INVALID_FRAMEBUFFER => Err(ax_display::DisplayError::InvalidFramebuffer),
        _ => Err(ax_display::DisplayError::BadState),
    }
}

fn map_display_info(info: RdifDisplayInfo, fb_base_vaddr: usize) -> ax_display::DisplayInfo {
    ax_display::DisplayInfo {
        width: info.width,
        height: info.height,
        fb_base_vaddr,
        fb_size: info.fb_size,
        stride: info.stride,
        format: match info.format {
            RdifPixelFormat::Rgb565 => ax_display::PixelFormat::Rgb565,
            RdifPixelFormat::Rgb888 => ax_display::PixelFormat::Rgb888,
            RdifPixelFormat::Xrgb8888 => ax_display::PixelFormat::Xrgb8888,
            RdifPixelFormat::Argb8888 => ax_display::PixelFormat::Argb8888,
            RdifPixelFormat::Bgr888 => ax_display::PixelFormat::Bgr888,
            RdifPixelFormat::Xbgr8888 => ax_display::PixelFormat::Xbgr8888,
        },
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeSet;

    use super::*;

    fn masked_source(generation: u64) -> MaskedSource {
        MaskedSource::try_new(generation, 1).unwrap()
    }

    #[test]
    fn completion_slots_are_bounded_and_generation_identified() {
        let remote = DisplayRemote::new();
        let mut reservations = alloc::vec::Vec::new();
        let mut generations = BTreeSet::new();
        for _ in 0..DISPLAY_COMPLETION_CAPACITY {
            let reservation = remote.reserve_completion().unwrap();
            assert!(generations.insert(reservation.0));
            reservations.push(reservation);
        }
        assert!(remote.reserve_completion().is_none());

        for (_, slot) in reservations {
            DisplayRemote::release_completion(slot);
        }
        assert!(remote.reserve_completion().is_some());
    }

    #[test]
    fn completion_error_mapping_preserves_public_semantics() {
        assert_eq!(
            completion_result(completion_state_from_result(Err(
                RdifDisplayError::NotSupported
            ))),
            Err(ax_display::DisplayError::NotSupported)
        );
        assert_eq!(
            completion_result(completion_state_from_result(Err(
                RdifDisplayError::InvalidFramebuffer
            ))),
            Err(ax_display::DisplayError::InvalidFramebuffer)
        );
    }

    #[test]
    fn unpublished_irq_token_is_consumed_exactly_once() {
        let fallback = DisplayIrqFallback::new();
        let source = masked_source(7);

        assert_eq!(
            fallback.record_from_irq(source),
            DisplayIrqFallbackRecord::Stored
        );
        assert_eq!(
            fallback.record_from_irq(source),
            DisplayIrqFallbackRecord::Coalesced
        );
        assert_eq!(
            fallback.take_owner(),
            DisplayIrqFallbackTake::Source(source)
        );
        assert_eq!(fallback.take_owner(), DisplayIrqFallbackTake::Empty);
    }

    #[test]
    fn conflicting_unpublished_irq_token_fails_closed() {
        let fallback = DisplayIrqFallback::new();
        assert_eq!(
            fallback.record_from_irq(masked_source(7)),
            DisplayIrqFallbackRecord::Stored
        );
        assert_eq!(
            fallback.record_from_irq(masked_source(8)),
            DisplayIrqFallbackRecord::Conflict
        );
        assert_eq!(fallback.take_owner(), DisplayIrqFallbackTake::Conflict);
    }

    #[test]
    fn uncontained_irq_quench_is_retained_until_owner_teardown() {
        let fallback = DisplayIrqFallback::new();
        assert_eq!(
            finish_display_fault(
                &fallback,
                FaultContainment::Uncontained,
                Ok(MaintenancePublishResult::Published),
            ),
            irq_framework::IrqReturn::MaskLineAndWake
        );

        assert!(fallback.take_line_quench_owner());
        assert!(!fallback.take_line_quench_owner());
    }

    #[test]
    fn published_fault_token_stays_exclusively_in_its_event() {
        let fallback = DisplayIrqFallback::new();
        let source = masked_source(7);

        assert_eq!(
            finish_display_fault(
                &fallback,
                FaultContainment::DeviceSourceMasked(source),
                Ok(MaintenancePublishResult::Published),
            ),
            irq_framework::IrqReturn::DisableActionAndWake
        );
        assert_eq!(fallback.take_owner(), DisplayIrqFallbackTake::Empty);

        assert_eq!(
            finish_display_fault(
                &fallback,
                FaultContainment::DeviceSourceMasked(source),
                Ok(MaintenancePublishResult::Overflowed),
            ),
            irq_framework::IrqReturn::DisableActionAndWake
        );
        assert_eq!(
            fallback.take_owner(),
            DisplayIrqFallbackTake::Source(source)
        );
    }
}
