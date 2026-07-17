//! Discovery-to-ready controller activation on shared runtime workers.

use alloc::{boxed::Box, format, vec::Vec};
use core::{
    mem,
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering, fence},
};

use ax_driver::block::RdifBlockDevice;
use ax_kspin::SpinNoPreempt;
use rdif_block::{
    ControllerInitEndpoint, IdList, InitError, InitInput, InitIrqProgress, InitPoll, InitSchedule,
};

use super::BlockControllerError;
use crate::{
    irq::Registration,
    task::{ThreadWakeHandle, WaitQueue},
    workqueue::{DelayedWork, WorkItem, WorkOutcome, WorkPriority, WorkQueue},
};

const ACTIVATION_RUNNING: u8 = 0;
const ACTIVATION_READY: u8 = 1;
const ACTIVATION_FAILED: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivationIrqAction {
    Handled,
    Wake,
    QuenchAndWake,
}

impl ActivationIrqAction {
    const fn into_irq_return(self) -> ax_hal::irq::IrqReturn {
        match self {
            Self::Handled => ax_hal::irq::IrqReturn::Handled,
            Self::Wake => ax_hal::irq::IrqReturn::Wake,
            Self::QuenchAndWake => ax_hal::irq::IrqReturn::QuenchAndWake,
        }
    }
}

/// Linux-style publish/barrier/recheck handshake for initialization events.
///
/// IRQ always latches a declared source. It activates work only when that
/// source belongs to the currently published schedule. The symmetric full
/// barriers ensure that an IRQ racing command submission cannot miss both the
/// wait mask and the worker's pending recheck.
struct InitializationWake {
    declared_sources: u64,
    waiting_sources: AtomicU64,
    pending_sources: AtomicU64,
}

/// Fixed, allocation-free handoff for destructive IRQ acknowledgements.
///
/// A source remains in this latch until the worker either acknowledges it or
/// determines that it no longer belongs to the controller. Contention restores
/// the exact bit before the current worker pass yields.
struct DeferredInitializationIrqs {
    sources: AtomicU64,
}

impl DeferredInitializationIrqs {
    const fn new() -> Self {
        Self {
            sources: AtomicU64::new(0),
        }
    }

    fn record(&self, source_id: usize) {
        self.sources.fetch_or(1_u64 << source_id, Ordering::Release);
    }

    fn take(&self) -> IdList {
        IdList::from_bits(self.sources.swap(0, Ordering::AcqRel))
    }

    fn restore(&self, sources: IdList) {
        self.sources.fetch_or(sources.bits(), Ordering::Release);
    }
}

impl InitializationWake {
    const fn new(declared_sources: IdList) -> Self {
        Self {
            declared_sources: declared_sources.bits(),
            waiting_sources: AtomicU64::new(0),
            pending_sources: AtomicU64::new(0),
        }
    }

    fn begin_poll(&self) -> IdList {
        self.waiting_sources.store(0, Ordering::Release);
        IdList::from_bits(self.pending_sources.swap(0, Ordering::AcqRel))
    }

    fn declares(&self, source_id: usize) -> bool {
        source_id < u64::BITS as usize && self.declared_sources & (1_u64 << source_id) != 0
    }

    fn record_irq(&self, source_id: usize) -> bool {
        if !self.declares(source_id) {
            return false;
        }
        let source = 1_u64 << source_id;
        self.pending_sources.fetch_or(source, Ordering::Release);
        fence(Ordering::SeqCst);
        self.waiting_sources.load(Ordering::Acquire) & source != 0
    }

    fn publish_schedule(&self, sources: IdList) -> Result<bool, InitError> {
        let sources = sources.bits();
        if sources & !self.declared_sources != 0 {
            return Err(InitError::MissingInterrupt);
        }
        self.waiting_sources.store(sources, Ordering::Release);
        fence(Ordering::SeqCst);
        Ok(self.pending_sources.load(Ordering::Acquire) & sources != 0)
    }

    fn clear(&self) {
        self.waiting_sources.store(0, Ordering::Release);
    }
}

/// Pinned bridge retained after activation so a timer event already copied to
/// CPU-local expiration storage can never observe reclaimed callback data.
struct ControllerActivation {
    device: SpinNoPreempt<Option<RdifBlockDevice>>,
    domain: Pin<&'static WorkQueue>,
    work: WorkItem,
    timer: DelayedWork,
    wake: InitializationWake,
    deferred_irqs: DeferredInitializationIrqs,
    status: AtomicU8,
    irq_work_failed: AtomicBool,
    error: SpinNoPreempt<Option<InitError>>,
    completion: WaitQueue,
    completion_wake: ThreadWakeHandle,
}

impl ControllerActivation {
    fn allocate(
        device: RdifBlockDevice,
        declared_sources: IdList,
        cpu: usize,
        completion_wake: ThreadWakeHandle,
    ) -> &'static Self {
        let domain = Box::leak(Box::new(WorkQueue::new(cpu, WorkPriority::High)));
        let domain = unsafe {
            // SAFETY: the logical activation domain is leaked for shutdown
            // lifetime and is never moved after an intrusive item binds to it.
            Pin::new_unchecked(&*domain)
        };
        let mut activation = Box::new(Self {
            device: SpinNoPreempt::new(Some(device)),
            domain,
            work: WorkItem::new(controller_init_work_entry, 0),
            timer: DelayedWork::new(controller_init_timer_entry, 0),
            wake: InitializationWake::new(declared_sources),
            deferred_irqs: DeferredInitializationIrqs::new(),
            status: AtomicU8::new(ACTIVATION_RUNNING),
            irq_work_failed: AtomicBool::new(false),
            error: SpinNoPreempt::new(None),
            completion: WaitQueue::new(),
            completion_wake,
        });
        let address = ptr::from_ref(activation.as_ref()).expose_provenance();
        activation.work = WorkItem::new(controller_init_work_entry, address);
        activation.timer = DelayedWork::new(controller_init_timer_entry, address);
        Box::leak(activation)
    }

    fn work(&'static self) -> Pin<&'static WorkItem> {
        unsafe {
            // SAFETY: the activation object is leaked before either intrusive
            // work node can be published and therefore has a stable address.
            Pin::new_unchecked(&self.work)
        }
    }

    fn timer(&'static self) -> Pin<&'static DelayedWork> {
        unsafe {
            // SAFETY: identical shutdown-lifetime pinning contract to `work`.
            Pin::new_unchecked(&self.timer)
        }
    }

    fn queue_work(&'static self) -> Result<(), InitError> {
        self.domain
            .queue_work_on(self.work())
            .map(|_| ())
            .map_err(|_| InitError::Hardware("could not queue controller initialization work"))
    }

    fn fail_from_irq_work_admission(&self) {
        self.irq_work_failed.store(true, Ordering::Release);
        // A terminal IRQ-side failure may race the ordinary worker's Ready
        // publication. Failed overrides Ready because a deferred device source
        // can remain asserted until task-context teardown masks the device.
        self.status.store(ACTIVATION_FAILED, Ordering::Release);
        let _ = self.completion_wake.wake();
    }

    fn record_irq(&'static self, source_id: usize) -> ActivationIrqAction {
        if self.status.load(Ordering::Acquire) != ACTIVATION_RUNNING {
            return ActivationIrqAction::Handled;
        }
        if !self.wake.record_irq(source_id) {
            return ActivationIrqAction::Handled;
        }
        match self.queue_work() {
            Ok(()) => ActivationIrqAction::Wake,
            Err(_) => {
                self.fail_from_irq_work_admission();
                ActivationIrqAction::Handled
            }
        }
    }

    fn record_deferred_irq(&'static self, source_id: usize) -> ActivationIrqAction {
        if self.status.load(Ordering::Acquire) != ACTIVATION_RUNNING
            || !self.wake.declares(source_id)
        {
            return ActivationIrqAction::QuenchAndWake;
        }
        self.deferred_irqs.record(source_id);
        match self.queue_work() {
            Ok(()) => ActivationIrqAction::Wake,
            Err(_) => {
                self.fail_from_irq_work_admission();
                ActivationIrqAction::QuenchAndWake
            }
        }
    }

    fn service_deferred_irqs(&self) -> Result<bool, InitError> {
        let sources = self.deferred_irqs.take();
        if sources.is_empty() {
            return Ok(false);
        }

        let mut deferred = IdList::none();
        {
            let mut device = self.device.lock();
            let device = device.as_mut().ok_or(InitError::InvalidState)?;
            let ControllerInitEndpoint::Pending(initializer) =
                device.bundle_mut().controller_init()
            else {
                return Err(InitError::InvalidState);
            };
            for source_id in sources.iter() {
                match initializer.service_deferred_irq(source_id) {
                    InitIrqProgress::Unhandled => {}
                    InitIrqProgress::Acknowledged => {
                        let _waiting = self.wake.record_irq(source_id);
                    }
                    InitIrqProgress::Deferred => deferred.insert(source_id),
                    InitIrqProgress::Failed(error) => return Err(error),
                }
            }
        }
        if !deferred.is_empty() {
            self.deferred_irqs.restore(deferred);
        }
        Ok(!deferred.is_empty())
    }

    fn arm_schedule(&'static self, schedule: InitSchedule) -> Result<WorkOutcome, InitError> {
        let schedule = schedule.validate()?;
        let irq_ready = self.wake.publish_schedule(schedule.irq_sources())?;
        if let Some(deadline_ns) = schedule.wake_at_ns() {
            let delay_ns = deadline_ns.saturating_sub(ax_hal::time::monotonic_time_nanos());
            self.domain
                .mod_delayed_work_on(self.domain.cpu(), self.timer(), delay_ns)
                .map_err(|_| {
                    InitError::Hardware("could not arm controller initialization deadline")
                })?;
        }
        Ok(if schedule.run_again() || irq_ready {
            WorkOutcome::Requeue
        } else {
            WorkOutcome::Complete
        })
    }

    fn finish(&self, result: Result<(), InitError>) {
        let requested_status = match result {
            Ok(()) => ACTIVATION_READY,
            Err(error) => {
                *self.error.lock() = Some(error);
                ACTIVATION_FAILED
            }
        };
        let status = if self.irq_work_failed.load(Ordering::Acquire) {
            ACTIVATION_FAILED
        } else {
            requested_status
        };
        self.wake.clear();
        if publish_terminal_status(&self.status, status) {
            self.completion.notify_all();
        }
    }

    fn take_device(&self) -> RdifBlockDevice {
        self.device
            .lock()
            .take()
            .expect("activation retains its discovered device until teardown")
    }
}

fn controller_init_work_entry(data: usize) -> WorkOutcome {
    let activation = unsafe {
        // SAFETY: callback data points to a leaked ControllerActivation. Work
        // and delayed-work cancellation complete before its device is moved.
        &*ptr::with_exposed_provenance::<ControllerActivation>(data)
    };
    if activation.status.load(Ordering::Acquire) != ACTIVATION_RUNNING {
        return WorkOutcome::Complete;
    }
    if activation.timer.take_failure().is_some() {
        activation.finish(Err(InitError::Hardware(
            "controller initialization deadline delivery failed",
        )));
        return WorkOutcome::Complete;
    }
    match activation.service_deferred_irqs() {
        Ok(true) => return WorkOutcome::Requeue,
        Ok(false) => {}
        Err(error) => {
            activation.finish(Err(error));
            return WorkOutcome::Complete;
        }
    }

    let input = InitInput::new(
        ax_hal::time::monotonic_time_nanos(),
        activation.wake.begin_poll(),
    );
    let progress = {
        let mut device = activation.device.lock();
        let Some(device) = device.as_mut() else {
            activation.finish(Err(InitError::InvalidState));
            return WorkOutcome::Complete;
        };
        match device.bundle_mut().controller_init() {
            ControllerInitEndpoint::Ready => InitPoll::Ready(()),
            ControllerInitEndpoint::Pending(initializer) => initializer.poll_init(input),
        }
    };

    match progress {
        InitPoll::Ready(()) => {
            activation.finish(Ok(()));
            WorkOutcome::Complete
        }
        InitPoll::Failed(error) => {
            activation.finish(Err(error));
            WorkOutcome::Complete
        }
        InitPoll::Pending(schedule) => match activation.arm_schedule(schedule) {
            Ok(outcome) => outcome,
            Err(error) => {
                activation.finish(Err(error));
                WorkOutcome::Complete
            }
        },
    }
}

fn controller_init_timer_entry(data: usize) -> WorkOutcome {
    let activation = unsafe {
        // SAFETY: callback data follows the same leaked activation contract as
        // the ordinary initialization work callback.
        &*ptr::with_exposed_provenance::<ControllerActivation>(data)
    };
    if activation.status.load(Ordering::Acquire) != ACTIVATION_RUNNING {
        return WorkOutcome::Complete;
    }
    if let Err(error) = activation.queue_work() {
        activation.finish(Err(error));
    }
    WorkOutcome::Complete
}

fn publish_terminal_status(status: &AtomicU8, terminal: u8) -> bool {
    debug_assert!(terminal == ACTIVATION_READY || terminal == ACTIVATION_FAILED);
    status
        .compare_exchange(
            ACTIVATION_RUNNING,
            terminal,
            Ordering::Release,
            Ordering::Acquire,
        )
        .is_ok()
}

/// Drives a staged hardware controller to Ready before queue publication.
pub(super) fn drive_controller_initialization(
    mut device: RdifBlockDevice,
) -> Result<RdifBlockDevice, BlockControllerError> {
    let declared_sources = match device.bundle_mut().controller_init() {
        ControllerInitEndpoint::Ready => return Ok(device),
        ControllerInitEndpoint::Pending(initializer) => initializer.irq_sources(),
    };
    if declared_sources.is_empty() {
        return Err(BlockControllerError::Initialization(
            InitError::MissingInterrupt,
        ));
    }

    let cpu = ax_hal::percpu::this_cpu_id();
    let completion_wake = crate::task::current_thread_handle()
        .map_err(BlockControllerError::Task)?
        .wake_handle();
    let activation = ControllerActivation::allocate(device, declared_sources, cpu, completion_wake);
    let name = {
        let device = activation.device.lock();
        alloc::string::String::from(
            device
                .as_ref()
                .expect("activation owns its discovered device")
                .name(),
        )
    };
    let mut registrations = Vec::new();

    for source_id in declared_sources.iter() {
        let (binding, mut handler) = {
            let mut device = activation.device.lock();
            let device = device
                .as_mut()
                .expect("activation owns its discovered device");
            let binding = device
                .irq_for_source(source_id)
                .cloned()
                .ok_or(BlockControllerError::MissingIrqBinding(source_id))?;
            let handler = match device.bundle_mut().controller_init() {
                ControllerInitEndpoint::Ready => {
                    return Err(BlockControllerError::Initialization(
                        InitError::InvalidState,
                    ));
                }
                ControllerInitEndpoint::Pending(initializer) => initializer
                    .take_irq_handler(source_id)
                    .ok_or(BlockControllerError::MissingIrqHandler(source_id))?,
            };
            (binding, handler)
        };
        let irq = crate::irq::resolve_binding_irq(binding)?;
        let action_name = format!("{name}/blk-init-source-{source_id}");
        let registration =
            Registration::register_shared_disabled_on(action_name, irq, cpu, move |_ctx| {
                let outcome = handler.handle_irq();
                if !outcome.is_handled() {
                    return ax_hal::irq::IrqReturn::Unhandled;
                }
                if outcome.is_deferred() {
                    return activation.record_deferred_irq(source_id).into_irq_return();
                }
                activation.record_irq(source_id).into_irq_return()
            })?;
        registrations.push(registration);
    }

    for registration in &registrations {
        if let Err(error) = registration.enable() {
            if let Some(close_error) = close_unstarted_routes(&registrations) {
                mem::forget(registrations);
                return Err(close_error);
            }
            return Err(error.into());
        }
    }
    if let Err(error) = activation
        .device
        .lock()
        .as_ref()
        .expect("activation owns its discovered device")
        .enable_irq()
    {
        if let Some(close_error) = close_started_routes(activation, &registrations) {
            mem::forget(registrations);
            return Err(close_error);
        }
        return Err(error.into());
    }

    let drive_result = activation
        .domain
        .queue_work_on(activation.work())
        .map(|_| ())
        .map_err(BlockControllerError::WorkQueue)
        .and_then(|()| {
            activation
                .completion
                .try_wait_until(|| activation.status.load(Ordering::Acquire) != ACTIVATION_RUNNING)
                .map_err(BlockControllerError::Task)
        });

    let route_error = close_started_routes(activation, &registrations);
    if let Some(error) = route_error {
        // A live device with an unproven IRQ mask must retain the disabled OS
        // action objects and its discovery owner for shutdown lifetime.
        mem::forget(registrations);
        return Err(error);
    }
    activation
        .domain
        .cancel_delayed_work_sync(activation.timer())
        .map_err(BlockControllerError::WorkQueue)?;
    activation
        .domain
        .cancel_work_sync(activation.work())
        .map_err(BlockControllerError::WorkQueue)?;
    drop(registrations);

    drive_result?;

    let status = activation.status.load(Ordering::Acquire);
    let error = if activation.irq_work_failed.load(Ordering::Acquire) {
        Some(InitError::Hardware(
            "could not queue controller initialization work from IRQ",
        ))
    } else {
        activation.error.lock().take()
    };
    match status {
        ACTIVATION_READY => Ok(activation.take_device()),
        // A failed initializer may still have an admin command or DMA engine
        // owned by hardware. With no typed quiescence proof, retain the masked
        // controller in this shutdown-lifetime quarantine instead of dropping
        // mappings or DMA buffers on an assumption.
        ACTIVATION_FAILED => Err(BlockControllerError::Initialization(
            error.unwrap_or(InitError::InvalidState),
        )),
        _ => Err(BlockControllerError::Initialization(
            InitError::InvalidState,
        )),
    }
}

fn close_unstarted_routes(registrations: &[Registration]) -> Option<BlockControllerError> {
    let mut first_error = None;
    for registration in registrations {
        if let Err(error) = registration.disable()
            && first_error.is_none()
        {
            first_error = Some(error.into());
        }
    }
    for registration in registrations {
        if let Err(error) = registration.synchronize()
            && first_error.is_none()
        {
            first_error = Some(error.into());
        }
    }
    first_error
}

fn close_started_routes(
    activation: &ControllerActivation,
    registrations: &[Registration],
) -> Option<BlockControllerError> {
    if let Err(error) = activation
        .device
        .lock()
        .as_ref()
        .expect("activation owns its discovered device")
        .disable_irq()
    {
        // Keep every acknowledgement action live when the device cannot prove
        // its interrupt source is masked. The caller retains both the action
        // objects and the controller for shutdown lifetime, preventing an
        // asserted level from becoming an unhandled interrupt storm.
        return Some(error.into());
    }

    let mut first_error = None;
    for registration in registrations {
        if let Err(error) = registration.disable()
            && first_error.is_none()
        {
            first_error = Some(error.into());
        }
    }
    for registration in registrations {
        if let Err(error) = registration.synchronize()
            && first_error.is_none()
        {
            first_error = Some(error.into());
        }
    }
    first_error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_irq_between_poll_and_schedule_publish_forces_a_rerun() {
        let wake = InitializationWake::new(IdList::from_bits(1 << 3));

        assert!(wake.begin_poll().is_empty());
        assert!(!wake.record_irq(3), "the wait mask is not published yet");
        assert!(
            wake.publish_schedule(IdList::from_bits(1 << 3)).unwrap(),
            "the worker-side pending recheck must close the fast-IRQ window"
        );
    }

    #[test]
    fn unrelated_declared_irq_is_latched_without_reactivating_this_wait() {
        let wake = InitializationWake::new(IdList::from_bits((1 << 1) | (1 << 2)));
        assert!(!wake.publish_schedule(IdList::from_bits(1 << 1)).unwrap());

        assert!(!wake.record_irq(2));
        assert!(wake.begin_poll().contains(2));
    }

    #[test]
    fn deferred_irq_is_not_pollable_before_worker_acknowledgement() {
        let wake = InitializationWake::new(IdList::from_bits(1 << 3));
        let deferred = DeferredInitializationIrqs::new();

        deferred.record(3);
        assert!(wake.begin_poll().is_empty());

        let sources = deferred.take();
        assert!(sources.contains(3));
        assert!(!wake.record_irq(3));
        assert!(wake.begin_poll().contains(3));
    }

    #[test]
    fn contended_deferred_irq_is_restored_for_the_next_worker_pass() {
        let deferred = DeferredInitializationIrqs::new();
        deferred.record(5);

        let sources = deferred.take();
        assert!(sources.contains(5));
        deferred.restore(sources);

        assert!(deferred.take().contains(5));
        assert!(deferred.take().is_empty());
    }

    #[test]
    fn repeated_deferred_ack_contention_preserves_the_source_across_worker_yields() {
        let wake = InitializationWake::new(IdList::from_bits(1 << 5));
        let deferred = DeferredInitializationIrqs::new();
        deferred.record(5);

        for _ in 0..64 {
            let sources = deferred.take();
            assert!(sources.contains(5));
            deferred.restore(sources);
            // Production returns WorkOutcome::Requeue after this restoration,
            // so one contended acknowledgement consumes at most one bounded
            // worker pass instead of spinning inside the callback.
        }

        let acknowledged = deferred.take();
        assert!(acknowledged.contains(5));
        assert!(!wake.record_irq(5));
        assert!(wake.begin_poll().contains(5));
        assert!(deferred.take().is_empty());
    }

    #[test]
    fn ready_terminal_state_cannot_be_overwritten_by_a_queued_rerun() {
        let status = AtomicU8::new(ACTIVATION_RUNNING);

        assert!(publish_terminal_status(&status, ACTIVATION_READY));
        assert!(!publish_terminal_status(&status, ACTIVATION_FAILED));
        assert_eq!(status.load(Ordering::Acquire), ACTIVATION_READY);
    }
}
