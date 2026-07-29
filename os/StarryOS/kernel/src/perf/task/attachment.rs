use super::*;

/// Monotonic time source shared with the system-wide path.
#[inline]
pub(super) fn now_ns() -> u64 {
    ax_runtime::hal::time::monotonic_time_nanos()
}

/// Attaches `ptc` to `thr` and arms the scheduler hooks.
///
/// The thread context serializes this commit against task-exit tombstoning.
pub fn attach(thr: &Thread, ptc: Arc<PerTaskCounter>) -> AxResult<()> {
    thr.perf_context().attach(ptc)
}

/// Withdraws a counter whose family publication failed before its thread became
/// schedulable.
pub(in crate::perf) fn detach_unpublished(thr: &Thread, ptc: &Arc<PerTaskCounter>) {
    thr.perf_context().detach_unpublished(ptc);
}
