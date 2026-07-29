use super::*;

/// Applies userspace disable intent and fences any live owner-CPU generation.
pub(crate) fn disable_counter(ptc: &Arc<PerTaskCounter>) -> AxResult<()> {
    ptc.enabled.store(false, Ordering::Release);
    // Never retain the non-sleeping run-state lock across an owner-CPU worker
    // rendezvous. See `stop_requested_on_owner` for the temporary-lifetime trap.
    let action = ptc.run_state.lock().begin_disable();
    let result = match action {
        PmuCloseAction::AlreadyClosed | PmuCloseAction::Complete => Ok(()),
        PmuCloseAction::Stop(lease) => cpu_worker::stop_task_counter(Arc::clone(ptc), lease),
    };
    if result.is_ok() {
        ptc.publish_rdpmc_inactive();
    }
    result
}

/// Resets a task-bound count between two complete scheduling generations.
pub(crate) fn reset_counter(ptc: &Arc<PerTaskCounter>) -> AxResult<()> {
    // A one-shot owner snapshot is insufficient: the target can migrate before
    // the fixed worker runs and start a new slice on another CPU. Quiescing the
    // exact generation first gives RESET one Linux-style context boundary.
    let was_enabled = ptc.enabled.swap(false, Ordering::AcqRel);
    if let Err(error) = disable_counter(ptc) {
        if was_enabled {
            ptc.set_enabled();
        }
        return Err(error);
    }
    ptc.accumulated.store(0, Ordering::Release);
    ptc.publish_rdpmc_inactive();
    if was_enabled && !ptc.resources_released() {
        ptc.set_enabled();
        ptc.synchronize_context()?;
    }
    Ok(())
}
