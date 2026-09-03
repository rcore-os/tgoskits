//! Scheduler-tick-gated extension work executed in ordinary task context.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use super::ThreadId;

/// Execution mode sampled by the periodic scheduler tick.
///
/// This is the OS-independent equivalent of Linux's `user_mode(regs)` result.
/// The IRQ entry passes the saved-context classification into the scheduler
/// tick so syscall boundaries do not need to publish a mirrored mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerTickMode {
    /// The thread is executing userspace.
    User,
    /// The thread is executing kernel code.
    System,
}

/// IRQ-safe tick-sampled user/system CPU time for one scheduler thread.
///
/// The object is retained by both the OS task and [`super::ThreadExtension`].
/// Only the current CPU samples it from the periodic tick's saved trap origin.
#[derive(Debug)]
pub struct SchedulerTickCpuTime {
    user_ns: AtomicU64,
    system_ns: AtomicU64,
}

impl SchedulerTickCpuTime {
    /// Creates an empty accounting stream.
    pub const fn new() -> Self {
        Self {
            user_ns: AtomicU64::new(0),
            system_ns: AtomicU64::new(0),
        }
    }

    /// Returns the raw tick-accounted totals.
    pub fn snapshot(&self) -> SchedulerTickCpuTimeSnapshot {
        SchedulerTickCpuTimeSnapshot {
            user_ns: self.user_ns.load(Ordering::Acquire),
            system_ns: self.system_ns.load(Ordering::Acquire),
        }
    }

    pub(crate) fn sample(&self, mode: SchedulerTickMode, tick_ns: u64) {
        let total = match mode {
            SchedulerTickMode::User => &self.user_ns,
            SchedulerTickMode::System => &self.system_ns,
        };
        total
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(tick_ns))
            })
            .expect("infallible scheduler-tick CPU-time update failed");
    }
}

impl Default for SchedulerTickCpuTime {
    fn default() -> Self {
        Self::new()
    }
}

/// Coherent-enough raw totals from independent monotonic tick counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulerTickCpuTimeSnapshot {
    user_ns: u64,
    system_ns: u64,
}

impl SchedulerTickCpuTimeSnapshot {
    /// Returns tick-sampled userspace CPU time.
    pub const fn user_ns(self) -> u64 {
        self.user_ns
    }

    /// Returns tick-sampled kernel CPU time.
    pub const fn system_ns(self) -> u64 {
        self.system_ns
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periodic_tick_samples_only_the_published_execution_mode() {
        let accounting = SchedulerTickCpuTime::new();

        accounting.sample(SchedulerTickMode::User, 10);
        accounting.sample(SchedulerTickMode::System, 10);

        assert_eq!(
            accounting.snapshot(),
            SchedulerTickCpuTimeSnapshot {
                user_ns: 10,
                system_ns: 10,
            }
        );
    }
}
