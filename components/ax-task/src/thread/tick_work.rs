//! Scheduler-tick-gated extension work executed in ordinary task context.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use super::ThreadId;

/// Shared process or subsystem interest in scheduler-tick task work.
///
/// An operating system may share one gate across every scheduler thread that
/// belongs to the same higher-level accounting domain. The hard-IRQ path only
/// observes this atomic gate; it never invokes the associated callback.
#[derive(Debug)]
pub struct SchedulerTickGate {
    state: AtomicU64,
}

impl SchedulerTickGate {
    const ENABLED: u64 = 1;
    const GENERATION_STEP: u64 = 2;

    /// Creates a disabled scheduler-tick gate.
    pub const fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
        }
    }

    /// Publishes whether scheduler ticks should enqueue deferred extension work.
    ///
    /// A disable transition invalidates every queued publication from the
    /// previous enabled generation. It does not wait for a callback that an
    /// ordinary-context consumer already claimed.
    pub fn set_enabled(&self, enabled: bool) {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if (observed & Self::ENABLED != 0) == enabled {
                return;
            }
            let generation = observed
                .checked_add(Self::GENERATION_STEP)
                .expect("scheduler tick gate generation overflow");
            let updated = (generation & !Self::ENABLED) | u64::from(enabled);
            match self.state.compare_exchange_weak(
                observed,
                updated,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(current) => observed = current,
            }
        }
    }

    fn enabled_generation(&self) -> Option<u64> {
        let state = self.state.load(Ordering::Acquire);
        (state & Self::ENABLED != 0).then_some(state)
    }

    fn generation_is_enabled(&self, generation: u64) -> bool {
        self.state.load(Ordering::Acquire) == generation
    }
}

impl Default for SchedulerTickGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Task-context callback selected by one scheduler-tick publication.
///
/// `observed_ns` is the latest monotonic scheduler-tick timestamp coalesced
/// into this publication. It lets the callback account the carrier thread up
/// to the IRQ observation boundary without running OS code in hard IRQ.
///
/// The callback returns [`SchedulerTickWorkDisposition::Retry`] only when a
/// transient task-context serialization boundary prevented it from consuming
/// the publication. The task system then republishes the same generation
/// instead of spinning in one worker pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerTickWorkDisposition {
    /// The callback consumed the publication.
    Complete,
    /// The callback made no state change and needs a later task-context retry.
    Retry,
}

/// Scheduler-tick task-work callback.
pub type SchedulerTickTaskWork = unsafe extern "Rust" fn(
    data: usize,
    thread: ThreadId,
    observed_ns: u64,
) -> SchedulerTickWorkDisposition;

#[derive(Clone, Debug)]
pub(crate) struct SchedulerTickWork {
    gate: Arc<SchedulerTickGate>,
    callback: SchedulerTickTaskWork,
}

impl SchedulerTickWork {
    pub(crate) const fn new(gate: Arc<SchedulerTickGate>, callback: SchedulerTickTaskWork) -> Self {
        Self { gate, callback }
    }

    pub(crate) fn enabled_generation(&self) -> Option<u64> {
        self.gate.enabled_generation()
    }

    pub(crate) fn generation_is_enabled(&self, generation: u64) -> bool {
        self.gate.generation_is_enabled(generation)
    }

    pub(crate) fn gate(&self) -> Arc<SchedulerTickGate> {
        Arc::clone(&self.gate)
    }

    pub(crate) unsafe fn invoke(
        &self,
        data: usize,
        thread: ThreadId,
        observed_ns: u64,
    ) -> SchedulerTickWorkDisposition {
        unsafe { (self.callback)(data, thread, observed_ns) }
    }
}

/// One detached scheduler-tick publication owned by the task-work consumer.
#[derive(Debug)]
pub(crate) struct SchedulerTickWorkClaim {
    work: SchedulerTickWork,
    generation: u64,
    observed_ns: u64,
}

impl SchedulerTickWorkClaim {
    pub(crate) const fn new(work: SchedulerTickWork, generation: u64, observed_ns: u64) -> Self {
        Self {
            work,
            generation,
            observed_ns,
        }
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn generation_is_enabled(&self) -> bool {
        self.work.generation_is_enabled(self.generation)
    }

    pub(crate) unsafe fn invoke(
        &self,
        data: usize,
        thread: ThreadId,
    ) -> SchedulerTickWorkDisposition {
        unsafe { self.work.invoke(data, thread, self.observed_ns) }
    }
}
