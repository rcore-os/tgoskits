//! Generation-based single ownership for the smoltcp protocol executor.

use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use ax_task::WaitQueue;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PollGeneration(u64);

pub(crate) struct ProtocolPollRuntime {
    requested: AtomicU64,
    completed: AtomicU64,
    scheduled: AtomicBool,
    executor_wake: WaitQueue,
    completion: WaitQueue,
}

impl ProtocolPollRuntime {
    pub(crate) const fn new() -> Self {
        Self {
            requested: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            scheduled: AtomicBool::new(false),
            executor_wake: WaitQueue::new(),
            completion: WaitQueue::new(),
        }
    }

    pub(crate) fn request(&self) -> PollGeneration {
        let generation = PollGeneration(
            self.requested
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1),
        );
        self.schedule();
        generation
    }

    pub(crate) fn schedule(&self) {
        if !self.scheduled.swap(true, Ordering::AcqRel) {
            self.executor_wake.notify_one(true);
        }
    }

    pub(crate) fn requested_generation(&self) -> PollGeneration {
        PollGeneration(self.requested.load(Ordering::Acquire))
    }

    pub(crate) fn wait(&self) {
        self.executor_wake
            .wait_until(|| self.scheduled.load(Ordering::Acquire));
    }

    pub(crate) fn wait_timeout(&self, duration: Duration) -> bool {
        self.executor_wake
            .wait_timeout_until(duration, || self.scheduled.load(Ordering::Acquire))
    }

    pub(crate) fn complete(&self, generation: PollGeneration) {
        self.completed.store(generation.0, Ordering::Release);
        self.completion.notify_all(true);
    }

    pub(crate) fn wait_for_completion(&self, generation: PollGeneration) {
        if self.has_completed(generation) {
            return;
        }
        self.completion
            .wait_until(|| self.has_completed(generation));
    }

    pub(crate) fn finish_cycle(&self, external_pending: impl FnOnce() -> bool) -> bool {
        self.scheduled.store(false, Ordering::Release);
        if self.requested.load(Ordering::Acquire) != self.completed.load(Ordering::Acquire)
            || external_pending()
        {
            self.scheduled.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }

    fn has_completed(&self, generation: PollGeneration) -> bool {
        self.completed
            .load(Ordering::Acquire)
            .wrapping_sub(generation.0)
            < (1_u64 << 63)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synchronous_flush_never_takes_protocol_ownership() {
        let runtime = ProtocolPollRuntime::new();
        let generation = runtime.request();
        assert!(!runtime.has_completed(generation));
        runtime.complete(generation);
        runtime.wait_for_completion(generation);
        assert!(runtime.has_completed(generation));
    }

    #[test]
    fn request_racing_completion_forces_another_cycle() {
        let runtime = ProtocolPollRuntime::new();
        let first = runtime.request();
        let claimed = runtime.requested_generation();
        assert_eq!(first, claimed);
        let second = runtime.request();
        runtime.complete(claimed);
        assert!(runtime.finish_cycle(|| false));
        assert!(!runtime.has_completed(second));
    }

    #[test]
    fn completion_order_survives_generation_wrap() {
        let runtime = ProtocolPollRuntime::new();
        runtime.requested.store(u64::MAX - 1, Ordering::Relaxed);
        runtime.completed.store(u64::MAX - 1, Ordering::Relaxed);

        let before_wrap = runtime.request();
        assert_eq!(before_wrap, PollGeneration(u64::MAX));
        runtime.complete(before_wrap);
        assert!(runtime.has_completed(before_wrap));

        let after_wrap = runtime.request();
        assert_eq!(after_wrap, PollGeneration(0));
        assert!(!runtime.has_completed(after_wrap));
        runtime.complete(after_wrap);
        assert!(runtime.has_completed(after_wrap));
    }
}
