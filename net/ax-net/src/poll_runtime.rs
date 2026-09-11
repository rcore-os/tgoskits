//! Generation-based single ownership for the smoltcp protocol executor.

use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use ax_task::sync::WaitQueue;

/// Bounds consecutive protocol polls across immediately runnable generations.
/// The limits follow Linux's softirq restart/time budget; neither a pending
/// socket nor an expired soft deadline grants unbounded CPU ownership.
pub(super) struct ProtocolPollBudget {
    remaining: usize,
    deadline_nanos: u64,
}

impl ProtocolPollBudget {
    const MAX_POLLS: usize = 10;
    const MAX_NANOS: u64 = 2_000_000;

    pub(super) fn new(now_nanos: u64) -> Self {
        Self {
            remaining: Self::MAX_POLLS,
            deadline_nanos: now_nanos.saturating_add(Self::MAX_NANOS),
        }
    }

    pub(super) fn consume(&mut self, now_nanos: u64) -> bool {
        self.remaining = self.remaining.saturating_sub(1);
        self.remaining == 0 || now_nanos >= self.deadline_nanos
    }

    pub(super) fn reset(&mut self, now_nanos: u64) {
        *self = Self::new(now_nanos);
    }
}

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
            self.executor_wake.notify_one();
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
        self.completion.notify_all();
    }

    pub(crate) fn wait_for_completion(&self, generation: PollGeneration) {
        if self.has_completed(generation) {
            return;
        }
        self.completion
            .wait_until(|| self.has_completed(generation));
    }

    pub(crate) fn finish_cycle(&self, external_pending: impl FnOnce() -> bool) -> bool {
        // Every producer performs a release RMW on scheduled, including
        // already-scheduled requests. Acquire that publication before reading
        // requested/external work; a release store alone can lose the producer
        // that observed scheduled=true and therefore sent no wakeup.
        self.scheduled.swap(false, Ordering::AcqRel);
        if self.requested.load(Ordering::Acquire) != self.completed.load(Ordering::Acquire)
            || external_pending()
        {
            // Keep the RMW chain intact when another producer races this
            // rearm. Overwriting its release with a plain store would hide its
            // generation from the following cycle's acquire-clear.
            self.scheduled.swap(true, Ordering::AcqRel);
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
    fn immediate_protocol_work_yields_within_a_bounded_number_of_polls() {
        let mut budget = ProtocolPollBudget::new(0);
        for poll in 1..ProtocolPollBudget::MAX_POLLS {
            assert!(!budget.consume(0), "yielded before poll budget at {poll}");
        }
        assert!(
            budget.consume(0),
            "immediate work monopolizes the owner CPU"
        );
        budget.reset(7);
        assert!(!budget.consume(7));
    }

    #[test]
    fn expensive_protocol_work_yields_at_the_time_budget() {
        let mut budget = ProtocolPollBudget::new(19);
        assert!(!budget.consume(19 + ProtocolPollBudget::MAX_NANOS - 1));
        assert!(budget.consume(19 + ProtocolPollBudget::MAX_NANOS));
    }

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
