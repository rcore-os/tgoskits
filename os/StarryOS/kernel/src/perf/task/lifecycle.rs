use super::*;

/// Exec hook: the given (current) thread has committed a new image in `execve`.
///
/// Flips any `enable_on_exec` counter to `enabled` and — because the task is the
/// running task right now — programs it onto HW immediately via
/// [`perf_sched_in`]. The `running` flag inside `perf_sched_in` prevents
/// double-programming an already-enabled counter.
pub fn on_exec(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let now = now_ns();
    thr.perf_context().with_counters(|counters| {
        for ptc in counters.iter() {
            if ptc.run_state.lock().is_stopping() {
                continue;
            }
            if ptc.enable_on_exec && !ptc.enabled.swap(true, Ordering::AcqRel) {
                ptc.enabled_at_ns.store(now, Ordering::Release);
            }
        }
    });
    // Program the now-enabled counters onto HW for the current task. Takes the
    // list lock itself, so it is released above first.
    perf_sched_in(thr);
}

/// Build a side-band write target for `ptc` if it has a mapped ring and requested
/// any side-band record (`attr.comm`/`mmap2`/`task`); else `None`.
pub(super) fn sideband_target(ptc: &PerTaskCounter, pid: u32, tid: u32) -> Option<SidebandTarget> {
    if !(ptc.want_comm || ptc.want_mmap2 || ptc.want_task) {
        return None;
    }
    let ring = ptc.output.lock().effective()?.0;
    Some(SidebandTarget {
        ring,
        sample_type: ptc.sample_type,
        sample_id_all: ptc.sample_id_all,
        id: ptc.sample_id.load(Ordering::Relaxed),
        pid,
        tid,
    })
}

/// Task-exit hook: emit `PERF_RECORD_EXIT` (for `attr.task` events) then free
/// every HW counter the exiting thread still holds.
///
/// The EXIT record must be written *before* [`free_hw`] zeroes the ring geometry,
/// so it is emitted per counter just before that counter is freed; the exiting
/// thread is the subject and its parent (if any) supplies `ppid`/`ptid`.
///
/// `free_hw` is idempotent per counter; safe even if the perf fd is still open
/// (its `Drop` will call `free_hw` again and find it already freed).
pub fn on_task_exit(thr: &Thread) {
    // Closing and snapshotting share the same lock as attach. An open either
    // commits into this exact snapshot or observes the tombstone and returns
    // ESRCH; no counter can appear after cleanup has selected its ownership set.
    let counters = thr.perf_context().close_and_snapshot();
    if counters.is_empty() {
        return;
    }
    let pid = thr.proc_data.proc.pid();
    let tid = thr.tid();
    let (ppid, ptid) = match thr.proc_data.proc.parent() {
        // The parent process's tgid; its main-thread tid equals that tgid.
        Some(p) => {
            let ppid = p.pid();
            (ppid, ppid)
        }
        None => (0, 0),
    };
    for ptc in &counters {
        if ptc.want_task
            && let Some(t) = sideband_target(ptc, pid, tid)
        {
            sideband::emit_exit(&t, pid, ppid, tid, ptid);
        }
    }
    release_task_counters(thr, &counters);
}

/// Scheduler-lifetime fallback for prepared-task rollback and final teardown.
///
/// Linux-visible exit emits side-band records in [`on_task_exit`]. This later,
/// idempotent callback only guarantees that an unpublished or externally
/// terminated scheduler record cannot retain PMU reservations.
pub(crate) fn on_scheduler_task_exit(thr: &Thread) {
    let counters = thr.perf_context().close_and_snapshot();
    release_task_counters(thr, &counters);
}

fn release_task_counters(thr: &Thread, counters: &[Arc<PerTaskCounter>]) {
    for ptc in counters {
        if let Err(error) = free_hw(ptc) {
            warn!(
                "perf: task-exit failed to quiesce counter {:?} on tid {}: {error}",
                ptc.counter,
                thr.tid()
            );
        }
    }
}

/// Release the HW counter backing `ptc` and tear down its bookkeeping, once.
///
/// Idempotence and in-flight owner CPU identity are both held by
/// [`PmuRunState`] plus [`PmuResourceRelease`]. Either the fd side or task-exit
/// side may win the hardware stop; exactly one caller reclaims the reservation.
///
/// For a *sampling* counter that is currently armed, the overflow-IRQ path is
/// torn down in the UAF-safe order before the slot/ring `Arc`s drop: stop the
/// counter, mask the IRQ, then `unregister` the [`SampleSlot`] — so the overflow
/// handler can no longer reach the ring or notification anchor. An inherited
/// member then drops its redirect; the root output remains fd-owned until the
/// complete family has quiesced.
pub(crate) fn free_hw(ptc: &Arc<PerTaskCounter>) -> AxResult<()> {
    if ptc.resources_released() {
        return Ok(());
    }
    let close_action = ptc.run_state.lock().begin_close();
    let stop_result = match close_action {
        PmuCloseAction::AlreadyClosed | PmuCloseAction::Complete => Ok(()),
        PmuCloseAction::Stop(lease) => cpu_worker::stop_task_counter(Arc::clone(ptc), lease),
    };
    stop_result?;

    let Some(resource_claim) = ptc.resources.claim() else {
        return Ok(());
    };
    ptc.publish_rdpmc_inactive();
    // An inherited task has no fd-owned output lifetime of its own. Its EXIT
    // side-band record was emitted before this fence, so its strong redirect
    // can now be dropped. Withdraw the family relation before returning the PMU
    // slot so a concurrent clone cannot reserve the hardware and then observe a
    // stale full-family snapshot.
    if !ptc.is_family_root() {
        if let Some(family) = ptc.family() {
            family.retire_child(ptc);
        }
        ptc.clear_family_output();
    }
    super::hw_allocation::free_counter(ptc.counter);
    if resource_claim == PmuResourceClaim::Published {
        let previous = PERF_TASK_ACTIVE.fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "task perf active count underflow");
    }
    Ok(())
}
