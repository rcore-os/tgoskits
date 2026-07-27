//! Recursion guard for deferred scheduler tracing.

/// Reports whether one scheduler transition should enter the deferred trace ring.
///
/// Disabled tracepoints must impose no capture or wake work. Transitions of
/// either tracing service worker are infrastructure events: recording the
/// scheduler worker directly wakes itself, while recording the pipe worker can
/// form a scheduler-worker -> pipe-worker -> scheduler-worker feedback loop.
#[inline]
pub(crate) fn should_defer_sched_switch(
    trace_enabled: bool,
    worker_ids: [u64; 2],
    previous_thread: u64,
    next_thread: u64,
) -> bool {
    trace_enabled
        && worker_ids.iter().all(|worker_id| {
            *worker_id == 0 || (previous_thread != *worker_id && next_thread != *worker_id)
        })
}
