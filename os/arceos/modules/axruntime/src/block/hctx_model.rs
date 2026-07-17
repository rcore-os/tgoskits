//! Pure hardware-queue state used by the runtime adapter and host tests.

use core::{
    marker::PhantomData,
    sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

use thiserror::Error;

const PHASE_BITS: u32 = 3;
const PHASE_MASK: u64 = (1 << PHASE_BITS) - 1;
const MAX_EPOCH: u64 = u64::MAX >> PHASE_BITS;
const ACCESS_CLOSED: usize = 1 << (usize::BITS - 1);
const ACCESS_COUNT_MASK: usize = !ACCESS_CLOSED;
const TERMINAL_CLOSED: usize = 1 << (usize::BITS - 1);
const IRQ_PUBLISHER_COUNT_MASK: usize = !TERMINAL_CLOSED;

/// Maximum number of state transitions, events, completions, or dispatches
/// performed by one hctx work callback.
pub const HCTX_SERVICE_BUDGET: usize = 64;

/// One successful borrow of queue-driver state.
pub struct HctxAccessPermit {
    _private: (),
}

/// Admission and drain gate for direct submit and worker-side driver access.
pub struct HctxAccessGate {
    state: AtomicUsize,
}

impl HctxAccessGate {
    /// Creates an open gate with no active accessor.
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
        }
    }

    /// Acquires one driver-state access while recovery admission remains open.
    pub fn try_enter(&self) -> Option<HctxAccessPermit> {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if observed & ACCESS_CLOSED != 0 {
                return None;
            }
            assert!(
                observed & ACCESS_COUNT_MASK != ACCESS_COUNT_MASK,
                "hctx access count overflowed"
            );
            match self.state.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(HctxAccessPermit { _private: () }),
                Err(actual) => observed = actual,
            }
        }
    }

    /// Closes new access and returns whether every earlier accessor is gone.
    pub fn close(&self) -> bool {
        self.state.fetch_or(ACCESS_CLOSED, Ordering::AcqRel) & ACCESS_COUNT_MASK == 0
    }

    /// Releases one permit and returns whether it drained a closed gate.
    pub fn leave(&self, _permit: HctxAccessPermit) -> bool {
        let previous = self.state.fetch_sub(1, Ordering::AcqRel);
        assert!(
            previous & ACCESS_COUNT_MASK != 0,
            "hctx access count underflowed"
        );
        previous == (ACCESS_CLOSED | 1)
    }

    /// Returns whether the closed gate has no driver-state accessor.
    pub fn is_drained(&self) -> bool {
        self.state.load(Ordering::Acquire) == ACCESS_CLOSED
    }

    /// Reopens a fully drained gate after queue reinitialization.
    pub fn reopen(&self) -> Result<(), HctxTransitionError> {
        self.state
            .compare_exchange(ACCESS_CLOSED, 0, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| HctxTransitionError::InvalidTransition)
    }
}

impl Default for HctxAccessGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Linearization gate shared by hard-IRQ publication and watchdog timeout.
///
/// IRQ producers never wait: a producer that enters first increments the
/// active count, while a producer that observes a closed cutoff is ordered
/// after that timeout decision and may still publish its snapshot for recovery
/// processing. The hctx worker closes the gate only when every earlier IRQ
/// publisher has left, then rechecks queue-local IRQ evidence before claiming
/// a request terminal state.
pub struct HctxTerminalGate {
    state: AtomicUsize,
}

impl HctxTerminalGate {
    /// Creates an open terminal-arbitration domain.
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
        }
    }

    /// Starts one non-blocking IRQ publication that precedes a later cutoff.
    ///
    /// `None` means a timeout cutoff already linearized. The IRQ endpoint must
    /// still retain or publish its acknowledged snapshot; it is simply ordered
    /// after that timeout and cannot revoke the terminal claim.
    pub fn begin_irq_publication(&self) -> Option<HctxIrqPublication<'_>> {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if observed & TERMINAL_CLOSED != 0 {
                return None;
            }
            assert!(
                observed & IRQ_PUBLISHER_COUNT_MASK != IRQ_PUBLISHER_COUNT_MASK,
                "hctx IRQ publisher count overflowed"
            );
            match self.state.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(HctxIrqPublication {
                        gate: self,
                        _not_send: PhantomData,
                    });
                }
                Err(actual) => observed = actual,
            }
        }
    }

    /// Attempts to establish the timeout cutoff after all earlier IRQ
    /// publications completed.
    pub fn try_begin_terminal(&self) -> Option<HctxTerminalPermit<'_>> {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if observed & TERMINAL_CLOSED != 0 {
                return None;
            }
            let closed = observed | TERMINAL_CLOSED;
            match self.state.compare_exchange_weak(
                observed,
                closed,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) if observed & IRQ_PUBLISHER_COUNT_MASK == 0 => {
                    return Some(HctxTerminalPermit {
                        gate: self,
                        _not_send: PhantomData,
                    });
                }
                Ok(_) => {
                    self.state.fetch_and(!TERMINAL_CLOSED, Ordering::Release);
                    return None;
                }
                Err(actual) => observed = actual,
            }
        }
    }

    fn finish_irq_publication(&self) {
        let previous = self.state.fetch_sub(1, Ordering::Release);
        assert!(
            previous & IRQ_PUBLISHER_COUNT_MASK != 0,
            "hctx IRQ publisher count underflowed"
        );
    }

    fn finish_terminal(&self) {
        let previous = self.state.fetch_and(!TERMINAL_CLOSED, Ordering::Release);
        assert_eq!(
            previous, TERMINAL_CLOSED,
            "hctx terminal cutoff released with active IRQ publishers"
        );
    }
}

impl Default for HctxTerminalGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Active IRQ publication ordered before any successful terminal cutoff.
pub struct HctxIrqPublication<'gate> {
    gate: &'gate HctxTerminalGate,
    _not_send: PhantomData<*mut ()>,
}

impl Drop for HctxIrqPublication<'_> {
    fn drop(&mut self) {
        self.gate.finish_irq_publication();
    }
}

/// Exclusive timeout cutoff ordered after all earlier IRQ publications.
pub struct HctxTerminalPermit<'gate> {
    gate: &'gate HctxTerminalGate,
    _not_send: PhantomData<*mut ()>,
}

impl Drop for HctxTerminalPermit<'_> {
    fn drop(&mut self) {
        self.gate.finish_terminal();
    }
}

/// One callback-wide accounting object shared by every hctx service stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServiceBudget {
    remaining: usize,
}

impl ServiceBudget {
    /// Creates a non-zero budget no larger than the hard callback bound.
    pub const fn new(limit: usize) -> Result<Self, ServiceBudgetError> {
        if limit == 0 {
            return Err(ServiceBudgetError::Empty);
        }
        if limit > HCTX_SERVICE_BUDGET {
            return Err(ServiceBudgetError::TooLarge {
                requested: limit,
                maximum: HCTX_SERVICE_BUDGET,
            });
        }
        Ok(Self { remaining: limit })
    }

    /// Charges a complete operation without partially consuming the budget.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceBudgetError::Exhausted`] without changing the budget
    /// when the complete operation does not fit in this callback.
    pub fn consume(&mut self, operations: usize) -> Result<(), ServiceBudgetError> {
        let remaining =
            self.remaining
                .checked_sub(operations)
                .ok_or(ServiceBudgetError::Exhausted {
                    requested: operations,
                    remaining: self.remaining,
                })?;
        self.remaining = remaining;
        Ok(())
    }

    /// Returns the number of service operations still permitted.
    pub const fn remaining(self) -> usize {
        self.remaining
    }

    /// Returns whether every operation in this callback has been consumed.
    pub const fn is_exhausted(self) -> bool {
        self.remaining == 0
    }
}

/// Invalid callback-wide hctx service budget.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ServiceBudgetError {
    /// A zero budget cannot advance any queue state.
    #[error("hctx service budget must be non-zero")]
    Empty,
    /// A caller attempted to exceed the fixed callback latency bound.
    #[error("hctx service budget {requested} exceeds the maximum {maximum}")]
    TooLarge {
        /// Requested operation count.
        requested: usize,
        /// Fixed maximum operation count.
        maximum: usize,
    },
    /// The next complete service operation does not fit in this callback.
    #[error("hctx service operation needs {requested} budget units but only {remaining} remain")]
    Exhausted {
        /// Units needed by the indivisible operation.
        requested: usize,
        /// Units remaining before the failed charge.
        remaining: usize,
    },
}

/// A reason why one hardware queue needs its bounded service callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum HctxCause {
    /// A software context published a request for dispatch.
    Submit        = 1 << 0,
    /// The IRQ endpoint acknowledged an event for this queue.
    Irq           = 1 << 1,
    /// Task context requested cancellation.
    Cancel        = 1 << 2,
    /// A watchdog won the terminal request race.
    Timeout       = 1 << 3,
    /// Controller teardown stopped queue admission.
    Shutdown      = 1 << 4,
    /// The fixed IRQ event ring overflowed and recovery is required.
    EventOverflow = 1 << 5,
    /// The absolute deadline timer fired and request metadata must be checked.
    Watchdog      = 1 << 6,
}

impl HctxCause {
    const fn bit(self) -> u8 {
        self as u8
    }
}

/// Ordered portions of one hardware-queue service callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceStage {
    /// Consume acknowledged IRQ snapshots and classify errors first.
    IrqAndError,
    /// Resolve watchdog, cancellation, and shutdown terminal races.
    TimeoutAndCancel,
    /// Publish terminal ownership and wake each request's waiter.
    CompletionAndWake,
    /// Dispatch old hctx work before newly staged software contexts.
    Dispatch,
}

/// Facts that decide whether one bounded hctx pass must immediately run again.
///
/// A staged request blocked behind an accepted hardware request deliberately
/// sleeps until its completion IRQ. Requeueing merely because staging remains
/// non-empty would turn the shared worker into a completion poll loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServiceContinuation {
    /// A producer published a cause after this pass claimed its batch.
    pub cause_pending: bool,
    /// The pass consumed all dispatch budget while staging remains non-empty.
    pub dispatch_budget_exhausted: bool,
    /// At least one accepted request still waits in software.
    pub staged_request: bool,
    /// Hardware owns at least one accepted request that can produce an IRQ.
    pub inflight_request: bool,
}

impl ServiceContinuation {
    /// Returns whether progress is available without waiting for a new event.
    pub const fn requires_immediate_requeue(self) -> bool {
        self.cause_pending
            || (self.staged_request && (self.dispatch_budget_exhausted || !self.inflight_request))
    }
}

/// Lifecycle of one runtime-owned hardware queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum HctxPhase {
    /// Queue admission and IRQ completion are active.
    Running        = 0,
    /// IRQ is masked and DMA is being quiesced after a fault.
    Recovering     = 1,
    /// The controller initialization state machine is running again.
    Reinitializing = 2,
    /// New submissions are stopped while accepted requests drain.
    Quiescing      = 3,
    /// Host resources are detached but not assigned to a guest.
    Detached       = 4,
    /// MMIO, DMA, and IRQ ownership belongs exclusively to a guest.
    GuestOwned     = 5,
    /// Recovery could not prove safe reuse of the controller.
    Offline        = 6,
}

impl HctxPhase {
    fn decode(control: u64) -> Result<Self, HctxTransitionError> {
        match control & PHASE_MASK {
            0 => Ok(Self::Running),
            1 => Ok(Self::Recovering),
            2 => Ok(Self::Reinitializing),
            3 => Ok(Self::Quiescing),
            4 => Ok(Self::Detached),
            5 => Ok(Self::GuestOwned),
            6 => Ok(Self::Offline),
            _ => Err(HctxTransitionError::CorruptState),
        }
    }
}

/// Invalid hardware-queue lifecycle operation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum HctxTransitionError {
    /// The current phase does not permit the requested transition.
    #[error("hardware queue lifecycle transition is invalid")]
    InvalidTransition,
    /// The packed lifecycle word contains an unrecognized phase.
    #[error("hardware queue lifecycle word is corrupt")]
    CorruptState,
    /// The queue epoch cannot be advanced without wrapping.
    #[error("hardware queue epoch is exhausted")]
    EpochExhausted,
}

/// Exact source selected by the none-scheduler dispatch arbiter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchSource {
    /// A request already admitted to the hctx dispatch list.
    HardwareDispatchList,
    /// A request staged by the named CPU software context.
    Cpu(usize),
}

/// A transition permit that prevents a stale caller from committing a phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HctxTransition {
    control: u64,
}

impl HctxTransition {
    /// Phase installed by the transition.
    pub fn phase(self) -> HctxPhase {
        HctxPhase::decode(self.control).expect("transition contains a validated phase")
    }

    /// Queue epoch installed by the transition.
    pub const fn epoch(self) -> u64 {
        decode_epoch(self.control)
    }
}

/// Atomic cause and lifecycle state for one owner-serialized hardware queue.
pub struct HctxControl {
    causes: AtomicU8,
    lifecycle: AtomicU64,
}

impl HctxControl {
    /// Creates an active queue at non-zero epoch one.
    pub const fn new() -> Self {
        Self {
            causes: AtomicU8::new(0),
            lifecycle: AtomicU64::new(encode_lifecycle(1, HctxPhase::Running)),
        }
    }

    /// Publishes one service cause without allocation, locks, or callbacks.
    pub fn raise(&self, cause: HctxCause) {
        self.causes.fetch_or(cause.bit(), Ordering::Release);
    }

    /// Returns whether a cause raised before this Acquire load remains pending.
    pub fn has_pending(&self) -> bool {
        self.causes.load(Ordering::Acquire) != 0
    }

    /// Returns whether IRQ evidence or an IRQ-ring overflow was published
    /// after the worker's last cause snapshot.
    pub fn has_irq_or_error_pending(&self) -> bool {
        let urgent = HctxCause::Irq.bit() | HctxCause::EventOverflow.bit();
        self.causes.load(Ordering::Acquire) & urgent != 0
    }

    /// Atomically claims all causes visible to this bounded worker pass.
    pub fn take_service_batch(&self) -> ServiceBatch {
        ServiceBatch {
            causes: self.causes.swap(0, Ordering::AcqRel),
        }
    }

    /// Current lifecycle phase.
    pub fn phase(&self) -> HctxPhase {
        HctxPhase::decode(self.lifecycle.load(Ordering::Acquire))
            .expect("HctxControl publishes only validated phases")
    }

    /// Current generation used to reject stale IRQ and completion events.
    pub fn epoch(&self) -> u64 {
        decode_epoch(self.lifecycle.load(Ordering::Acquire))
    }

    /// Returns the event generation only while the same atomic snapshot is
    /// serviceable by normal IRQ work.
    ///
    /// Reading phase and epoch separately can combine a pre-recovery phase
    /// with the post-recovery generation. Such a mixed observation would let a
    /// late IRQ alias the next queue generation after reinitialization.
    pub fn accepted_event_epoch(&self) -> Option<u64> {
        let lifecycle = self.lifecycle.load(Ordering::Acquire);
        let phase =
            HctxPhase::decode(lifecycle).expect("HctxControl publishes only validated phases");
        matches!(phase, HctxPhase::Running | HctxPhase::Quiescing)
            .then_some(decode_epoch(lifecycle))
    }

    /// Returns whether normal request admission is still open.
    pub fn accepts_submission(&self) -> bool {
        self.phase() == HctxPhase::Running
    }

    /// Returns whether accepted requests may still consume IRQ and dispatch work.
    pub fn services_accepted_work(&self) -> bool {
        self.accepted_event_epoch().is_some()
    }

    /// Returns whether an acknowledged event belongs to a serviceable generation.
    pub fn accepts_event(&self, event_epoch: u64) -> bool {
        let lifecycle = self.lifecycle.load(Ordering::Acquire);
        let phase =
            HctxPhase::decode(lifecycle).expect("HctxControl publishes only validated phases");
        matches!(phase, HctxPhase::Running | HctxPhase::Quiescing)
            && decode_epoch(lifecycle) == event_epoch
    }

    /// Stops normal service, advances the epoch, and enters recovery.
    ///
    /// A watchdog may win while an orderly quiesce is waiting for an accepted
    /// request. Both running and quiescing queues therefore converge on the
    /// same recovery state instead of leaving teardown permanently blocked.
    pub fn begin_recovery(&self) -> Result<HctxTransition, HctxTransitionError> {
        loop {
            let observed = self.lifecycle.load(Ordering::Acquire);
            if !matches!(
                HctxPhase::decode(observed)?,
                HctxPhase::Running | HctxPhase::Quiescing
            ) {
                return Err(HctxTransitionError::InvalidTransition);
            }
            let epoch = decode_epoch(observed)
                .checked_add(1)
                .filter(|epoch| *epoch <= MAX_EPOCH)
                .ok_or(HctxTransitionError::EpochExhausted)?;
            let updated = encode_lifecycle(epoch, HctxPhase::Recovering);
            if self
                .lifecycle
                .compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(HctxTransition { control: updated });
            }
        }
    }

    /// Snapshots the exact recovery generation after an asynchronous access
    /// and IRQ drain transferred ownership to the controller worker.
    pub fn recovery_transition(&self) -> Result<HctxTransition, HctxTransitionError> {
        let control = self.lifecycle.load(Ordering::Acquire);
        if HctxPhase::decode(control)? != HctxPhase::Recovering {
            return Err(HctxTransitionError::InvalidTransition);
        }
        Ok(HctxTransition { control })
    }

    /// Moves the exact recovery generation into the common init state machine.
    pub fn begin_reinitialization(
        &self,
        recovery: HctxTransition,
    ) -> Result<HctxTransition, HctxTransitionError> {
        self.transition_exact(recovery, HctxPhase::Recovering, HctxPhase::Reinitializing)
    }

    /// Publishes a successfully reinitialized queue for normal dispatch.
    pub fn finish_reinitialization(&self) -> Result<(), HctxTransitionError> {
        self.transition_phase(HctxPhase::Reinitializing, HctxPhase::Running)
            .map(|_| ())
    }

    /// Stops new admission before accepted requests and work are drained.
    pub fn begin_quiesce(&self) -> Result<HctxTransition, HctxTransitionError> {
        self.transition_phase(HctxPhase::Running, HctxPhase::Quiescing)
    }

    /// Commits detach only for the generation represented by `quiescing`.
    pub fn finish_detach(&self, quiescing: HctxTransition) -> Result<(), HctxTransitionError> {
        self.transition_exact(quiescing, HctxPhase::Quiescing, HctxPhase::Detached)
            .map(|_| ())
    }

    /// Reopens an exactly matched, non-destructive quiesce reservation.
    pub fn cancel_quiesce(&self, quiescing: HctxTransition) -> Result<(), HctxTransitionError> {
        self.transition_exact(quiescing, HctxPhase::Quiescing, HctxPhase::Running)
            .map(|_| ())
    }

    /// Transfers an already detached queue to exclusive guest ownership.
    pub fn enter_guest_owned(&self) -> Result<(), HctxTransitionError> {
        self.transition_phase(HctxPhase::Detached, HctxPhase::GuestOwned)
            .map(|_| ())
    }

    /// Starts DMA quiescence for hardware state left by a stopped guest.
    pub fn begin_guest_return_recovery(&self) -> Result<HctxTransition, HctxTransitionError> {
        self.transition_with_new_epoch(HctxPhase::GuestOwned, HctxPhase::Recovering)
    }

    /// Permanently prevents reuse after DMA or controller quiesce failed.
    pub fn mark_offline(&self) -> Result<(), HctxTransitionError> {
        loop {
            let observed = self.lifecycle.load(Ordering::Acquire);
            let phase = HctxPhase::decode(observed)?;
            if phase == HctxPhase::Offline {
                return Ok(());
            }
            let updated = encode_lifecycle(decode_epoch(observed), HctxPhase::Offline);
            if self
                .lifecycle
                .compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    fn transition_with_new_epoch(
        &self,
        expected: HctxPhase,
        desired: HctxPhase,
    ) -> Result<HctxTransition, HctxTransitionError> {
        loop {
            let observed = self.lifecycle.load(Ordering::Acquire);
            if HctxPhase::decode(observed)? != expected {
                return Err(HctxTransitionError::InvalidTransition);
            }
            let epoch = decode_epoch(observed)
                .checked_add(1)
                .filter(|epoch| *epoch <= MAX_EPOCH)
                .ok_or(HctxTransitionError::EpochExhausted)?;
            let updated = encode_lifecycle(epoch, desired);
            if self
                .lifecycle
                .compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(HctxTransition { control: updated });
            }
        }
    }

    fn transition_phase(
        &self,
        expected: HctxPhase,
        desired: HctxPhase,
    ) -> Result<HctxTransition, HctxTransitionError> {
        loop {
            let observed = self.lifecycle.load(Ordering::Acquire);
            if HctxPhase::decode(observed)? != expected {
                return Err(HctxTransitionError::InvalidTransition);
            }
            let updated = encode_lifecycle(decode_epoch(observed), desired);
            if self
                .lifecycle
                .compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(HctxTransition { control: updated });
            }
        }
    }

    fn transition_exact(
        &self,
        permit: HctxTransition,
        expected: HctxPhase,
        desired: HctxPhase,
    ) -> Result<HctxTransition, HctxTransitionError> {
        if permit.phase() != expected {
            return Err(HctxTransitionError::InvalidTransition);
        }
        let updated = encode_lifecycle(permit.epoch(), desired);
        self.lifecycle
            .compare_exchange(permit.control, updated, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| HctxTransition { control: updated })
            .map_err(|_| HctxTransitionError::InvalidTransition)
    }
}

impl Default for HctxControl {
    fn default() -> Self {
        Self::new()
    }
}

/// Causes claimed by exactly one service callback invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServiceBatch {
    causes: u8,
}

impl ServiceBatch {
    /// Returns whether this batch claimed `cause`.
    pub const fn contains(self, cause: HctxCause) -> bool {
        self.causes & cause.bit() != 0
    }

    /// Iterates only the applicable stages in their mandatory order.
    pub fn stages(self) -> impl Iterator<Item = ServiceStage> {
        let irq = self.contains(HctxCause::Irq) || self.contains(HctxCause::EventOverflow);
        let terminal = self.contains(HctxCause::Timeout)
            || self.contains(HctxCause::Watchdog)
            || self.contains(HctxCause::Cancel)
            || self.contains(HctxCause::Shutdown);
        let dispatch = self.contains(HctxCause::Submit) || irq;
        [
            irq.then_some(ServiceStage::IrqAndError),
            terminal.then_some(ServiceStage::TimeoutAndCancel),
            (irq || terminal).then_some(ServiceStage::CompletionAndWake),
            dispatch.then_some(ServiceStage::Dispatch),
        ]
        .into_iter()
        .flatten()
    }
}

/// None-scheduler source arbiter for one hctx.
pub struct DispatchArbiter<const CPU_COUNT: usize> {
    next_cpu: usize,
}

impl<const CPU_COUNT: usize> DispatchArbiter<CPU_COUNT> {
    /// Starts round-robin selection at CPU zero.
    pub const fn new() -> Self {
        Self { next_cpu: 0 }
    }

    /// Selects old hctx dispatch work first, then a non-empty software context.
    pub fn select(
        &mut self,
        hardware_dispatch_pending: bool,
        software_context_ready: &[bool; CPU_COUNT],
    ) -> Option<DispatchSource> {
        if hardware_dispatch_pending {
            return Some(DispatchSource::HardwareDispatchList);
        }
        if CPU_COUNT == 0 {
            return None;
        }
        for offset in 0..CPU_COUNT {
            let cpu = (self.next_cpu + offset) % CPU_COUNT;
            if software_context_ready[cpu] {
                self.next_cpu = (cpu + 1) % CPU_COUNT;
                return Some(DispatchSource::Cpu(cpu));
            }
        }
        None
    }
}

impl<const CPU_COUNT: usize> Default for DispatchArbiter<CPU_COUNT> {
    fn default() -> Self {
        Self::new()
    }
}

const fn encode_lifecycle(epoch: u64, phase: HctxPhase) -> u64 {
    (epoch << PHASE_BITS) | phase as u64
}

const fn decode_epoch(control: u64) -> u64 {
    control >> PHASE_BITS
}
