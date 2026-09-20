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
    realtime: Option<RealtimeTickAccounting>,
}

#[derive(Debug)]
struct RealtimeTickAccounting {
    gate: Arc<SchedulerTickGate>,
    period_ns: AtomicU64,
    last_period: AtomicU64,
    ticks: AtomicU64,
}

impl SchedulerTickCpuTime {
    /// Creates an empty accounting stream.
    pub const fn new() -> Self {
        Self {
            user_ns: AtomicU64::new(0),
            system_ns: AtomicU64::new(0),
            realtime: None,
        }
    }

    /// Creates tick accounting with optional continuous real-time accounting.
    ///
    /// The OS owns the shared gate. While enabled, actual FIFO/RR class ticks
    /// count at most once per common monotonic-clock period, including across
    /// migration. A real-time wake or PI deboost to Fair resets the count but
    /// preserves deduplication.
    /// Disabling the gate preserves previously accumulated ticks.
    pub fn with_realtime_gate(gate: Arc<SchedulerTickGate>) -> Self {
        Self {
            realtime: Some(RealtimeTickAccounting {
                gate,
                period_ns: AtomicU64::new(0),
                last_period: AtomicU64::new(u64::MAX),
                ticks: AtomicU64::new(0),
            }),
            ..Self::new()
        }
    }

    /// Returns continuous real-time ticks and their fixed period in nanoseconds.
    ///
    /// Returns `None` until the first enabled real-time tick, or when this
    /// stream has no real-time accounting. The period cannot change during
    /// the stream's lifetime. A wake or PI deboost may concurrently reset the count.
    pub fn realtime_ticks(&self) -> Option<(u64, core::num::NonZeroU64)> {
        let realtime = self.realtime.as_ref()?;
        let period = core::num::NonZeroU64::new(realtime.period_ns.load(Ordering::Acquire))?;
        Some((realtime.ticks.load(Ordering::Acquire), period))
    }

    pub(crate) fn sample_realtime(&self, wall_ns: u64, tick_ns: u64) {
        let Some(realtime) = &self.realtime else {
            return;
        };
        if realtime.gate.enabled_generation().is_none() {
            return;
        }
        assert_ne!(tick_ns, 0, "real-time tick period must be nonzero");
        let period = realtime.period_ns.load(Ordering::Relaxed);
        if period == 0 {
            realtime.period_ns.store(tick_ns, Ordering::Release);
        } else {
            assert_eq!(period, tick_ns, "real-time tick period changed");
        }
        // Owner-rq exclusion and migration handoff serialize every writer.
        // Only the count is consumed remotely; last_period is writer-local
        // bookkeeping represented atomically to keep the carrier safely shared.
        let current_period = wall_ns / tick_ns;
        if realtime.last_period.load(Ordering::Relaxed) == current_period {
            return;
        }
        realtime
            .last_period
            .store(current_period, Ordering::Relaxed);
        let ticks = realtime.ticks.load(Ordering::Relaxed);
        realtime
            .ticks
            .store(ticks.saturating_add(1), Ordering::Release);
    }

    pub(crate) fn reset_realtime(&self) {
        if let Some(realtime) = &self.realtime {
            // Resetting the count does not permit a second charge in this period.
            realtime.ticks.store(0, Ordering::Release);
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
