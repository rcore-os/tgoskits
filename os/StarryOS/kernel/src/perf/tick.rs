//! Per-CPU perf tick (Tier-2 multiplexing).
//!
//! Registered with the periodic scheduler tick via [`ax_task::set_perf_tick`] at
//! [`super::perf_event_init`], this runs in timer-IRQ context on each core and
//! drives counter rotation for the currently-running task: when a task has more
//! enabled counting events than the core has programmable counters, the events
//! take turns on hardware so each is sampled over time (and `time_running <
//! time_enabled` lets `perf` scale the counts).
//!
//! It is alloc-free and takes no sleeping locks, like the overflow handler.

/// Invoked from every periodic scheduler tick (see [`ax_task::set_perf_tick`]).
///
/// `_scheduler_tick` is always `true` here (the hook is called only on the
/// scheduler tick); rotation advances once per call.
pub fn perf_tick(_scheduler_tick: bool) {
    super::task::perf_rotate_current();
}
