//! Single-owner scheduling for the protocol poll worker.
//!
//! Socket and device paths only publish work. The permanent net worker is the
//! sole task allowed to run the smoltcp protocol core. This mirrors Linux NAPI:
//! `scheduled` is the owner bit and the request generation is the sticky
//! `MISSED` state that survives a request racing worker completion.

use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use ax_task::WaitQueue;

/// A wrapping protocol-poll request generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PollGeneration(u64);

/// Coordinates one permanent poll worker and task-context completion waiters.
pub(crate) struct PollRuntime {
    requested: AtomicU64,
    completed: AtomicU64,
    scheduled: AtomicBool,
    worker_wake: WaitQueue,
    completion: WaitQueue,
}

impl PollRuntime {
    /// Creates a dormant poll runtime suitable for static initialization.
    pub(crate) const fn new() -> Self {
        Self {
            requested: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            scheduled: AtomicBool::new(false),
            worker_wake: WaitQueue::new(),
            completion: WaitQueue::new(),
        }
    }

    /// Publishes protocol work and schedules the permanent worker.
    pub(crate) fn request(&self) -> PollGeneration {
        self.publish_request(|| {
            self.worker_wake.notify_one();
        })
    }

    /// Schedules the worker for non-protocol work such as deferred wakeups.
    pub(crate) fn schedule_worker(&self) {
        self.schedule(|| {
            self.worker_wake.notify_one();
        });
    }

    /// Returns the latest request generation observed by the worker.
    pub(crate) fn requested_generation(&self) -> PollGeneration {
        PollGeneration(self.requested.load(Ordering::Acquire))
    }

    /// Sleeps until the worker or another source publishes work or timeout.
    pub(crate) fn wait_timeout_until(
        &self,
        timeout: Duration,
        external_pending: impl Fn() -> bool,
    ) -> bool {
        self.worker_wake
            .wait_timeout_until(timeout, || self.is_scheduled() || external_pending())
    }

    /// Publishes completion of every request through `generation`.
    pub(crate) fn complete(&self, generation: PollGeneration) {
        self.publish_completion(generation, || {
            self.completion.notify_all();
        });
    }

    /// Blocks task context until the permanent worker completes `generation`.
    pub(crate) fn wait_for_completion(&self, generation: PollGeneration) {
        self.completion
            .wait_until(|| self.has_completed(generation));
    }

    /// Releases worker ownership and reports whether it must run another cycle.
    ///
    /// Work published before the owner bit is cleared is recovered by the
    /// generation/external-state recheck. Work published afterwards observes a
    /// clear owner bit and wakes the worker itself.
    pub(crate) fn finish_cycle(&self, external_pending: impl FnOnce() -> bool) -> bool {
        self.scheduled.store(false, Ordering::Release);
        if self.has_uncompleted_request() || external_pending() {
            self.scheduled.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }

    fn publish_request(&self, wake: impl FnOnce()) -> PollGeneration {
        let generation = PollGeneration(
            self.requested
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1),
        );
        self.schedule(wake);
        generation
    }

    fn schedule(&self, wake: impl FnOnce()) {
        if !self.scheduled.swap(true, Ordering::AcqRel) {
            wake();
        }
    }

    fn publish_completion(&self, generation: PollGeneration, notify: impl FnOnce()) {
        self.completed.store(generation.0, Ordering::Release);
        notify();
    }

    fn is_scheduled(&self) -> bool {
        self.scheduled.load(Ordering::Acquire)
    }

    fn has_uncompleted_request(&self) -> bool {
        self.requested.load(Ordering::Acquire) != self.completed.load(Ordering::Acquire)
    }

    fn has_completed(&self, generation: PollGeneration) -> bool {
        let completed = self.completed.load(Ordering::Acquire);
        completed.wrapping_sub(generation.0) < (1_u64 << 63)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synchronous_flush_never_takes_poll_ownership() {
        let runtime = PollRuntime::new();
        let mut worker_wakes = 0;

        let requested = runtime.publish_request(|| worker_wakes += 1);

        assert_eq!(worker_wakes, 1);
        assert!(!runtime.has_completed(requested));
        runtime.publish_completion(requested, || {});
        assert!(runtime.has_completed(requested));
    }

    #[test]
    fn request_racing_completion_forces_another_worker_cycle() {
        let runtime = PollRuntime::new();
        let first = runtime.publish_request(|| {});
        let claimed = runtime.requested_generation();
        assert_eq!(claimed, first);

        let second = runtime
            .publish_request(|| panic!("an active worker must coalesce a concurrent request"));
        runtime.publish_completion(claimed, || {});

        assert!(runtime.finish_cycle(|| false));
        assert!(!runtime.has_completed(second));
        runtime.publish_completion(second, || {});
        assert!(!runtime.finish_cycle(|| false));
        assert!(runtime.has_completed(second));
    }

    #[test]
    fn completion_order_remains_valid_across_generation_wrap() {
        let runtime = PollRuntime::new();
        runtime.requested.store(u64::MAX, Ordering::Relaxed);
        runtime.completed.store(u64::MAX, Ordering::Relaxed);

        let wrapped = runtime.publish_request(|| {});
        assert_eq!(wrapped, PollGeneration(0));
        assert!(!runtime.has_completed(wrapped));
        runtime.publish_completion(wrapped, || {});
        assert!(runtime.has_completed(wrapped));
    }
}
