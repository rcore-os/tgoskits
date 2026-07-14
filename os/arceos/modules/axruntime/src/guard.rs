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
    irq_depth: u32,
    outer_irqs_enabled: bool,
    preempt_depth: u32,
}

impl RuntimeGuardState {
    const fn new() -> Self {
        Self {
            irq_depth: 0,
            outer_irqs_enabled: false,
            preempt_depth: 0,
        }
    }

    fn enter_irq(&mut self, outer_irqs_enabled: bool) {
        if self.irq_depth == 0 {
            self.outer_irqs_enabled = outer_irqs_enabled;
        }
        self.irq_depth = self
            .irq_depth
            .checked_add(1)
            .expect("runtime IRQ guard nesting overflow");
    }

    fn exit_irq(&mut self) -> bool {
        assert!(self.irq_depth > 0, "unbalanced runtime IRQ guard exit");
        self.irq_depth -= 1;
        let restore_irqs = self.irq_depth == 0 && self.outer_irqs_enabled;
        if self.irq_depth == 0 {
            self.outer_irqs_enabled = false;
        }
        restore_irqs
    }

    #[cfg(any(feature = "multitask", test))]
    fn finish_initial_context_switch(&mut self) -> bool {
        self.exit_irq()
    }

    fn enter_preempt(&mut self) {
        self.preempt_depth = self
            .preempt_depth
            .checked_add(1)
            .expect("runtime preemption guard nesting overflow");
    }

    fn exit_preempt(&mut self) -> bool {
        assert!(
            self.preempt_depth > 0,
            "unbalanced runtime preemption guard exit"
        );
        self.preempt_depth -= 1;
        self.preempt_depth == 0
    }

    #[cfg(any(feature = "multitask", test))]
    fn replace_preempt_depth(&mut self, next: u32) -> u32 {
        core::mem::replace(&mut self.preempt_depth, next)
    }

    #[cfg(feature = "fs")]
    const fn has_context_guard(self) -> bool {
        self.irq_depth != 0 || self.preempt_depth != 0
    }
}

#[ax_percpu::def_percpu]
static RUNTIME_GUARD_STATE: RuntimeGuardState = RuntimeGuardState::new();

pub(crate) fn assert_boot_guards_released() {
    let state = read_state();
    assert_eq!(state.irq_depth, 0, "IRQ guard crossed a runtime boot phase");
    assert_eq!(
        state.preempt_depth, 0,
        "preemption guard crossed a runtime boot phase"
    );
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

pub(crate) fn enter_irq() {
    let outer_irqs_enabled = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();

    let mut state = read_state();
    state.enter_irq(outer_irqs_enabled);
    write_state(state);
}

pub(crate) fn exit_irq() {
    let mut state = read_state();
    let restore_irqs = state.exit_irq();
    write_state(state);

    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
}

#[cfg(feature = "multitask")]
pub(crate) fn finish_initial_context_switch() {
    let mut state = read_state();
    let restore_irqs = state.finish_initial_context_switch();
    write_state(state);

    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
}

fn enter_preempt() {
    // This raw IRQ window serializes the whole per-CPU state update against a
    // hard interrupt. It cannot use ax-kspin because this is its runtime hook.
    let restore_irqs = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();
    let mut state = read_state();
    state.enter_preempt();
    write_state(state);
    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
}

fn exit_preempt() -> bool {
    // Keep IRQ-depth and preemption-depth fields from being overwritten by an
    // interrupt that arrives during the non-atomic per-CPU copy/update/write.
    let restore_irqs = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();
    let mut state = read_state();
    let outermost = state.exit_preempt();
    write_state(state);
    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
    outermost
}

#[cfg(feature = "multitask")]
pub(crate) fn enter_scheduler_frame_guard() {
    enter_preempt();
}

#[cfg(feature = "multitask")]
pub(crate) fn exit_scheduler_frame_guard() {
    let _outermost = exit_preempt();
}

/// Saves the outgoing task's preemption nesting and installs the incoming one.
///
/// IRQ nesting is deliberately not switched: the scheduler IRQ guard is a
/// CPU-local baton consumed by the resumed or freshly entered context.
#[cfg(feature = "multitask")]
pub(crate) fn replace_preempt_depth_for_context_switch(next: u32) -> u32 {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "preemption context switch requires local IRQs disabled"
    );
    let mut state = read_state();
    let previous = state.replace_preempt_depth(next);
    write_state(state);
    previous
}

fn read_state() -> RuntimeGuardState {
    // SAFETY: callers have either disabled local interrupts or execute one
    // instruction sequence before mutating this CPU's state. Preemption cannot
    // migrate kernel execution while the runtime guard service is active.
    unsafe { RUNTIME_GUARD_STATE.read_current_raw() }
}

fn write_state(state: RuntimeGuardState) {
    // SAFETY: only the current CPU accesses its own guard state, and IRQ entry
    // disables local interrupts before publishing a new nesting level.
    unsafe { RUNTIME_GUARD_STATE.write_current_raw(state) }
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
        ax_hal::console::write_text_bytes(text.as_bytes());
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

struct ArceOsLockRuntime;

impl_lock_runtime! {
    impl LockRuntime for ArceOsLockRuntime {
        fn irq_enter() {
            enter_irq();
        }

        fn irq_exit() {
            exit_irq();
        }

        fn irqs_enabled() -> bool {
            ax_hal::asm::irqs_enabled()
        }

        fn preempt_enter() {
            enter_preempt();
        }

        fn preempt_exit() -> bool {
            exit_preempt()
        }

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

        fn need_resched() -> bool {
            #[cfg(feature = "multitask")]
            {
                ax_task::current_cpu_needs_resched().unwrap_or(false)
            }
            #[cfg(not(feature = "multitask"))]
            {
                false
            }
        }

        fn schedule() {
            #[cfg(feature = "multitask")]
            if let Err(error) = ax_task::schedule_current_cpu() {
                panic!("preemption-exit scheduler entry failed: {error}");
            }
        }

        fn current_thread_id() -> u64 {
            #[cfg(feature = "multitask")]
            {
                // Lockdep hooks can run in hard IRQ context while an ax-task
                // registry lock is interrupted. The owner CPU publishes the
                // current thread directly, so tracing must not re-enter the
                // scheduler registry merely to identify it.
                crate::task::current_cpu_local()
                    .and_then(ax_task::CpuLocal::current)
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
mod tests {
    use super::*;

    #[test]
    fn nested_irq_exits_restore_only_the_outer_state() {
        let mut state = RuntimeGuardState::new();
        state.enter_irq(true);
        state.enter_irq(false);

        assert!(!state.exit_irq());
        assert!(state.exit_irq());
    }

    #[test]
    fn disabled_outer_irq_state_stays_disabled() {
        let mut state = RuntimeGuardState::new();
        state.enter_irq(false);

        assert!(!state.exit_irq());
    }

    #[test]
    fn preempt_exit_reports_only_the_outermost_transition() {
        let mut state = RuntimeGuardState::new();
        state.enter_preempt();
        state.enter_preempt();

        assert!(!state.exit_preempt());
        assert!(state.exit_preempt());
    }

    #[test]
    fn context_switch_replaces_only_preemption_nesting() {
        let mut state = RuntimeGuardState::new();
        state.enter_irq(true);
        state.enter_preempt();
        state.enter_preempt();

        assert_eq!(state.replace_preempt_depth(7), 2);
        assert_eq!(state.preempt_depth, 7);
        assert_eq!(state.irq_depth, 1);
        assert!(state.outer_irqs_enabled);
    }

    #[test]
    #[cfg(feature = "fs")]
    fn context_guard_state_rejects_sleep_until_every_depth_is_released() {
        let mut state = RuntimeGuardState::new();
        assert!(!state.has_context_guard());

        state.enter_preempt();
        assert!(state.has_context_guard());
        assert!(state.exit_preempt());
        assert!(!state.has_context_guard());
    }

    #[test]
    fn initial_context_entry_consumes_the_scheduler_irq_baton() {
        let mut state = RuntimeGuardState::new();
        state.enter_irq(true);

        assert!(state.finish_initial_context_switch());
        assert_eq!(state.irq_depth, 0);
        assert!(!state.outer_irqs_enabled);
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
