use super::*;

/// Read back `(value, time_enabled, time_running)` for `read(perf_fd)`.
///
/// `value` is the accumulated delta plus the live slice if the counter is
/// currently running. For `perf stat -- cmd` the child has already exited by the
/// time the parent reads, so `running == false` and `accumulated` is final.
pub(crate) fn read_counter(ptc: &Arc<PerTaskCounter>) -> AxResult<(u64, u64, u64)> {
    let owner = ptc.run_state.lock().running().map(PmuRunLease::owner);
    if let Some(owner) = owner {
        cpu_worker::read_task_counter(Arc::clone(ptc), owner)
    } else {
        read_task_on_owner(ptc)
    }
}

/// Reads a task-bound event from a pinned owner worker or a detached state.
pub(crate) fn read_task_on_owner(ptc: &PerTaskCounter) -> AxResult<(u64, u64, u64)> {
    let mut value = ptc.accumulated.load(Ordering::Acquire);
    let mut time_enabled = ptc.time_enabled_ns.load(Ordering::Acquire);
    let mut time_running = ptc.time_running_ns.load(Ordering::Acquire);
    let run_state = ptc.run_state.lock();
    if let Some(lease) = run_state.running()
        && lease.owner().as_usize() == ax_hal::percpu::this_cpu_id()
    {
        // Live slice: add the in-progress count and elapsed time. This is a
        // local owner-CPU snapshot; remote reads are routed through the CPU
        // worker in the complete PMU ownership path.
        value += ptc.counter.read();
        let dt = now_ns().saturating_sub(ptc.last_in_ns.load(Ordering::Acquire));
        time_enabled += dt;
        time_running += dt;
    }
    Ok((value, time_enabled, time_running))
}
