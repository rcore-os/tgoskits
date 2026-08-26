use alloc::{sync::Arc, vec::Vec};
use core::ffi::c_long;

use ax_runtime::hal::time::TimeValue;
use axpoll::IoEvents;
use bytemuck::AnyBitPattern;
use linux_raw_sys::general::ROBUST_LIST_LIMIT;
use starry_signal::{SignalInfo, Signo};

use super::{
    AlarmTarget, AlarmToken, PendingTimerActions, ProcessData, Thread, TimerState, UserTaskRef,
    ZombieSnapshot, current_user_task, processes, publish_zombie,
    resolve_futex_for_process_teardown, send_signal_to_process, send_signal_to_process_data,
    send_signal_to_thread, yield_now,
};
use crate::{
    StarryError, StarryResult,
    mm::{VmMutPtr, VmPtr},
    task::{
        PgidNumber, PidIdentity, PidNamespaceLifecycle, PidNamespaceRef, PidView, Process,
        ProcessCpuTime, ProcessGroup, ROOT_PID_NS, Tgid, ThreadExit, Tid, TidNumber,
    },
};

const FUTEX_OWNER_DIED: u32 = 0x40000000;
const FUTEX_TID_MASK: u32 = 0x3fffffff;
const FUTEX_WAITERS: u32 = 0x80000000;

/// Decode the Linux wait-status encoding into (si_code, si_status).
///
/// - Normal exit (`_exit`/`exit_group`): `(CLD_EXITED, exit_value)`
/// - Killed by signal: `(CLD_KILLED, signum)` or `(CLD_DUMPED, signum)`
pub fn decode_wait_status(raw: i32) -> (i32, i32) {
    use linux_raw_sys::general::{CLD_DUMPED, CLD_EXITED, CLD_KILLED};
    if raw & 0x7f == 0 {
        (CLD_EXITED as i32, (raw >> 8) & 0xff)
    } else {
        let signum = raw & 0x7f;
        if (raw & 0x80) != 0 {
            (CLD_DUMPED as i32, signum)
        } else {
            (CLD_KILLED as i32, signum)
        }
    }
}

/// PID indexes own no expired weak-map entries; retained for memtrack's hook.
#[cfg(feature = "memtrack")]
pub fn cleanup_task_tables() {}

/// Lists all tasks.
pub fn tasks() -> Vec<UserTaskRef> {
    ROOT_PID_NS
        .published_members()
        .into_iter()
        .filter(|identity| identity.has_role::<Tid>())
        .filter_map(|identity| identity.live_task())
        .collect()
}

/// Finds the task with the given typed root-namespace TID.
pub(crate) fn get_task_by_number(tid: TidNumber) -> StarryResult<UserTaskRef> {
    PidView::new(ROOT_PID_NS.clone())
        .resolve_thread(tid)?
        .live_task()
        .ok_or(StarryError::NoSuchProcess)
}

/// Finds a task using a typed TID in the calling thread's active PID namespace.
pub(crate) fn get_user_task_by_number(tid: TidNumber) -> StarryResult<UserTaskRef> {
    super::current_pid_view()
        .resolve_thread(tid)?
        .live_task()
        .ok_or(StarryError::NoSuchProcess)
}

/// Detach every live tracee that still points at `tracer_pid`.
///
/// A ptrace relationship must not outlive the tracer. Otherwise a tracee can
/// remain stuck in ptrace-stop with a dead tracer PID, or resume later with
/// stale ptrace state still armed. Either outcome is unsafe during task-exit
/// cleanup paths. Clearing the stop state wakes any tracee blocked in
/// `ptrace_stop_current()` so it can continue without consulting the dead
/// tracer again.
pub fn detach_live_tracees_of(tracer: &Arc<PidIdentity>) {
    if !tracer.may_have_ptrace_tracees() {
        return;
    }
    for tracee in processes() {
        if !tracee
            .ptrace_tracer_identity()
            .is_some_and(|registered| Arc::ptr_eq(&registered, tracer))
        {
            continue;
        }
        tracee.clear_ptrace_stop();
        tracee.clear_ptrace_traceme();
        tracee.clear_ptrace_attached();
        tracee.clear_ptrace_tracer();
        tracee.set_ptrace_options(0);
    }
}

/// Finds the process group with the given typed root-namespace PGID.
pub(crate) fn get_process_group_by_number(pgid: PgidNumber) -> StarryResult<Arc<ProcessGroup>> {
    PidView::new(ROOT_PID_NS.clone()).resolve_group(pgid)
}

/// Returns the accumulated `(utime, stime)` for a task without side effects.
pub fn task_cpu_time(task: &UserTaskRef) -> (TimeValue, TimeValue) {
    task.as_thread().cpu_time().output()
}

fn apply_process_timer_actions(proc_data: &ProcessData, pending: PendingTimerActions) {
    let pid = proc_data.proc.pid_number();
    for signo in pending.signals() {
        let _ = send_signal_to_process(pid, Some(SignalInfo::new_kernel(signo)));
    }
    pending.apply_alarms(AlarmTarget::Process(Arc::downgrade(&proc_data.identity())));
}

fn sample_interval_timer_cpu_time_if_active<T>(
    active: bool,
    sample: impl FnOnce() -> T,
) -> Option<T> {
    active.then(sample)
}

#[cfg(axtest)]
fn inactive_interval_timer_poll_skips_cpu_time_sample_for_test() -> bool {
    let samples = core::cell::Cell::new(0);
    let snapshot = sample_interval_timer_cpu_time_if_active(false, || {
        samples.set(samples.get() + 1);
        ()
    });
    snapshot.is_none() && samples.get() == 0
}

#[cfg(all(test, axtest))]
mod axtests {
    #[axtest::axtest]
    fn inactive_interval_timer_poll_skips_cpu_time_sample() {
        assert!(super::inactive_interval_timer_poll_skips_cpu_time_sample_for_test());
    }
}

fn poll_interval_timers(proc_data: &ProcessData, token: Option<&AlarmToken>) {
    let Some(snapshot) =
        sample_interval_timer_cpu_time_if_active(proc_data.has_active_interval_timers(), || {
            proc_data.cpu_time_snapshot()
        })
    else {
        return;
    };
    if let Some(pending) = proc_data.poll_interval_timers(snapshot, token) {
        apply_process_timer_actions(proc_data, pending);
    }
}

pub(crate) fn poll_process_cpu_timers_from_scheduler_tick(proc_data: &ProcessData) {
    if !proc_data.has_active_cpu_interval_timers() {
        return;
    }
    let snapshot = proc_data.scheduler_tick_cpu_time_snapshot();
    if let Some(pending) = proc_data.poll_cpu_interval_timers(snapshot) {
        apply_process_timer_actions(proc_data, pending);
    }
}

/// Polls process interval and POSIX timers from a retained process view.
pub(crate) fn poll_process_timers(proc_data: &ProcessData) {
    poll_interval_timers(proc_data, None);
    if proc_data.posix_timers().has_armed_timers() {
        proc_data.posix_timers().poll_expired(
            AlarmTarget::Process(Arc::downgrade(&proc_data.identity())),
            |sig| {
                let _ = send_signal_to_process(proc_data.proc.pid_number(), Some(sig));
            },
        );
    }
}

pub(crate) fn poll_process_timer_for_alarm(identity: &Arc<PidIdentity>, token: &AlarmToken) {
    if let Some(proc_data) = identity.live_data() {
        poll_interval_timers(&proc_data, Some(token));
        proc_data.posix_timers().poll_expired_for(
            AlarmTarget::Process(Arc::downgrade(identity)),
            token,
            |sig| {
                let _ = send_signal_to_process(proc_data.proc.pid_number(), Some(sig));
            },
        );
    }
}

/// Sets the current thread's user/kernel accounting state.
pub(crate) fn set_timer_state(thr: &Thread, state: TimerState) {
    thr.set_cpu_time_state(state);
    poll_interval_timers(&thr.proc_data, None);
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AnyBitPattern)]
pub struct RobustList {
    pub next: *mut RobustList,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AnyBitPattern)]
pub struct RobustListHead {
    pub list: RobustList,
    pub futex_offset: c_long,
    pub list_op_pending: *mut RobustList,
}

fn robust_futex_address(entry: *mut RobustList, offset: i64) -> StarryResult<usize> {
    let address = (entry as u64)
        .checked_add_signed(offset)
        .ok_or(StarryError::InvalidInput)?;
    let address = usize::try_from(address).map_err(|_| StarryError::InvalidInput)?;
    if address % size_of::<u32>() != 0 {
        return Err(StarryError::InvalidInput);
    }
    Ok(address)
}

fn wake_robust_futex(proc_data: &ProcessData, address: usize) {
    resolve_futex_for_process_teardown(proc_data, address).wake(1, u32::MAX);
}

fn handle_futex_death(
    current: &UserTaskRef,
    thr: &Thread,
    entry: *mut RobustList,
    offset: i64,
    pending: bool,
) -> StarryResult<()> {
    let address = robust_futex_address(entry, offset)?;
    let futex_word = address as *mut u32;
    // Linux compares the robust-futex owner field against task_pid_vnr(curr),
    // i.e. the user-visible TID written by userspace through gettid().
    // After non-leader execve, that value is the thread's active-namespace TID,
    // not its root-namespace TID or scheduler task id.
    let owner_tid = thr.user_tid().get() & FUTEX_TID_MASK;
    let value = futex_word.vm_read(current)?;
    let owner = value & FUTEX_TID_MASK;

    if pending && owner == 0 {
        wake_robust_futex(&thr.proc_data, address);
        return Ok(());
    }

    if owner != owner_tid {
        return Ok(());
    }
    futex_word.vm_write(current, (value & FUTEX_WAITERS) | FUTEX_OWNER_DIED)?;
    if value & FUTEX_WAITERS != 0 {
        wake_robust_futex(&thr.proc_data, address);
    }
    Ok(())
}

pub fn exit_robust_list(
    current: &UserTaskRef,
    thr: &Thread,
    head: *const RobustListHead,
) -> crate::StarryResult<()> {
    // Reference: https://elixir.bootlin.com/linux/v6.13.6/source/kernel/futex/core.c#L777

    let mut limit = ROBUST_LIST_LIMIT;

    let end_ptr = head.cast::<RobustList>() as *mut RobustList;
    let head = head.vm_read(current)?;
    let mut entry = head.list.next;
    let offset = head.futex_offset;
    // Bit 0 marks PI futexes in Linux's robust-list ABI.  Starry handles only
    // regular futexes here, but the pointer still needs to be untagged.
    let pending = (head.list_op_pending as usize & !1) as *mut RobustList;

    while !core::ptr::eq(entry, end_ptr) {
        if entry.is_null() {
            break;
        }
        let Ok(node) = entry.vm_read(current) else {
            debug!("robust list: failed to read entry {entry:?}");
            break;
        };
        let next_entry = node.next;
        if entry != pending {
            handle_futex_death(current, thr, entry, offset, false).unwrap_or_else(|err| {
                debug!("robust list: failed to clean entry {entry:?}: {err:?}");
            });
        }
        entry = next_entry;

        limit -= 1;
        if limit == 0 {
            debug!("robust list: entry limit reached");
            break;
        }
        yield_now();
    }

    // Process the pending entry that was skipped in the loop
    if !pending.is_null() && !core::ptr::eq(pending, end_ptr) {
        handle_futex_death(current, thr, pending, offset, true).unwrap_or_else(|err| {
            debug!("robust list: failed to clean pending entry {pending:?}: {err:?}");
        });
    }

    Ok(())
}

// The `sched:sched_process_exit` tracepoint is defined here, next to its sole
// emission site in `do_exit`, so the event schema and the fast-path call stay
// together. Registration into the global `.tracepoint` section is by link
// section, so the definition's module location is immaterial to discovery.
ax_tracepoint::define_event_trace!(
    sched_process_exit,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(sched),
    TP_PROTO(tid: u64, exit_code: i32),
    TP_STRUCT__entry {
        tid: u64,
        exit_code: i32,
    },
    TP_fast_assign {
        tid: tid,
        exit_code: exit_code,
    },
    TP_ident(__entry),
    TP_printk({
        alloc::format!(
            "tid={} exit_code={}",
            __entry.tid,
            __entry.exit_code,
        )
    })
);

fn emit_sched_process_exit(tid: TidNumber, exit_code: i32) {
    trace_sched_process_exit(tid.get() as u64, exit_code);
}

fn close_process_relations_for_exit(
    process: &Arc<Process>,
    pid_namespace: &PidNamespaceRef,
) -> Vec<Arc<Process>> {
    loop {
        if pid_namespace.lifecycle() == PidNamespaceLifecycle::ShuttingDown {
            return process
                .begin_namespace_shutdown_relations()
                .into_retained_children();
        }

        let orphan_reaper = super::orphan_reaper_for(process);
        if let Some(relations) = process.try_begin_exit_relations(&orphan_reaper) {
            return relations.into_reparented_children();
        }
    }
}

pub fn do_exit(exit_code: i32, group_exit: bool) {
    let curr = current_user_task();
    let thr = curr.as_thread();
    if !thr.begin_exit() {
        return;
    }

    info!("{} exit with code: {}", curr.id_name(), exit_code);

    emit_sched_process_exit(thr.tid(), exit_code);

    if group_exit && let Some(tids) = thr.proc_data.proc.start_group_exit(exit_code) {
        let sig = SignalInfo::new_kernel(Signo::SIGKILL);
        for tid in tids {
            if tid == thr.tid_number() {
                continue;
            }
            let _ = send_signal_to_thread(None, tid, Some(sig));
            let _ = zap_thread(tid);
        }
    }

    // Free any per-task perf HW counters attached to this thread before the fd
    // table is torn down, so the PMU slots are released even if a perf fd
    // outlives the task (its own `Drop::free_hw` is idempotent). Runs for every
    // exiting thread, not just the last in the group.
    #[cfg(target_arch = "aarch64")]
    crate::perf::task::on_task_exit(thr);

    // Robust futex ownership must be released before clone-child-tid wakes a
    // pthread joiner; otherwise userspace can observe thread exit before the
    // OWNER_DIED handoff has been written.
    let head = thr.robust_list_head() as *const RobustListHead;
    if !head.is_null()
        && let Err(err) = exit_robust_list(&curr, thr, head)
    {
        warn!("exit robust list failed: {err:?}");
    }

    let clear_child_tid = thr.clear_child_tid() as *mut u32;
    if clear_child_tid.vm_write(&curr, 0).is_ok() {
        resolve_futex_for_process_teardown(&thr.proc_data, clear_child_tid as usize)
            .wake(1, u32::MAX);
        yield_now();
    }

    let process = &thr.proc_data.proc;

    // A thread may own a private fd table after unshare(CLONE_FILES) or
    // close_range(CLOSE_RANGE_UNSHARE). Release it when that thread exits;
    // shared tables remain alive until their final sharer exits.
    crate::file::close_all_fds();

    // Match Linux exit_mm(): every thread leaves its user mm in task context
    // before it retires from the thread group. Consequently ThreadExit::Last
    // proves that all scheduler address-space slots are detached before the
    // process slot is released and the zombie becomes waitable.
    ax_runtime::task::detach_current_address_space()
        .unwrap_or_else(|error| panic!("failed to detach exiting task address space: {error}"));

    // Use the user-visible TID (`thr.tid()`), not the scheduler ID. After
    // a non-leader `execve`'s de_thread the two differ, and the thread
    // group is keyed by the user-visible TID.
    let is_process_leader = thr.tid().pid_number() == process.pid().pid_number();
    thr.account_cpu_time_now();
    let (utime, stime) = task_cpu_time(&curr);
    let task_identity = thr.pid_identity();
    // The lease keeps this identity's exit path pending until the tail of
    // `do_exit`, covering zombie publication, parent notification, and
    // relation close the way Linux holds `pid_allocated` until `free_pid()`.
    let exit_path = if is_process_leader {
        // Publish the complete leader snapshot before dropping the thread-group
        // lock below. A peer may become the final exiting thread immediately
        // after the leader is removed from the group.
        let (tid_lease, exit_path) = thr.retire_pid_retaining_tid();
        thr.proc_data.retire_leader(thr.nice(), tid_lease);
        exit_path
    } else {
        thr.retire_pid()
    };
    let task_generation = ax_cgroup::ProcessId::new(task_identity.id().get())
        .expect("PID identity generation must be non-zero");
    let (thread_exit, cgroup_exit) = thr.proc_data.finish_thread_exit(task_generation, || {
        process.exit_thread(
            thr.tid_number(),
            exit_code,
            ProcessCpuTime::new(utime, stime),
        )
    });
    super::cgroup_exit_invariant::enforce(cgroup_exit);
    if let ThreadExit::Last(exit_owner) = thread_exit {
        debug_assert!(Arc::ptr_eq(exit_owner.process(), process));
        thr.proc_data.release_cgroup_namespace();
        thr.proc_data
            .cancel_interval_timer_alarm()
            .apply_cancellation();
        thr.proc_data.posix_timers().clear();

        // AIO contexts pin the process address space and may have worker tasks
        // waiting on outstanding requests. Tear them down before releasing the
        // process address-space slot.
        crate::syscall::cleanup_aio_contexts_for_process(thr.proc_data.identity().id());

        // Drop ptrace relationships owned by this process before publishing the
        // final zombie state. Tracees blocked in ptrace-stop must not retain a
        // dead tracer PID or stale stop context once the tracer is gone.
        detach_live_tracees_of(&process.identity());

        // Release all POSIX (fcntl) locks held by this pid. Linux releases
        // them implicitly via fl_release_private when the last fd referring
        // to the inode is closed; we track POSIX locks by pid rather than
        // by fd, so the cleanup happens here at process-exit time. Without
        // this, a child fork → F_SETLK → exit would permanently pin the
        // record in FCNTL_LOCKS and block all later acquirers.
        let process_identity_id = process.identity().id();
        crate::syscall::release_pid_locks(process_identity_id);
        crate::syscall::release_pid_flock_locks(process_identity_id);

        // PID namespace init owns the only namespace-shutdown transaction.
        // This includes root PID 1: unlike Linux's immortal global init,
        // Starry joins PID 1 and shuts the system down when its userspace
        // command completes. Close child publication before SIGKILL is
        // delivered so no fork can escape the victim snapshot. Normal exits
        // atomically reparent through the process topology transaction instead.
        let pid_ns = thr.active_pid_namespace();
        let identity = thr.proc_data.identity();
        let shutdown_executor = thr.pid_identity();
        let namespace_shutdown = if pid_ns.init_identity() == Some(identity.id()) {
            Some(
                pid_ns
                    .begin_shutdown(identity.id(), shutdown_executor.id())
                    .expect("PID namespace init failed to enter shutdown"),
            )
        } else {
            None
        };
        let children_snapshot = if namespace_shutdown.is_some() {
            process
                .begin_namespace_shutdown_relations()
                .into_retained_children()
        } else {
            close_process_relations_for_exit(process, &pid_ns)
        };

        if let Some(shutdown) = namespace_shutdown.as_ref() {
            let sig = SignalInfo::new_kernel(Signo::SIGKILL);
            for victim in pid_ns.published_members() {
                if victim.id() != identity.id()
                    && victim.has_role::<Tgid>()
                    && let Ok(victim_process) = victim.public_process()
                {
                    let _ = send_signal_to_process(victim_process.pid_number(), Some(sig));
                    // The fatal signal is published before interrupting every
                    // runtime thread, matching Linux's signal/wakeup ordering.
                    for tid in victim_process.threads() {
                        if let Ok(task) = get_task_by_number(tid) {
                            task.interrupt();
                        }
                    }
                }
            }
            shutdown.wait_for_descendants_exit();
        }

        // Freeze all Linux-visible exit data in the generation-specific PID
        // identity. This is the sole Live -> Zombie state transition.
        let zombie_cred = thr.cred();
        let ptrace_tracer = thr.proc_data.ptrace_tracer_identity();
        let is_clone_child = thr.proc_data.is_clone_child();
        let wait_parent_tid = thr.proc_data.wait_parent_tid();
        let (zombie_nice, leader_tid_lease) = thr.proc_data.take_retired_leader_for_zombie();

        // A parent that observes this child as a zombie must not see IPC
        // resources that still belong to the exiting process. In particular,
        // a vfork parent resumes only after this cleanup.
        crate::syscall::clear_proc_shm(
            process_identity_id,
            process.identity().snapshot(),
            &thr.proc_data.aspace(),
        );

        // Drop memfd inode accounting before waitpid returns (SMP); use
        // process_slots refcounting — not vm_aspace_shared + clear().
        thr.proc_data.release_aspace_slot_if_needed();

        publish_zombie(
            &thr.proc_data,
            ZombieSnapshot {
                cred: zombie_cred,
                nice: zombie_nice,
                ptrace_tracer: ptrace_tracer.as_ref().map(|identity| identity.snapshot()),
                is_clone_child,
                wait_parent_tid,
                cpu_time: exit_owner.cpu_time(),
                tid_lease: leader_tid_lease,
                tgid_lease: thr.proc_data.take_tgid_lease(),
            },
        )
        .expect("last process thread must own one live PID identity");
        if let Some(parent) = process.parent()
            && let Some(parent_data) = parent.identity().live_data()
        {
            if let Some(signo) = thr.proc_data.exit_signal() {
                use starry_signal::Signo;

                let child_uid = thr.cred().uid;
                let (code, status) = decode_wait_status(exit_owner.exit_code());

                let sig = if signo == Signo::SIGCHLD {
                    let child_pid = process
                        .identity()
                        .visible_number(&parent.identity().active_namespace())
                        .unwrap_or_else(|| {
                            panic!(
                                "child process must be visible to its parent: child id={:?} \
                                 snapshot={:?}, parent id={:?} snapshot={:?} parent active \
                                 ns={:?} lifecycle={:?}",
                                process.identity().id(),
                                process.identity().snapshot(),
                                parent.identity().id(),
                                parent.identity().snapshot(),
                                parent.identity().active_namespace().id(),
                                parent.identity().active_namespace().lifecycle(),
                            )
                        })
                        .get();
                    SignalInfo::new_sigchld(child_pid, child_uid, code, status)
                } else {
                    SignalInfo::new_kernel(signo)
                };
                let _ = send_signal_to_process_data(&parent_data, Some(sig));
            }
            // Child exit state is published before waking waiters.
            unsafe { parent_data.child_exit_event().wake(axpoll::IoEvents::IN) };
        }
        if let Some(tracer) = ptrace_tracer
            && process
                .parent()
                .is_none_or(|parent| !Arc::ptr_eq(&parent.identity(), &tracer))
            && let Some(data) = tracer.live_data()
        {
            // Child exit state is published before waking waiters.
            unsafe { data.child_exit_event().wake(axpoll::IoEvents::IN) };
        }
        // Send pdeathsig to child processes
        for child in children_snapshot {
            let child_tid = TidNumber::from(child.pid_number().pid_number());
            if let Ok(child_task) = get_task_by_number(child_tid) {
                let child_thr = child_task.as_thread();
                let sig = child_thr.pdeathsig();
                if sig > 0
                    && let Some(signo) = Signo::from_repr(sig as u8)
                {
                    let _ = send_signal_to_process(
                        child.pid_number(),
                        Some(SignalInfo::new_kernel(signo)),
                    );
                }
            }
        }

        // Process exit state is published before waking pidfd/wait waiters.
        unsafe {
            thr.proc_data
                .exit_event()
                .wake(IoEvents::IN | IoEvents::RDNORM);
        };

        // Unblock a vfork parent waiting for this child to exit.
        thr.proc_data.notify_vfork_done();
    }

    thr.set_exit();
    task_identity.notify_thread_pidfd_exit();
    unsafe { thr.exit_event().wake(axpoll::IoEvents::IN) };

    // The exit path is complete only after zombie publication, parent
    // notification, and relation close. PID namespace shutdown waits on this
    // completion instead of the early runtime-link detach, mirroring Linux's
    // `pid_allocated` drop in `free_pid()` — never before `do_notify_parent()`.
    exit_path.complete();
    // Exec observes transfer readiness from the exact retained PID identity.
    // Wake after completing that identity-owned exit path.
    unsafe { thr.proc_data.thread_exit_event().wake(axpoll::IoEvents::IN) };
}

/// Request a sibling thread to exit with thread-only semantics.
///
/// Sets the target's `exit_request` flag and interrupts it. On its next
/// return to user space, `check_signals` observes the flag and routes to
/// `do_exit(0, false)` — no `group_exit`, no fatal-signal cascade. Used by
/// `sys_execve` to reap siblings without dragging the calling thread (or
/// the soon-to-be-loaded image) into a process-fatal exit.
///
/// Best-effort: returns `Err` if the target tid is already gone or no
/// longer a user thread; callers should treat that as "already reaped".
pub fn zap_thread(tid: TidNumber) -> StarryResult<()> {
    let task = get_task_by_number(tid)?;
    let thr = task.as_thread();
    thr.set_exit_request();
    // Match Linux's pending-SIGKILL plus signal_wake_up pairing: the
    // interruption bit is the persistent reason that aborts a future wait,
    // while interrupt() also publishes the direct scheduler wake needed for
    // raw WaitQueue sleepers. A bare wake can be consumed before a
    // LocalExecutor commits to park.
    task.interrupt();
    Ok(())
}

#[cfg(all(test, not(axtest)))]
fn decode_wait_status_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::{CLD_DUMPED, CLD_EXITED, CLD_KILLED};

    // Normal exit: raw & 0x7f == 0 → (CLD_EXITED, exit_value).
    let (code, status) = decode_wait_status(0);
    assert!(code == CLD_EXITED as i32 && status == 0);

    let (code, status) = decode_wait_status(0x0100); // exit(1)
    assert!(code == CLD_EXITED as i32 && status == 1);

    let (code, status) = decode_wait_status(0xFF00); // exit(255)
    assert!(code == CLD_EXITED as i32 && status == 255);

    // Killed by signal (no core dump): (CLD_KILLED, signum).
    let (code, status) = decode_wait_status(9); // SIGKILL
    assert!(code == CLD_KILLED as i32 && status == 9);

    let (code, status) = decode_wait_status(11); // SIGSEGV
    assert!(code == CLD_KILLED as i32 && status == 11);

    // Killed by signal with core dump: (CLD_DUMPED, signum).
    let (code, status) = decode_wait_status(0x89); // SIGKILL | 0x80
    assert!(code == CLD_DUMPED as i32 && status == 9);

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn decode_wait_status_rules_hold() {
        assert!(super::decode_wait_status_rules_hold_for_test());
    }
}
