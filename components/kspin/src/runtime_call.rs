//! Calls across the lock runtime boundary.

#[cfg(feature = "lockdep")]
use crate::LockdepEvent;

#[inline(always)]
pub(crate) fn irq_enter() {
    imp::irq_enter();
}

#[inline(always)]
pub(crate) fn irq_exit() {
    imp::irq_exit();
}

#[inline(always)]
pub(crate) fn preempt_enter() {
    imp::preempt_enter();
}

#[inline(always)]
pub(crate) fn preempt_exit() {
    imp::preempt_exit();
}

#[inline(always)]
pub(crate) unsafe fn preempt_exit_irq_return() {
    // SAFETY: the caller forwards the LockRuntime IRQ-return contract.
    unsafe { imp::preempt_exit_irq_return() };
}

#[inline(always)]
#[cfg(feature = "lockdep")]
pub(crate) fn current_thread_id() -> u64 {
    imp::current_thread_id()
}

#[inline(always)]
#[cfg(feature = "lockdep")]
pub(crate) fn lockdep_acquire(event: LockdepEvent) {
    imp::lockdep_acquire(event);
}

#[inline(always)]
#[cfg(feature = "lockdep")]
pub(crate) fn lockdep_release(event: LockdepEvent) {
    imp::lockdep_release(event);
}

#[inline(always)]
pub(crate) fn lockdep_set_trace_enabled(enabled: bool) {
    #[cfg(feature = "lockdep")]
    imp::lockdep_set_trace_enabled(enabled);

    #[cfg(not(feature = "lockdep"))]
    let _ = enabled;
}

#[inline(always)]
pub(crate) fn lockdep_dump_trace() {
    #[cfg(feature = "lockdep")]
    imp::lockdep_dump_trace();
}

#[cfg(not(test))]
mod imp {
    pub(crate) use crate::lock_runtime::*;
}

#[cfg(test)]
pub(crate) mod imp {
    use core::cell::RefCell;
    use std::thread_local;

    #[cfg(feature = "lockdep")]
    use crate::LockdepEvent;

    #[derive(Clone, Debug, Default)]
    struct State {
        irq_depth: usize,
        preempt_depth: usize,
        need_resched: bool,
        scheduled: usize,
        events: std::vec::Vec<&'static str>,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State::default());
    }

    pub(crate) fn irq_enter() {
        update(|state| {
            state.irq_depth += 1;
            state.events.push("irq-enter");
        });
    }

    pub(crate) fn irq_exit() {
        update(|state| {
            assert!(state.irq_depth > 0, "unbalanced test IRQ exit");
            state.irq_depth -= 1;
            state.events.push("irq-exit");
        });
    }

    pub(crate) fn preempt_enter() {
        update(|state| {
            state.preempt_depth += 1;
            state.events.push("preempt-enter");
        });
    }

    pub(crate) fn preempt_exit() {
        update(|state| {
            assert!(state.preempt_depth > 0, "unbalanced test preempt exit");
            state.preempt_depth -= 1;
            state.events.push("preempt-exit");
            if state.preempt_depth == 0 && state.need_resched {
                state.need_resched = false;
                state.scheduled += 1;
                state.events.push("schedule");
            }
        });
    }

    pub(crate) unsafe fn preempt_exit_irq_return() {
        update(|state| {
            assert!(state.preempt_depth > 0, "unbalanced test preempt exit");
            state.preempt_depth -= 1;
            state.events.push("preempt-exit-irq-return");
            if state.preempt_depth == 0 && state.need_resched {
                state.need_resched = false;
                state.scheduled += 1;
                state.events.push("schedule-irq-return");
            }
        });
    }

    #[cfg(feature = "lockdep")]
    pub(crate) fn current_thread_id() -> u64 {
        1
    }

    #[cfg(feature = "lockdep")]
    pub(crate) fn lockdep_acquire(_event: LockdepEvent) {}

    #[cfg(feature = "lockdep")]
    pub(crate) fn lockdep_release(_event: LockdepEvent) {}

    #[cfg(feature = "lockdep")]
    pub(crate) fn lockdep_set_trace_enabled(_enabled: bool) {}

    #[cfg(feature = "lockdep")]
    pub(crate) fn lockdep_dump_trace() {}

    pub(crate) fn reset() {
        STATE.with(|state| *state.borrow_mut() = State::default());
    }

    pub(crate) fn set_need_resched() {
        update(|state| state.need_resched = true);
    }

    pub(crate) fn snapshot() -> (usize, usize, usize, std::vec::Vec<&'static str>) {
        with(|state| {
            (
                state.irq_depth,
                state.preempt_depth,
                state.scheduled,
                state.events.clone(),
            )
        })
    }

    fn with<T>(f: impl FnOnce(&State) -> T) -> T {
        STATE.with(|state| f(&state.borrow()))
    }

    fn update(f: impl FnOnce(&mut State)) {
        STATE.with(|state| f(&mut state.borrow_mut()));
    }
}
