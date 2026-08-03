use core::sync::atomic::{AtomicU32, Ordering};

pub(crate) const PREEMPT_NO_RESCHED: u32 = 1 << 31;
const PREEMPT_DEPTH_MASK: u32 = !PREEMPT_NO_RESCHED;

/// Outcome of preparing one ordinary preemption-guard exit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreemptExit {
    /// A nested depth was consumed without exposing a preemptible context.
    NestedConsumed,
    /// The final depth was consumed because no scheduler work is pending.
    FinalConsumed,
    /// The final depth remains published for scheduler-baton conversion.
    FinalPending,
}

/// One architecture-selected ordinary preemption word.
///
/// x86_64 stores the live word in its fixed CPU anchor, matching Linux's
/// per-CPU `__preempt_count`. Load/store architectures store it in the current
/// thread header, matching their Linux `thread_info::preempt_count` ownership.
#[repr(transparent)]
pub(crate) struct PreemptState(AtomicU32);

impl PreemptState {
    pub(crate) const fn new() -> Self {
        Self(AtomicU32::new(PREEMPT_NO_RESCHED))
    }

    #[inline(always)]
    pub(crate) fn depth(&self) -> u32 {
        self.0.load(Ordering::Relaxed) & PREEMPT_DEPTH_MASK
    }

    #[inline(always)]
    pub(crate) fn need_resched(&self) -> bool {
        self.0.load(Ordering::Relaxed) & PREEMPT_NO_RESCHED == 0
    }

    #[inline(always)]
    pub(crate) fn set_need_resched(&self) {
        self.0.fetch_and(PREEMPT_DEPTH_MASK, Ordering::Relaxed);
    }

    #[inline(always)]
    pub(crate) fn clear_need_resched(&self) {
        self.0.fetch_or(PREEMPT_NO_RESCHED, Ordering::Relaxed);
    }

    #[inline(always)]
    pub(crate) fn enter_guard(&self) {
        let previous = self.0.fetch_add(1, Ordering::Relaxed);
        assert_ne!(
            previous & PREEMPT_DEPTH_MASK,
            PREEMPT_DEPTH_MASK,
            "preemption guard nesting overflow"
        );
    }

    pub(crate) fn prepare_guard_exit(&self) -> PreemptExit {
        loop {
            let state = self.0.load(Ordering::Relaxed);
            let depth = state & PREEMPT_DEPTH_MASK;
            assert!(depth > 0, "unbalanced preemption guard exit");
            if depth == 1 {
                if state & PREEMPT_NO_RESCHED == 0 {
                    return PreemptExit::FinalPending;
                }
                if self
                    .0
                    .compare_exchange_weak(
                        state,
                        PREEMPT_NO_RESCHED,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    return PreemptExit::FinalConsumed;
                }
                continue;
            }
            if self
                .0
                .compare_exchange_weak(state, state - 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return PreemptExit::NestedConsumed;
            }
        }
    }

    #[inline(always)]
    pub(crate) fn consume_final_guard(&self) -> bool {
        self.0
            .compare_exchange(1, 0, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_exit_consumes_one_depth() {
        let state = PreemptState::new();
        state.enter_guard();
        state.enter_guard();

        assert_eq!(state.prepare_guard_exit(), PreemptExit::NestedConsumed);
        assert_eq!(state.depth(), 1);
    }

    #[test]
    fn final_exit_without_work_becomes_preemptible() {
        let state = PreemptState::new();
        state.enter_guard();

        assert_eq!(state.prepare_guard_exit(), PreemptExit::FinalConsumed);
        assert_eq!(state.depth(), 0);
    }

    #[test]
    fn pending_final_exit_waits_for_baton_claim() {
        let state = PreemptState::new();
        state.enter_guard();
        state.set_need_resched();

        assert_eq!(state.prepare_guard_exit(), PreemptExit::FinalPending);
        assert_eq!(state.depth(), 1);
        assert!(state.consume_final_guard());
        assert_eq!(state.depth(), 0);
        assert!(state.need_resched());
        state.clear_need_resched();
        assert!(!state.need_resched());
        assert!(!state.consume_final_guard());
    }

    #[test]
    #[should_panic(expected = "unbalanced preemption guard exit")]
    fn unbalanced_exit_is_rejected() {
        let state = PreemptState::new();
        let _ = state.prepare_guard_exit();
    }
}
