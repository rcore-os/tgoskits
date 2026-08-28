//! Lost-wakeup-safe generation state for blocked-vCPU host timers.

use core::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VcpuTimerWaitToken {
    generation: u64,
}

pub(crate) struct VcpuTimerWaitGeneration {
    next: AtomicU64,
    armed: AtomicU64,
    completed: AtomicU64,
}

impl VcpuTimerWaitGeneration {
    pub(crate) const fn new() -> Self {
        Self {
            next: AtomicU64::new(0),
            armed: AtomicU64::new(0),
            completed: AtomicU64::new(0),
        }
    }

    /// Publishes state written before this call as one new blocked wait.
    pub(crate) fn arm(&self) -> VcpuTimerWaitToken {
        let previous = self
            .next
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |generation| {
                generation.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("vCPU timer wait generation exhausted"));
        let generation = previous
            .checked_add(1)
            .expect("vCPU timer wait generation must remain finite");
        self.armed.store(generation, Ordering::Release);
        VcpuTimerWaitToken { generation }
    }

    /// Claims one generation and publishes completion before its wake.
    pub(crate) fn complete(&self, token: VcpuTimerWaitToken) -> bool {
        if self
            .armed
            .compare_exchange(token.generation, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.completed.store(token.generation, Ordering::Release);
        true
    }

    pub(crate) fn cancel(&self, token: VcpuTimerWaitToken) {
        let _ =
            self.armed
                .compare_exchange(token.generation, 0, Ordering::AcqRel, Ordering::Acquire);
    }

    pub(crate) fn is_completed(&self, token: VcpuTimerWaitToken) -> bool {
        self.completed.load(Ordering::Acquire) == token.generation
    }

    pub(crate) fn invalidate(&self) -> bool {
        self.armed.swap(0, Ordering::AcqRel) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_is_published_once_for_the_armed_generation() {
        let state = VcpuTimerWaitGeneration::new();
        let token = state.arm();

        assert!(!state.is_completed(token));
        assert!(state.complete(token));
        assert!(state.is_completed(token));
        assert!(!state.complete(token));
    }

    #[test]
    fn cancelled_generation_cannot_complete_a_new_wait() {
        let state = VcpuTimerWaitGeneration::new();
        let stale = state.arm();
        state.cancel(stale);
        let current = state.arm();

        assert!(!state.complete(stale));
        assert!(!state.is_completed(stale));
        assert!(state.complete(current));
        assert!(state.is_completed(current));
    }

    #[test]
    fn invalidate_rejects_an_old_callback_after_migration() {
        let state = VcpuTimerWaitGeneration::new();
        let stale = state.arm();
        assert!(state.invalidate());
        assert!(!state.invalidate());
        let current = state.arm();

        assert!(!state.complete(stale));
        assert!(state.complete(current));
    }
}
