use alloc::{sync::Arc, vec::Vec};
use core::ffi::c_long;

use ax_runtime::hal::time::TimeValue;
use ax_task::{AxTaskRef, TaskInner, current};
use axpoll::IoEvents;
use bytemuck::AnyBitPattern;
use linux_raw_sys::general::ROBUST_LIST_LIMIT;
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};

use super::{
    AsThread, FutexKey, ProcessData, Thread, TimerState, ZombieSnapshot, futex_table_for_process,
    get_process_data_by_number, orphan_reaper_for, processes, publish_zombie,
    register_process_identity, send_signal_thread_inner, send_signal_to_process,
    send_signal_to_thread,
};
use crate::{
    StarryError, StarryResult,
    mm::atomic_update_user_u32,
    task::{
        PgidNumber, PidIdentity, PidView, ProcessCpuTime, ProcessGroup, ROOT_PID_NS, Tgid,
        ThreadExit, Tid, TidNumber,
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

/// Add the task, the thread and possibly its process, process group and session
/// to the corresponding tables.
pub fn add_task_to_table(task: &AxTaskRef) {
    task.as_thread().attach_pid_task(task);
    let proc_data = &task.as_thread().proc_data;

    register_process_identity(proc_data);
}

/// Lists all tasks.
pub fn tasks() -> Vec<AxTaskRef> {
    ROOT_PID_NS
        .published_members()
        .into_iter()
        .filter(|identity| identity.has_role::<Tid>())
        .filter_map(|identity| identity.live_task())
        .collect()
}

/// Finds the task with the given typed root-namespace TID.
pub(crate) fn get_task_by_number(tid: TidNumber) -> StarryResult<AxTaskRef> {
    PidView::new(ROOT_PID_NS.clone())
        .resolve_thread(tid)?
        .live_task()
        .ok_or(StarryError::NoSuchProcess)
}

/// Finds a task using a typed TID in the calling thread's active PID namespace.
pub(crate) fn get_user_task_by_number(tid: TidNumber) -> StarryResult<AxTaskRef> {
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

/// Accumulates CPU time for `task` from a timer-tick IRQ context.
///
/// Unlike `poll_timer`, this never emits signals, making it safe to call
/// from interrupt handlers.
pub fn tick_cpu_time(task: &TaskInner) {
    let Some(thr) = task.try_as_thread() else {
        return;
    };
    let Ok(mut time) = thr.time.try_borrow_mut() else {
        // Reentrant borrow means the task is mid-state-transition; skip.
        return;
    };
    time.tick();
}

/// Returns the accumulated `(utime, stime)` for a task without side effects.
pub fn task_cpu_time(task: &TaskInner) -> (TimeValue, TimeValue) {
    let Some(thr) = task.try_as_thread() else {
        return (TimeValue::ZERO, TimeValue::ZERO);
    };
    let Ok(time) = thr.time.try_borrow() else {
        return (TimeValue::ZERO, TimeValue::ZERO);
    };
    time.output()
}

/// Poll the timer
pub fn poll_timer(task: &TaskInner) {
    let Some(thr) = task.try_as_thread() else {
        return;
    };
    let Ok(mut time) = thr.time.try_borrow_mut() else {
        // reentrant borrow, likely IRQ
        return;
    };
    let emitter = |signo| {
        send_signal_thread_inner(task, thr, SignalInfo::new_kernel(signo));
    };
    time.poll(emitter);
}

/// Poll the process-level POSIX timers.
pub fn poll_process_timer(identity: &Arc<crate::task::PidIdentity>) {
    if let Some(proc_data) = identity.live_data() {
        if proc_data.poll_real_timer() {
            let _ = super::send_signal_to_process_data(
                &proc_data,
                Some(SignalInfo::new_kernel(Signo::SIGALRM)),
            );
        }
        proc_data.posix_timers.poll_expired(identity, |sig| {
            let _ = super::send_signal_to_process_data(&proc_data, Some(sig));
        });
    }
}

/// Sets the timer state.
pub fn set_timer_state(task: &TaskInner, state: TimerState) {
    let Some(thr) = task.try_as_thread() else {
        return;
    };
    let Ok(mut time) = thr.time.try_borrow_mut() else {
        // reentrant borrow, likely IRQ
        return;
    };
    let emitter = |signo| {
        send_signal_thread_inner(task, thr, SignalInfo::new_kernel(signo));
    };
    time.poll(emitter);
    time.set_state(state);
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
    let Some(key) = FutexKey::new_for_process_teardown(proc_data, address) else {
        warn!("robust futex wake skipped because the process MM is unavailable");
        return;
    };

    let futex_table = futex_table_for_process(proc_data, &key);

    let Some(futex) = futex_table.get(&key) else {
        return;
    };
    futex.wq.wake(1, u32::MAX);
}

fn handle_futex_death(
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
    let value = atomic_update_user_u32(futex_word, |value| {
        let owner = value & FUTEX_TID_MASK;
        if (pending && owner == 0) || owner != owner_tid {
            return Ok(value);
        }
        Ok((value & FUTEX_WAITERS) | FUTEX_OWNER_DIED)
    })?;
    let owner = value & FUTEX_TID_MASK;

    if pending && owner == 0 {
        wake_robust_futex(&thr.proc_data, address);
        return Ok(());
    }

    if owner != owner_tid {
        return Ok(());
    }
    if value & FUTEX_WAITERS != 0 {
        wake_robust_futex(&thr.proc_data, address);
    }
    Ok(())
}

pub fn exit_robust_list(thr: &Thread, head: *const RobustListHead) -> StarryResult<()> {
    // Reference: https://elixir.bootlin.com/linux/v6.13.6/source/kernel/futex/core.c#L777

    let mut limit = ROBUST_LIST_LIMIT;

    let end_ptr = head.cast::<RobustList>() as *mut RobustList;
    let head = head.vm_read()?;
    let mut entry = head.list.next;
    let offset = head.futex_offset;
    // Bit 0 marks PI futexes in Linux's robust-list ABI.  Starry handles only
    // regular futexes here, but the pointer still needs to be untagged.
    let pending = (head.list_op_pending as usize & !1) as *mut RobustList;

    while !core::ptr::eq(entry, end_ptr) {
        if entry.is_null() {
            break;
        }
        let Ok(node) = entry.vm_read() else {
            debug!("robust list: failed to read entry {entry:?}");
            break;
        };
        let next_entry = node.next;
        if entry != pending {
            handle_futex_death(thr, entry, offset, false).unwrap_or_else(|err| {
                debug!("robust list: failed to clean entry {entry:?}: {err:?}");
            });
        }
        entry = next_entry;

        limit -= 1;
        if limit == 0 {
            debug!("robust list: entry limit reached");
            break;
        }
        ax_task::yield_now();
    }

    // Process the pending entry that was skipped in the loop
    if !pending.is_null() && !core::ptr::eq(pending, end_ptr) {
        handle_futex_death(thr, pending, offset, true).unwrap_or_else(|err| {
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

pub fn do_exit(exit_code: i32, group_exit: bool) {
    let curr = current();
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
            let _ = send_signal_to_thread(None, tid, Some(sig.clone()));
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
        && let Err(err) = exit_robust_list(thr, head)
    {
        warn!("exit robust list failed: {err:?}");
    }

    let clear_child_tid = thr.clear_child_tid() as *mut u32;
    if clear_child_tid.vm_write(0).is_ok() {
        if let Some(key) =
            FutexKey::new_for_process_teardown(&thr.proc_data, clear_child_tid as usize)
        {
            let table = futex_table_for_process(&thr.proc_data, &key);
            let guard = table.get(&key);
            if let Some(futex) = guard {
                futex.wq.wake(1, u32::MAX);
            }
        }
        ax_task::yield_now();
    }

    let process = &thr.proc_data.proc;

    // A thread may own a private fd table after unshare(CLONE_FILES) or
    // close_range(CLOSE_RANGE_UNSHARE). Release it when that thread exits;
    // shared tables remain alive until their final sharer exits.
    crate::file::close_all_fds();

    // Use the user-visible TID (`thr.tid()`), not the scheduler ID. After
    // a non-leader `execve`'s de_thread the two differ, and the thread
    // group is keyed by the user-visible TID.
    let (utime, stime) = task_cpu_time(&curr);
    let process_identity = thr.proc_data.identity();
    let task_identity = thr.pid_identity();
    let thread_exit = process.exit_thread(
        thr.tid_number(),
        exit_code,
        ProcessCpuTime::new(utime, stime),
    );
    let cgroup_exit = match thread_exit {
        ThreadExit::AlreadyExited => None,
        ThreadExit::Remaining => Some(ax_cgroup::CgroupTaskExit::Thread),
        ThreadExit::Last(_) => Some(ax_cgroup::CgroupTaskExit::LastProcessTask),
    };
    if let Some(exit_kind) = cgroup_exit
        && let Err(error) = crate::cgroup::exit_task(&process_identity, &task_identity, exit_kind)
    {
        warn!("failed to release cgroup task charge: {error}");
    }
    if let ThreadExit::Last(process_cpu_time) = thread_exit {
        thr.proc_data.nsproxy.lock().release_cgroup_namespace();

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

        // Snapshot children before reparenting them. Otherwise
        // process.children() returns an empty
        // list and pdeathsig never reaches the real children.
        let children_snapshot = process.children();
        let orphan_reaper = orphan_reaper_for(process);
        process.reparent_children_to(&orphan_reaper);

        // Freeze all Linux-visible exit data in the generation-specific PID
        // identity. This is the sole Live -> Zombie state transition.
        let zombie_cred = thr.cred();
        let ptrace_tracer = thr.proc_data.ptrace_tracer_identity();
        let is_clone_child = thr.proc_data.is_clone_child();
        let wait_parent_tid = thr.proc_data.wait_parent_tid;

        // A parent that observes this child as a zombie must not see IPC
        // resources that still belong to the exiting process. In particular,
        // a vfork parent resumes only after this cleanup.
        if let Ok(aspace) = thr.proc_data.pin_aspace() {
            crate::syscall::clear_proc_shm(
                process_identity_id,
                process.identity().snapshot(),
                &aspace,
            );
        } else {
            warn!("shared-memory exit cleanup skipped for an unavailable MM");
        }

        // Release the process owner before publishing the zombie.  The typed
        // MM lifecycle defers reclaim until all kernel pins and activations
        // have quiesced, so this path cannot clear a root still in use.
        thr.proc_data.retire_mm_owner();

        publish_zombie(
            &thr.proc_data,
            ZombieSnapshot {
                cred: zombie_cred,
                ptrace_tracer: ptrace_tracer.as_ref().map(|identity| identity.snapshot()),
                is_clone_child,
                wait_parent_tid,
                cpu_time: process_cpu_time,
                tgid_lease: thr.proc_data.take_tgid_lease(),
            },
        )
        .expect("last process thread must own one live PID identity");
        if let Some(parent) = process.parent() {
            if let Some(signo) = thr.proc_data.exit_signal {
                use starry_signal::Signo;

                let child_uid = thr.cred().uid;
                let (code, status) = decode_wait_status(process.exit_code());

                let sig = if signo == Signo::SIGCHLD {
                    let child_pid = process
                        .identity()
                        .visible_number(&parent.identity().active_namespace())
                        .expect("child process must be visible to its parent")
                        .get();
                    SignalInfo::new_sigchld(child_pid, child_uid, code, status)
                } else {
                    SignalInfo::new_kernel(signo)
                };
                let _ = send_signal_to_process(parent.pid_number(), Some(sig));
            }
            if let Ok(data) = get_process_data_by_number(parent.pid_number()) {
                // Child exit state is published before waking waiters.
                unsafe { data.child_exit_event.wake(axpoll::IoEvents::IN) };
            }
        }
        if let Some(tracer) = ptrace_tracer
            && process
                .parent()
                .is_none_or(|parent| !Arc::ptr_eq(&parent.identity(), &tracer))
            && let Some(data) = tracer.live_data()
        {
            // Child exit state is published before waking waiters.
            unsafe { data.child_exit_event.wake(axpoll::IoEvents::IN) };
        }
        // Send pdeathsig to child processes
        for child in children_snapshot {
            let child_tid = TidNumber::from(child.pid_number().pid_number());
            if let Ok(child_task) = get_task_by_number(child_tid)
                && let Some(child_thr) = child_task.try_as_thread()
            {
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

        // If this process was the init of a non-root PID namespace,
        // send SIGKILL to all remaining processes in that namespace
        // (Linux: zap_pid_ns_processes).
        {
            let pid_ns = thr.active_pid_namespace();
            let identity = thr.proc_data.identity();
            if pid_ns.level() > 0 && pid_ns.init_identity() == Some(identity.id()) {
                let shutdown = pid_ns
                    .begin_shutdown(identity.id())
                    .expect("PID namespace init failed to enter shutdown");
                let sig = SignalInfo::new_kernel(Signo::SIGKILL);
                for victim in pid_ns.published_members() {
                    if victim.id() != identity.id()
                        && victim.has_role::<Tgid>()
                        && let Ok(process) = victim.public_process()
                    {
                        let _ = send_signal_to_process(process.pid_number(), Some(sig.clone()));
                        // A descendant may be parked on a raw `WaitQueue`
                        // (pipe/futex/filesystem wait) where `interrupt()` does
                        // not schedule it. The fatal signal is published first;
                        // force-wake every runtime thread so one observes
                        // SIGKILL, starts group exit, and retires the stable PID
                        // identity instead of pinning namespace shutdown.
                        for tid in process.threads() {
                            if let Ok(task) = get_task_by_number(tid) {
                                ax_task::wake_task(&task);
                            }
                        }
                    }
                }
                shutdown.wait_for_live_descendants();
            }
        }

        // Process exit state is published before waking pidfd/wait waiters.
        unsafe {
            thr.proc_data
                .exit_event
                .wake(IoEvents::IN | IoEvents::RDNORM);
        };

        // Unblock a vfork parent waiting for this child to exit.
        thr.proc_data.notify_vfork_done();
    }
    // Thread exit state is published before waking waiters.
    unsafe { thr.exit_event.wake(axpoll::IoEvents::IN) };
    unsafe { thr.proc_data.thread_exit_event.wake(axpoll::IoEvents::IN) };

    thr.retire_pid();
    thr.set_exit();
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
    let thr = task
        .try_as_thread()
        .ok_or(StarryError::OperationNotPermitted)?;
    thr.set_exit_request();
    // Poll-based I/O registers an interrupt waker, but some kernel waits can
    // still park directly on a raw `WaitQueue`. `wake_task` covers both forms:
    // the sibling is made runnable, observes the pending exit request, and
    // completes `do_exit` without depending on an unrelated future wakeup.
    ax_task::wake_task(&task);
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
