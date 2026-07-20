//! CPU-local implementation of the lock context runtime.

#[cfg(feature = "lockdep")]
use core::fmt::{self, Write};

use ax_kspin::{LockRuntime, LockdepEvent, impl_trait as impl_lock_runtime};

#[cfg(feature = "lockdep")]
const LOCK_TRACE_CAPACITY: usize = 128;

#[cfg(feature = "lockdep")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LockTraceOperation {
    Acquire,
    Release,
}

#[cfg(feature = "lockdep")]
#[derive(Clone, Copy, Debug)]
struct LockTraceEntry {
    sequence: u64,
    operation: LockTraceOperation,
    event: LockdepEvent,
}

#[cfg(feature = "lockdep")]
impl LockTraceEntry {
    const EMPTY: Self = Self {
        sequence: 0,
        operation: LockTraceOperation::Release,
        event: LockdepEvent {
            lock_address: 0,
            thread_id: 0,
            subclass: 0,
            kind: ax_kspin::LockKind::Mutex,
            is_try: false,
        },
    };
}

/// A fixed CPU-local ring used even from hard IRQ lock paths.
#[cfg(feature = "lockdep")]
#[derive(Clone, Copy, Debug)]
struct RuntimeLockTrace {
    entries: [LockTraceEntry; LOCK_TRACE_CAPACITY],
    next: usize,
    count: usize,
    next_sequence: u64,
    enabled: bool,
}

#[cfg(feature = "lockdep")]
impl RuntimeLockTrace {
    const fn new() -> Self {
        Self {
            entries: [LockTraceEntry::EMPTY; LOCK_TRACE_CAPACITY],
            next: 0,
            count: 0,
            next_sequence: 1,
            enabled: false,
        }
    }

    fn record(&mut self, operation: LockTraceOperation, event: LockdepEvent) {
        if !self.enabled {
            return;
        }
        self.entries[self.next] = LockTraceEntry {
            sequence: self.next_sequence,
            operation,
            event,
        };
        self.next = (self.next + 1) % LOCK_TRACE_CAPACITY;
        self.count = (self.count + 1).min(LOCK_TRACE_CAPACITY);
        self.next_sequence = self.next_sequence.wrapping_add(1).max(1);
    }

    fn oldest_index(self) -> usize {
        (self.next + LOCK_TRACE_CAPACITY - self.count) % LOCK_TRACE_CAPACITY
    }

    fn set_enabled(&mut self, enabled: bool) {
        if enabled && !self.enabled {
            self.next = 0;
            self.count = 0;
            self.next_sequence = 1;
        }
        self.enabled = enabled;
    }
}

#[cfg(feature = "lockdep")]
#[ax_percpu::def_percpu]
static RUNTIME_LOCK_TRACE: RuntimeLockTrace = RuntimeLockTrace::new();

#[derive(Clone, Copy, Debug)]
struct RuntimeGuardState {
    irq: RuntimeIrqState,
    preempt: RuntimePreemptState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RuntimeIrqState {
    depth: u32,
    outer_irqs_enabled: bool,
}

impl RuntimeIrqState {
    const fn new() -> Self {
        Self {
            depth: 0,
            outer_irqs_enabled: false,
        }
    }

    #[cfg(any(feature = "fs", feature = "multitask", test))]
    const fn is_clear(self) -> bool {
        self.depth == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RuntimePreemptState {
    lock_depth: u32,
    scheduler_baton: SchedulerBatonState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchedulerBatonState {
    #[cfg(any(feature = "multitask", test))]
    Active,
    #[cfg(any(feature = "multitask", test))]
    Transferred,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreemptExitOrigin {
    Task,
    IrqReturn,
}

/// Continuation-local ownership of the raw IRQ state around preemption exit.
///
/// The CPU-local scheduler baton may move between contexts and CPUs, while the
/// saved IRQ state belongs to the suspended guard destructor. Keeping the two
/// owners separate mirrors an IRQ-lock key across a context switch: the
/// scheduler must finish its baton with IRQs masked before this token can
/// restore the calling continuation's state.
#[must_use = "saved preemption-exit IRQ state must be restored by its continuation"]
#[derive(Debug, Eq, PartialEq)]
struct PreemptExitIrqOwner {
    origin: PreemptExitOrigin,
    restore_irqs: bool,
}

impl PreemptExitIrqOwner {
    const fn from_observed(origin: PreemptExitOrigin, irqs_enabled: bool) -> Option<Self> {
        if matches!(origin, PreemptExitOrigin::IrqReturn) && irqs_enabled {
            return None;
        }
        Some(Self {
            origin,
            restore_irqs: matches!(origin, PreemptExitOrigin::Task) && irqs_enabled,
        })
    }

    #[cfg(not(test))]
    fn capture(origin: PreemptExitOrigin) -> Self {
        let irqs_enabled = ax_hal::asm::irqs_enabled();
        // Fail closed if an IRQ-return caller violates its trap-frame contract.
        ax_hal::asm::disable_irqs();
        Self::from_observed(origin, irqs_enabled)
            .expect("IRQ-return preemption exit requires hardware IRQs disabled")
    }

    #[cfg(any(feature = "multitask", test))]
    const fn permits_scheduler_entry(&self) -> bool {
        matches!(self.origin, PreemptExitOrigin::IrqReturn) || self.restore_irqs
    }

    #[cfg(all(not(test), feature = "multitask"))]
    const fn scheduler_entry(&self) -> ax_task::runtime::RuntimeSchedulerEntry {
        match self.origin {
            PreemptExitOrigin::Task => ax_task::runtime::RuntimeSchedulerEntry::PreemptExit,
            PreemptExitOrigin::IrqReturn => ax_task::runtime::RuntimeSchedulerEntry::IrqReturn,
        }
    }

    #[cfg(not(test))]
    fn restore_saved_irq_state(self) {
        debug_assert!(
            !matches!(self.origin, PreemptExitOrigin::IrqReturn) || !self.restore_irqs,
            "an IRQ-return continuation must not restore task-context IRQ state"
        );
        assert!(
            !ax_hal::asm::irqs_enabled(),
            "preemption-exit continuation lost ownership of its masked IRQ state"
        );
        if self.restore_irqs {
            ax_hal::asm::enable_irqs();
        }
    }
}

#[cfg(feature = "multitask")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScheduleContextSnapshot {
    raw_irqs_enabled: bool,
    hard_irq: bool,
    irq_depth: u32,
    preempt_lock_depth: u32,
    scheduler_baton: SchedulerBatonState,
}

#[cfg(all(feature = "multitask", feature = "uspace"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserContextBoundary {
    /// Ordinary task context immediately before the runtime masks raw IRQs.
    TaskEntry,
    /// IRQ-masked boundary after publishing user accounting and before entry.
    UserEntry,
    /// IRQ-masked boundary after publishing kernel accounting and before decoding the exit.
    KernelEntry,
    /// IRQ-masked boundary after any IRQ dispatch and before restoring task context.
    TaskReturn,
}

#[cfg(all(feature = "multitask", feature = "uspace"))]
impl UserContextBoundary {
    const fn accepts(self, snapshot: ScheduleContextSnapshot) -> bool {
        match self {
            Self::TaskEntry => snapshot.is_task_context_safe(),
            Self::UserEntry | Self::KernelEntry | Self::TaskReturn => {
                snapshot.is_masked_task_context_safe()
            }
        }
    }
}

#[cfg(feature = "multitask")]
impl ScheduleContextSnapshot {
    const fn capture(raw_irqs_enabled: bool, hard_irq: bool, state: RuntimeGuardState) -> Self {
        Self {
            raw_irqs_enabled,
            hard_irq,
            irq_depth: state.irq.depth,
            preempt_lock_depth: state.preempt.lock_depth,
            scheduler_baton: state.preempt.scheduler_baton,
        }
    }

    const fn is_task_context_safe(self) -> bool {
        self.raw_irqs_enabled && self.has_clear_task_state()
    }

    #[cfg(feature = "uspace")]
    const fn is_masked_task_context_safe(self) -> bool {
        !self.raw_irqs_enabled && self.has_clear_task_state()
    }

    const fn has_clear_task_state(self) -> bool {
        !self.hard_irq
            && self.irq_depth == 0
            && self.preempt_lock_depth == 0
            && matches!(self.scheduler_baton, SchedulerBatonState::Finished)
    }
}

#[cfg(feature = "multitask")]
struct ScheduleContextDiagnostic {
    bytes: [u8; 192],
    len: usize,
}

#[cfg(feature = "multitask")]
impl ScheduleContextDiagnostic {
    const fn new() -> Self {
        Self {
            bytes: [0; 192],
            len: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        let available = self.bytes.len().saturating_sub(self.len);
        let copied = available.min(bytes.len());
        self.bytes[self.len..self.len + copied].copy_from_slice(&bytes[..copied]);
        self.len += copied;
    }

    fn push_bool(&mut self, value: bool) {
        self.push(if value { b"true" } else { b"false" });
    }

    fn push_u32(&mut self, mut value: u32) {
        let mut digits = [0u8; 10];
        let mut start = digits.len();
        loop {
            start -= 1;
            digits[start] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        self.push(&digits[start..]);
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl RuntimePreemptState {
    const fn new() -> Self {
        Self {
            lock_depth: 0,
            scheduler_baton: SchedulerBatonState::Finished,
        }
    }

    #[cfg(any(feature = "fs", feature = "multitask", test))]
    const fn is_clear(self) -> bool {
        self.lock_depth == 0 && matches!(self.scheduler_baton, SchedulerBatonState::Finished)
    }

    #[cfg(any(feature = "multitask", test))]
    const fn has_one_scheduler_frame(self) -> bool {
        self.lock_depth == 0 && !matches!(self.scheduler_baton, SchedulerBatonState::Finished)
    }

    #[cfg(any(feature = "multitask", test))]
    const fn has_active_scheduler_baton(self) -> bool {
        self.lock_depth == 0 && matches!(self.scheduler_baton, SchedulerBatonState::Active)
    }

    #[cfg(any(feature = "multitask", test))]
    fn claim_task_scheduler(&mut self) -> bool {
        if !self.is_clear() {
            return false;
        }
        self.scheduler_baton = SchedulerBatonState::Active;
        true
    }

    #[cfg(any(feature = "multitask", test))]
    fn claim_preempt_exit_scheduler(&mut self) -> bool {
        if self.lock_depth != 1 || !matches!(self.scheduler_baton, SchedulerBatonState::Finished) {
            return false;
        }
        self.lock_depth = 0;
        self.scheduler_baton = SchedulerBatonState::Active;
        true
    }

    #[cfg(any(feature = "multitask", test))]
    fn transfer_scheduler_baton(&mut self) {
        assert!(
            self.has_active_scheduler_baton(),
            "scheduler baton transfer requires the active scheduler frame"
        );
        self.scheduler_baton = SchedulerBatonState::Transferred;
    }

    #[cfg(any(feature = "multitask", test))]
    fn finish_scheduler_baton(&mut self) {
        assert!(
            self.has_one_scheduler_frame(),
            "scheduler baton finish requires an active or transferred frame"
        );
        self.scheduler_baton = SchedulerBatonState::Finished;
    }
}

impl RuntimeGuardState {
    const fn new() -> Self {
        Self {
            irq: RuntimeIrqState::new(),
            preempt: RuntimePreemptState::new(),
        }
    }

    fn enter_irq(&mut self, outer_irqs_enabled: bool) {
        if self.irq.depth == 0 {
            self.irq.outer_irqs_enabled = outer_irqs_enabled;
        }
        self.irq.depth = self
            .irq
            .depth
            .checked_add(1)
            .expect("runtime IRQ guard nesting overflow");
    }

    fn exit_irq(&mut self, owner: &'static str) -> bool {
        assert!(
            self.irq.depth > 0,
            "unbalanced runtime IRQ guard exit from {owner}"
        );
        self.irq.depth -= 1;
        let restore_irqs = self.irq.depth == 0 && self.irq.outer_irqs_enabled;
        if self.irq.depth == 0 {
            self.irq.outer_irqs_enabled = false;
        }
        restore_irqs
    }

    fn enter_lock_preempt(&mut self) {
        self.preempt.lock_depth = self
            .preempt
            .lock_depth
            .checked_add(1)
            .expect("runtime lock preemption guard nesting overflow");
    }

    fn exit_lock_preempt(&mut self) {
        assert!(
            self.preempt.lock_depth > 0,
            "unbalanced runtime lock preemption guard exit"
        );
        self.preempt.lock_depth -= 1;
    }

    #[cfg(any(feature = "multitask", test))]
    fn claim_task_scheduler(&mut self) -> bool {
        self.irq.is_clear() && self.preempt.claim_task_scheduler()
    }

    #[cfg(any(feature = "multitask", test))]
    fn claim_preempt_exit_scheduler(&mut self) -> bool {
        self.irq.is_clear() && self.preempt.claim_preempt_exit_scheduler()
    }

    #[cfg(any(feature = "multitask", test))]
    fn exit_scheduler_preempt(&mut self, owner: &'static str) {
        assert!(
            self.irq.is_clear(),
            "{owner} exited with live IRQ guard depth={}, outer_enabled={}",
            self.irq.depth,
            self.irq.outer_irqs_enabled,
        );
        assert!(
            self.preempt.has_one_scheduler_frame(),
            "scheduler frame exit requires the exact scheduler-owned baton"
        );
        self.preempt.finish_scheduler_baton();
    }

    #[cfg(any(feature = "multitask", test))]
    fn transfer_scheduler_preempt(&mut self) {
        assert!(
            self.irq.is_clear(),
            "scheduler baton transferred with a live IRQ guard"
        );
        self.preempt.transfer_scheduler_baton();
    }

    #[cfg(feature = "fs")]
    const fn has_context_guard(self) -> bool {
        !self.irq.is_clear() || !self.preempt.is_clear()
    }
}

#[ax_percpu::def_percpu]
static RUNTIME_GUARD_STATE: RuntimeGuardState = RuntimeGuardState::new();

pub(crate) fn assert_boot_guards_released() {
    let state = read_state();
    assert_eq!(
        state.irq,
        RuntimeIrqState::new(),
        "IRQ guard crossed a runtime boot phase"
    );
    assert_eq!(
        state.preempt,
        RuntimePreemptState::new(),
        "preemption guard crossed a runtime boot phase"
    );
}

/// Validates a public scheduler entry before it can publish task state.
#[cfg(feature = "multitask")]
pub(crate) fn validate_schedule_context(
    origin: ax_task::runtime::RuntimeScheduleOrigin,
) -> ax_task::runtime::RuntimeStatus {
    use ax_task::runtime::RuntimeStatus;

    let snapshot = schedule_context_snapshot();
    if snapshot.is_task_context_safe() {
        RuntimeStatus::Success
    } else {
        report_unsafe_schedule_context(origin, snapshot);
        RuntimeStatus::UnsafeContext
    }
}

#[cfg(all(feature = "multitask", feature = "uspace"))]
pub(crate) fn validate_user_context_boundary(
    boundary: UserContextBoundary,
) -> ax_task::runtime::RuntimeStatus {
    use ax_task::runtime::RuntimeStatus;

    let snapshot = schedule_context_snapshot();
    if boundary.accepts(snapshot) {
        RuntimeStatus::Success
    } else {
        report_unsafe_user_context_boundary(boundary, snapshot);
        RuntimeStatus::UnsafeContext
    }
}

#[cfg(feature = "multitask")]
fn schedule_context_snapshot() -> ScheduleContextSnapshot {
    let raw_irqs_enabled = ax_hal::asm::irqs_enabled();
    let hard_irq = in_hard_irq();
    if raw_irqs_enabled {
        ax_hal::asm::disable_irqs();
    }
    let state = read_state();
    if raw_irqs_enabled {
        ax_hal::asm::enable_irqs();
    }
    ScheduleContextSnapshot::capture(raw_irqs_enabled, hard_irq, state)
}

#[cfg(feature = "multitask")]
fn report_unsafe_schedule_context(
    origin: ax_task::runtime::RuntimeScheduleOrigin,
    snapshot: ScheduleContextSnapshot,
) {
    use ax_task::runtime::RuntimeScheduleOrigin;

    let mut diagnostic = ScheduleContextDiagnostic::new();
    diagnostic.push(b"[ctx] o=");
    diagnostic.push(match origin {
        RuntimeScheduleOrigin::Block => b"block",
        RuntimeScheduleOrigin::Yield => b"yield",
        RuntimeScheduleOrigin::Exit => b"exit",
        RuntimeScheduleOrigin::Preempt => b"preempt",
    });
    diagnostic.push(b" i=");
    diagnostic.push_bool(snapshot.raw_irqs_enabled);
    diagnostic.push(b" h=");
    diagnostic.push_bool(snapshot.hard_irq);
    diagnostic.push(b" d=");
    diagnostic.push_u32(snapshot.irq_depth);
    diagnostic.push(b" p=");
    diagnostic.push_u32(snapshot.preempt_lock_depth);
    diagnostic.push(b" b=");
    diagnostic.push(match snapshot.scheduler_baton {
        SchedulerBatonState::Active => b"active",
        SchedulerBatonState::Transferred => b"transferred",
        SchedulerBatonState::Finished => b"finished",
    });
    diagnostic.push(b"\n");
    crate::console::write_emergency_text_bytes(diagnostic.as_bytes());
}

#[cfg(all(feature = "multitask", feature = "uspace"))]
fn report_unsafe_user_context_boundary(
    boundary: UserContextBoundary,
    snapshot: ScheduleContextSnapshot,
) {
    let mut diagnostic = ScheduleContextDiagnostic::new();
    diagnostic.push(b"[ctx] o=");
    diagnostic.push(match boundary {
        UserContextBoundary::TaskEntry => b"task-entry",
        UserContextBoundary::UserEntry => b"user-entry",
        UserContextBoundary::KernelEntry => b"kernel-entry",
        UserContextBoundary::TaskReturn => b"task-return",
    });
    diagnostic.push(b" i=");
    diagnostic.push_bool(snapshot.raw_irqs_enabled);
    diagnostic.push(b" h=");
    diagnostic.push_bool(snapshot.hard_irq);
    diagnostic.push(b" d=");
    diagnostic.push_u32(snapshot.irq_depth);
    diagnostic.push(b" p=");
    diagnostic.push_u32(snapshot.preempt_lock_depth);
    diagnostic.push(b" b=");
    diagnostic.push(match snapshot.scheduler_baton {
        SchedulerBatonState::Active => b"active",
        SchedulerBatonState::Transferred => b"transferred",
        SchedulerBatonState::Finished => b"finished",
    });
    diagnostic.push(b"\n");
    crate::console::write_emergency_text_bytes(diagnostic.as_bytes());
}

/// Reports whether the current CPU is in a context that must not sleep.
#[cfg(feature = "fs")]
pub(crate) fn in_atomic_context() -> bool {
    if !ax_hal::asm::irqs_enabled() {
        return true;
    }
    #[cfg(feature = "irq")]
    if ax_hal::irq::in_irq_context() {
        return true;
    }

    // A raw local-IRQ window gives a coherent snapshot of preemption nesting
    // without recursively entering ax-kspin's LockRuntime hooks.
    ax_hal::asm::disable_irqs();
    let guarded = read_state().has_context_guard();
    ax_hal::asm::enable_irqs();
    guarded
}

#[cfg(not(test))]
pub(crate) fn enter_irq() {
    let outer_irqs_enabled = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();

    let mut state = read_state();
    state.enter_irq(outer_irqs_enabled);
    write_state(state);
}

#[cfg(not(test))]
pub(crate) fn exit_irq(owner: &'static str) {
    let mut state = read_state();
    let restore_irqs = state.exit_irq(owner);
    write_state(state);

    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
}

#[cfg(feature = "multitask")]
pub(crate) fn finish_initial_context_switch() {
    let _task_context_safe = exit_scheduler_frame_guard_inner(
        ax_task::runtime::RuntimeSchedulerReturn::Task,
        "initial scheduler frame",
    );
}

#[cfg(not(test))]
fn update_preempt_state(operation: impl FnOnce(&mut RuntimeGuardState)) {
    // This raw IRQ window serializes the whole per-CPU state update against a
    // hard interrupt. It cannot use ax-kspin because this is its runtime hook.
    let restore_irqs = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();
    let mut state = read_state();
    operation(&mut state);
    write_state(state);
    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
}

#[cfg(not(test))]
fn exit_lock_preempt(origin: PreemptExitOrigin) {
    // Serialize the eligibility decision against hard IRQ entry. When the last
    // guard must schedule, keep that exact depth published until TaskRuntime
    // atomically converts it into the CPU-local scheduler baton.
    let irq_owner = PreemptExitIrqOwner::capture(origin);
    let mut state = read_state();
    #[cfg(feature = "multitask")]
    {
        let must_schedule = state.irq.is_clear()
            && state.preempt.lock_depth == 1
            && matches!(state.preempt.scheduler_baton, SchedulerBatonState::Finished)
            && irq_owner.permits_scheduler_entry()
            && !in_hard_irq()
            && ax_task::current_cpu_needs_resched().unwrap_or(false);
        if must_schedule {
            write_state(state);
            let entry = irq_owner.scheduler_entry();
            // SAFETY: this path retains exactly one lock-preemption depth and
            // keeps raw IRQs disabled while the runtime atomically transforms
            // that depth into the typed scheduler baton.
            if let Err(error) = unsafe { ax_task::schedule_current_cpu_from_preempt_exit(entry) } {
                panic!("preemption-exit scheduler entry failed: {error}");
            }
            assert_preempt_exit_completed();
            irq_owner.restore_saved_irq_state();
            return;
        }
    }

    state.exit_lock_preempt();
    write_state(state);
    irq_owner.restore_saved_irq_state();
}

#[cfg(all(not(test), feature = "multitask"))]
fn assert_preempt_exit_completed() {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "preemption-exit scheduler returned after restoring IRQs owned by its outer guard"
    );
    let state = read_state();
    assert!(
        state.irq.is_clear() && state.preempt.is_clear(),
        "preemption-exit scheduler returned before finishing its CPU-local baton"
    );
}

#[cfg(feature = "multitask")]
pub(crate) fn enter_scheduler_frame_guard(
    _origin: ax_task::runtime::RuntimeScheduleOrigin,
    entry: ax_task::runtime::RuntimeSchedulerEntry,
) -> ax_task::runtime::RuntimeStatus {
    use ax_task::runtime::{RuntimeSchedulerEntry, RuntimeStatus};

    let irqs_enabled = ax_hal::asm::irqs_enabled();
    let raw_state_valid = match entry {
        RuntimeSchedulerEntry::Task => irqs_enabled,
        RuntimeSchedulerEntry::PreemptExit | RuntimeSchedulerEntry::IrqReturn => !irqs_enabled,
    };
    if !raw_state_valid || in_hard_irq() {
        return RuntimeStatus::UnsafeContext;
    }

    ax_hal::asm::disable_irqs();
    let mut state = read_state();
    let claimed = match entry {
        RuntimeSchedulerEntry::Task => state.claim_task_scheduler(),
        RuntimeSchedulerEntry::PreemptExit | RuntimeSchedulerEntry::IrqReturn => {
            state.claim_preempt_exit_scheduler()
        }
    };
    if !claimed {
        if irqs_enabled {
            ax_hal::asm::enable_irqs();
        }
        return RuntimeStatus::UnsafeContext;
    }
    write_state(state);
    RuntimeStatus::Success
}

#[cfg(feature = "multitask")]
pub(crate) fn exit_scheduler_frame_guard(
    return_to: ax_task::runtime::RuntimeSchedulerReturn,
) -> bool {
    exit_scheduler_frame_guard_inner(return_to, "resumed scheduler frame")
}

#[cfg(feature = "multitask")]
fn exit_scheduler_frame_guard_inner(
    return_to: ax_task::runtime::RuntimeSchedulerReturn,
    owner: &'static str,
) -> bool {
    use ax_task::runtime::RuntimeSchedulerReturn;

    assert!(
        !ax_hal::asm::irqs_enabled(),
        "scheduler baton must keep hardware IRQs disabled until switch tail"
    );
    let mut state = read_state();
    state.exit_scheduler_preempt(owner);
    write_state(state);
    match return_to {
        RuntimeSchedulerReturn::Task => {
            ax_hal::asm::enable_irqs();
            true
        }
        RuntimeSchedulerReturn::PreemptExit | RuntimeSchedulerReturn::IrqReturn => false,
    }
}

/// Verifies the fixed CPU-local baton immediately before the raw switch.
#[cfg(feature = "multitask")]
pub(crate) fn assert_scheduler_switch_baton() {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "scheduler switch requires local IRQs disabled"
    );
    let state = read_state();
    assert!(
        state.irq.is_clear() && state.preempt.has_active_scheduler_baton(),
        "scheduler switch requires the active CPU-local scheduler baton"
    );
}

/// Commits the scheduler baton to the raw context-switch continuation.
#[cfg(feature = "multitask")]
pub(crate) fn transfer_scheduler_switch_baton() {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "scheduler baton transfer requires local IRQs disabled"
    );
    let mut state = read_state();
    state.transfer_scheduler_preempt();
    write_state(state);
}

#[cfg(feature = "multitask")]
fn in_hard_irq() -> bool {
    #[cfg(feature = "irq")]
    {
        ax_hal::irq::in_irq_context()
    }
    #[cfg(not(feature = "irq"))]
    {
        false
    }
}

fn read_state() -> RuntimeGuardState {
    // SAFETY: callers have either disabled local interrupts or execute one
    // instruction sequence before mutating this CPU's state. Preemption cannot
    // migrate kernel execution while the runtime guard service is active.
    unsafe { RUNTIME_GUARD_STATE.current_ptr_unchecked().read() }
}

fn write_state(state: RuntimeGuardState) {
    // SAFETY: only the current CPU accesses its own guard state, and IRQ entry
    // disables local interrupts before publishing a new nesting level.
    unsafe { (RUNTIME_GUARD_STATE.current_ptr_unchecked() as *mut RuntimeGuardState).write(state) }
}

#[cfg(feature = "lockdep")]
fn with_lock_trace<R>(operation: impl FnOnce(&mut RuntimeLockTrace) -> R) -> R {
    // This hook is below ax-kspin, so raw IRQ masking is the only permitted
    // serialization primitive. With local IRQs masked, this CPU cannot migrate
    // while the raw per-CPU reference is live.
    let restore_irqs = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();
    let result = unsafe {
        // SAFETY: local IRQ masking serializes task and interrupt writers and
        // prevents a scheduler safe point from migrating this execution.
        operation(RUNTIME_LOCK_TRACE.current_ref_mut_raw())
    };
    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
    result
}

#[cfg(feature = "lockdep")]
fn record_lock_trace(operation: LockTraceOperation, event: LockdepEvent) {
    with_lock_trace(|trace| trace.record(operation, event));
}

#[cfg(feature = "lockdep")]
struct RawConsoleWriter;

#[cfg(feature = "lockdep")]
impl Write for RawConsoleWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        crate::console::write_emergency_text_bytes(text.as_bytes());
        Ok(())
    }
}

#[cfg(feature = "lockdep")]
fn dump_lock_trace() {
    // Console formatting is intentionally task-context-only. IRQ paths merely
    // append fixed-size entries and never call a diagnostic observer.
    #[cfg(feature = "irq")]
    if ax_hal::irq::in_irq_context() {
        return;
    }

    let (oldest, count, restore_enabled) = with_lock_trace(|trace| {
        let snapshot = (trace.oldest_index(), trace.count, trace.enabled);
        trace.enabled = false;
        snapshot
    });
    let mut writer = RawConsoleWriter;
    for offset in 0..count {
        let index = (oldest + offset) % LOCK_TRACE_CAPACITY;
        let entry = with_lock_trace(|trace| trace.entries[index]);
        let _ = writeln!(
            writer,
            "[kspin-lock:{:04}] {:?} tid={:#x} addr={:#x} kind={:?} subclass={} try={}",
            entry.sequence,
            entry.operation,
            entry.event.thread_id,
            entry.event.lock_address,
            entry.event.kind,
            entry.event.subclass,
            entry.event.is_try,
        );
    }
    with_lock_trace(|trace| trace.enabled = restore_enabled);
}

#[cfg(not(test))]
struct ArceOsLockRuntime;

#[cfg(not(test))]
impl_lock_runtime! {
    impl LockRuntime for ArceOsLockRuntime {
        fn irq_enter() {
            enter_irq();
        }

        fn irq_exit() {
            exit_irq("lock runtime");
        }

        fn preempt_enter() {
            update_preempt_state(RuntimeGuardState::enter_lock_preempt);
        }

        fn preempt_exit() {
            exit_lock_preempt(PreemptExitOrigin::Task);
        }

        unsafe fn preempt_exit_irq_return() {
            #[cfg(feature = "ipi")]
            ax_ipi::drain_deferred_callbacks();
            exit_lock_preempt(PreemptExitOrigin::IrqReturn);
        }

        fn current_thread_id() -> u64 {
            #[cfg(feature = "multitask")]
            {
                // Lockdep hooks can run in hard IRQ context while an ax-task
                // registry lock is interrupted. The owner CPU publishes the
                // current thread directly, so tracing must not re-enter the
                // scheduler registry merely to identify it.
                let _guard = ax_kspin::IrqGuard::new();
                crate::task::current_cpu_remote(_guard.cpu_pin())
                    .and_then(ax_task::CpuRemote::current_thread)
                    .map_or(0, |id| id.as_u64())
            }
            #[cfg(not(feature = "multitask"))]
            {
                0
            }
        }

        fn lockdep_acquire(event: LockdepEvent) {
            #[cfg(feature = "lockdep")]
            record_lock_trace(LockTraceOperation::Acquire, event);

            #[cfg(not(feature = "lockdep"))]
            let _ = event;
        }

        fn lockdep_release(event: LockdepEvent) {
            #[cfg(feature = "lockdep")]
            record_lock_trace(LockTraceOperation::Release, event);

            #[cfg(not(feature = "lockdep"))]
            let _ = event;
        }

        fn lockdep_set_trace_enabled(enabled: bool) {
            #[cfg(feature = "lockdep")]
            with_lock_trace(|trace| trace.set_enabled(enabled));

            #[cfg(not(feature = "lockdep"))]
            let _ = enabled;
        }

        fn lockdep_dump_trace() {
            #[cfg(feature = "lockdep")]
            dump_lock_trace();
        }
    }
}

#[cfg(test)]
mod host_test_provider {
    use core::{
        cell::Cell,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    #[derive(Clone, Copy)]
    struct HostGuardState {
        irq_depth: u32,
        preempt_depth: u32,
    }

    impl HostGuardState {
        const fn new() -> Self {
            Self {
                irq_depth: 0,
                preempt_depth: 0,
            }
        }

        fn enter_irq(&mut self) {
            self.irq_depth = self
                .irq_depth
                .checked_add(1)
                .expect("host-test IRQ guard nesting overflow");
        }

        fn exit_irq(&mut self) {
            self.irq_depth = self
                .irq_depth
                .checked_sub(1)
                .expect("unbalanced host-test IRQ guard exit");
        }

        fn enter_preempt(&mut self) {
            self.preempt_depth = self
                .preempt_depth
                .checked_add(1)
                .expect("host-test preemption guard nesting overflow");
        }

        fn exit_preempt(&mut self) {
            self.preempt_depth = self
                .preempt_depth
                .checked_sub(1)
                .expect("unbalanced host-test preemption guard exit");
        }
    }

    std::thread_local! {
        static HOST_GUARD_STATE: Cell<HostGuardState> = const { Cell::new(HostGuardState::new()) };
        static HOST_THREAD_ID: u64 = NEXT_HOST_THREAD_ID.fetch_add(1, Ordering::Relaxed);
    }

    static NEXT_HOST_THREAD_ID: AtomicU64 = AtomicU64::new(1);

    fn update_host_state(operation: impl FnOnce(&mut HostGuardState)) {
        HOST_GUARD_STATE.with(|state| {
            let mut snapshot = state.get();
            operation(&mut snapshot);
            state.set(snapshot);
        });
    }

    struct HostTestLockRuntime;

    impl_lock_runtime! {
        impl LockRuntime for HostTestLockRuntime {
            fn irq_enter() {
                update_host_state(HostGuardState::enter_irq);
            }

            fn irq_exit() {
                update_host_state(HostGuardState::exit_irq);
            }

            fn preempt_enter() {
                update_host_state(HostGuardState::enter_preempt);
            }

            fn preempt_exit() {
                update_host_state(HostGuardState::exit_preempt);
            }

            unsafe fn preempt_exit_irq_return() {
                update_host_state(HostGuardState::exit_preempt);
            }

            fn current_thread_id() -> u64 {
                HOST_THREAD_ID.with(|id| *id)
            }

            fn lockdep_acquire(_event: LockdepEvent) {}

            fn lockdep_release(_event: LockdepEvent) {}

            fn lockdep_set_trace_enabled(_enabled: bool) {}

            fn lockdep_dump_trace() {}
        }
    }

    #[cfg(test)]
    pub(super) fn depths() -> (u32, u32) {
        HOST_GUARD_STATE.with(|state| {
            let state = state.get();
            (state.irq_depth, state.preempt_depth)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_lock_runtime_balances_context_without_hardware_irq_instructions() {
        assert_eq!(host_test_provider::depths(), (0, 0));
        {
            let _irq = ax_kspin::IrqGuard::new();
            let _preempt = ax_kspin::PreemptGuard::new();
            assert_eq!(host_test_provider::depths(), (1, 1));
        }
        assert_eq!(host_test_provider::depths(), (0, 0));
    }

    #[test]
    fn nested_irq_exits_restore_only_the_outer_state() {
        let mut state = RuntimeGuardState::new();
        state.enter_irq(true);
        state.enter_irq(false);

        assert!(!state.exit_irq("test"));
        assert!(state.exit_irq("test"));
    }

    #[test]
    fn disabled_outer_irq_state_stays_disabled() {
        let mut state = RuntimeGuardState::new();
        state.enter_irq(false);

        assert!(!state.exit_irq("test"));
    }

    #[test]
    fn preempt_exit_irq_state_is_owned_by_the_suspended_continuation() {
        let task = PreemptExitIrqOwner::from_observed(PreemptExitOrigin::Task, true).unwrap();
        assert!(task.restore_irqs);
        assert!(task.permits_scheduler_entry());

        let masked_task =
            PreemptExitIrqOwner::from_observed(PreemptExitOrigin::Task, false).unwrap();
        assert!(!masked_task.restore_irqs);
        assert!(!masked_task.permits_scheduler_entry());

        let irq_return =
            PreemptExitIrqOwner::from_observed(PreemptExitOrigin::IrqReturn, false).unwrap();
        assert!(!irq_return.restore_irqs);
        assert!(irq_return.permits_scheduler_entry());
        assert!(
            PreemptExitIrqOwner::from_observed(PreemptExitOrigin::IrqReturn, true).is_none(),
            "a trap-frame continuation must arrive with raw IRQs already disabled",
        );
    }

    #[test]
    fn lock_preempt_exit_reports_only_the_outermost_transition() {
        let mut state = RuntimeGuardState::new();
        state.enter_lock_preempt();
        state.enter_lock_preempt();

        state.exit_lock_preempt();
        assert_eq!(state.preempt.lock_depth, 1);
        state.exit_lock_preempt();
        assert!(state.preempt.is_clear());
    }

    #[test]
    fn scheduler_baton_is_exactly_one_cpu_local_frame() {
        let mut state = RuntimeGuardState::new();
        assert!(state.claim_task_scheduler());
        assert!(state.preempt.has_one_scheduler_frame());
        assert_eq!(state.preempt.scheduler_baton, SchedulerBatonState::Active);

        state.transfer_scheduler_preempt();
        assert_eq!(
            state.preempt.scheduler_baton,
            SchedulerBatonState::Transferred
        );

        state.exit_scheduler_preempt("test scheduler frame");
        assert!(state.preempt.is_clear());
        assert_eq!(state.preempt.scheduler_baton, SchedulerBatonState::Finished);
    }

    #[test]
    #[should_panic(expected = "unbalanced runtime lock preemption guard exit")]
    fn lock_exit_cannot_consume_a_scheduler_frame() {
        let mut state = RuntimeGuardState::new();
        assert!(state.claim_task_scheduler());

        state.exit_lock_preempt();
    }

    #[test]
    fn scheduler_frame_cannot_cross_a_live_lock_guard() {
        let mut state = RuntimeGuardState::new();
        state.enter_lock_preempt();

        assert!(!state.claim_task_scheduler());
        assert!(state.claim_preempt_exit_scheduler());
    }

    #[test]
    fn scheduler_frame_cannot_enter_inside_an_ordinary_irq_guard() {
        let mut state = RuntimeGuardState::new();
        state.enter_irq(true);

        assert!(!state.claim_task_scheduler());
    }

    #[test]
    #[should_panic(expected = "test scheduler frame exited with live IRQ guard depth=1")]
    fn scheduler_frame_cannot_cross_a_live_irq_guard() {
        let mut state = RuntimeGuardState::new();
        assert!(state.claim_task_scheduler());
        state.enter_irq(true);

        state.exit_scheduler_preempt("test scheduler frame");
    }

    #[test]
    #[cfg(feature = "fs")]
    fn context_guard_state_rejects_sleep_until_every_depth_is_released() {
        let mut state = RuntimeGuardState::new();
        assert!(!state.has_context_guard());

        state.enter_lock_preempt();
        assert!(state.has_context_guard());
        state.exit_lock_preempt();
        assert!(!state.has_context_guard());
    }

    #[test]
    fn initial_context_entry_consumes_the_scheduler_baton() {
        let mut state = RuntimeGuardState::new();
        assert!(state.claim_task_scheduler());

        state.exit_scheduler_preempt("test scheduler frame");
        assert!(state.preempt.is_clear());
    }

    #[cfg(all(feature = "multitask", feature = "uspace"))]
    #[test]
    fn user_context_boundaries_keep_irq_restore_with_the_typed_owner() {
        let clear = RuntimeGuardState::new();
        let task_context = ScheduleContextSnapshot::capture(true, false, clear);
        let masked_context = ScheduleContextSnapshot::capture(false, false, clear);

        assert!(UserContextBoundary::TaskEntry.accepts(task_context));
        assert!(!UserContextBoundary::TaskEntry.accepts(masked_context));
        for boundary in [
            UserContextBoundary::UserEntry,
            UserContextBoundary::KernelEntry,
            UserContextBoundary::TaskReturn,
        ] {
            assert!(boundary.accepts(masked_context));
            assert!(!boundary.accepts(task_context));
        }

        let mut guarded = RuntimeGuardState::new();
        guarded.enter_lock_preempt();
        assert!(
            !UserContextBoundary::TaskEntry
                .accepts(ScheduleContextSnapshot::capture(true, false, guarded))
        );
        assert!(
            !UserContextBoundary::KernelEntry
                .accepts(ScheduleContextSnapshot::capture(false, false, guarded))
        );
    }

    #[cfg(feature = "lockdep")]
    #[test]
    fn lock_trace_ring_is_fixed_capacity_and_preserves_sequence_order() {
        let mut trace = RuntimeLockTrace::new();
        trace.set_enabled(true);
        for address in 0..LOCK_TRACE_CAPACITY + 7 {
            trace.record(
                LockTraceOperation::Acquire,
                LockdepEvent {
                    lock_address: address,
                    thread_id: 1,
                    subclass: 0,
                    kind: ax_kspin::LockKind::Mutex,
                    is_try: false,
                },
            );
        }

        assert_eq!(trace.count, LOCK_TRACE_CAPACITY);
        let oldest = trace.entries[trace.oldest_index()];
        assert_eq!(oldest.sequence, 8);
        assert_eq!(oldest.event.lock_address, 7);
    }

    #[cfg(feature = "lockdep")]
    #[test]
    fn enabling_lock_trace_starts_a_fresh_capture() {
        let mut trace = RuntimeLockTrace::new();
        trace.set_enabled(true);
        trace.record(
            LockTraceOperation::Release,
            LockdepEvent {
                lock_address: 1,
                thread_id: 1,
                subclass: 0,
                kind: ax_kspin::LockKind::RwWrite,
                is_try: false,
            },
        );
        trace.set_enabled(false);
        trace.set_enabled(true);

        assert_eq!(trace.count, 0);
        assert_eq!(trace.next_sequence, 1);
    }
}
