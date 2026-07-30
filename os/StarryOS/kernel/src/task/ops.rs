use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::ffi::c_long;

use ax_errno::{AxError, AxResult};
use ax_runtime::hal::time::TimeValue;
use ax_std::os::arceos::task::yield_current_cpu;
use ax_sync::PiMutex;
use axpoll::IoEvents;
use bytemuck::AnyBitPattern;
use linux_raw_sys::general::ROBUST_LIST_LIMIT;
use starry_process::{Pid, ProcessCpuTime, ProcessGroup, Session, ThreadExit};
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};
use weak_map::WeakMap;

use super::{
    AlarmTarget, AlarmToken, Cred, FutexKey, OrphanReaper, PendingTimerActions, ProcessData,
    Thread, TimerState, UserTaskRef, WeakUserTaskRef, ZombieSnapshot, current_user_task,
    futex_table_for_process, get_process_data, get_zombie_cred, is_zombie_process,
    namespace_shutdown_parent, orphan_reaper_for, process_belongs_to_pid_namespace, processes,
    publish_zombie, published_victim_tids, reap_process, register_prepared_process_identity,
    register_process_identity, release_thread_pid, send_signal_to_process, send_signal_to_thread,
    unregister_prepared_process_identity, wait_for_victims,
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

// These registries are task-context only. Their map operations may allocate,
// free, or upgrade scheduler handles, so contention must sleep with PI rather
// than spin with preemption enabled.
static TASK_TABLE: PiMutex<BTreeMap<Pid, WeakUserTaskRef>> = PiMutex::new(BTreeMap::new());

static PROCESS_GROUP_TABLE: PiMutex<WeakMap<Pid, Weak<ProcessGroup>>> =
    PiMutex::new(WeakMap::new());

static SESSION_TABLE: PiMutex<WeakMap<Pid, Weak<Session>>> = PiMutex::new(WeakMap::new());

fn remove_matching_entry<K, V>(
    entries: &mut BTreeMap<K, V>,
    key: &K,
    matches: impl FnOnce(&V) -> bool,
) -> bool
where
    K: Ord,
{
    if !entries.get(key).is_some_and(matches) {
        return false;
    }
    entries.remove(key);
    true
}

/// Cleanup expired entries in the task tables.
///
/// This function is intended to be used during memory leak analysis to remove
/// possible noise caused by expired entries in the [`WeakMap`].
#[cfg(feature = "memtrack")]
pub fn cleanup_task_tables() -> AxResult<()> {
    let mut invalid_extension = false;
    TASK_TABLE.lock().retain(|_, task| match task.upgrade() {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(_) => {
            invalid_extension = true;
            true
        }
    });
    PROCESS_GROUP_TABLE.lock().cleanup();
    SESSION_TABLE.lock().cleanup();
    if invalid_extension {
        Err(AxError::BadState)
    } else {
        Ok(())
    }
}

/// Add the task, the thread and possibly its process, process group and session
/// to the corresponding tables.
pub fn add_task_to_table(task: &UserTaskRef) {
    // Key by the user-visible thread tid, not the scheduler `task.id()`. The two
    // are equal for every task except the init process, whose pid/tid is pinned
    // to 1 while its scheduler id stays at whatever the allocator handed out
    // (see `entry::init`). All tid lookups (signals, get_task, ptrace) go
    // through this table, so they must agree with `Thread::tid`.
    let proc_data = &task.as_thread().proc_data;
    let tid = task.as_thread().tid() as Pid;

    let mut task_table = TASK_TABLE.lock();
    task_table.insert(tid, task.downgrade());
    drop(task_table);

    register_process_identity(proc_data);

    let proc = &proc_data.proc;
    let pg = proc.group();
    let mut pg_table = PROCESS_GROUP_TABLE.lock();
    if pg_table.contains_key(&pg.pgid()) {
        return;
    }
    pg_table.insert(pg.pgid(), &pg);
    drop(pg_table);

    let session = pg.session();
    let mut session_table = SESSION_TABLE.lock();
    if session_table.contains_key(&session.sid()) {
        return;
    }
    session_table.insert(session.sid(), &session);
}

/// Rollback token for a task registered before runtime entry activation.
///
/// Clone installs Linux-visible identity after fallible scheduler placement
/// while the runtime start gate remains closed. Dropping this token removes
/// only the matching generation and process object; [`Self::commit`] transfers
/// ownership to normal exit paths.
pub struct PreparedTaskRegistration {
    tid: Pid,
    scheduler_id: ax_std::os::arceos::task::ThreadId,
    process: Option<Arc<ProcessData>>,
    committed: bool,
}

impl PreparedTaskRegistration {
    /// Leaves the task and process entries installed for normal lifecycle code.
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for PreparedTaskRegistration {
    fn drop(&mut self) {
        if self.committed {
            return;
        }

        let mut task_table = TASK_TABLE.lock();
        remove_matching_entry(&mut task_table, &self.tid, |task| {
            task.scheduler_id() == self.scheduler_id
        });
        drop(task_table);

        let Some(process) = self.process.as_ref() else {
            return;
        };
        unregister_prepared_process_identity(process);
    }
}

/// Registers a staged task before its runtime entry is activated.
///
/// `new_process` must be true only for a freshly forked process. Existing
/// process entries belong to the thread group and must never be removed when a
/// sibling-thread creation rolls back.
pub fn register_prepared_task(
    task: &UserTaskRef,
    new_process: bool,
) -> AxResult<PreparedTaskRegistration> {
    let tid = task.as_thread().tid() as Pid;
    let scheduler_id = task.id();
    let mut task_table = TASK_TABLE.lock();
    if task_table.contains_key(&tid) {
        return Err(AxError::BadState);
    }
    task_table.insert(tid, task.downgrade());
    drop(task_table);

    let process = new_process.then(|| task.as_thread().proc_data.clone());
    if let Some(process) = process.as_ref()
        && let Err(error) = register_prepared_process_identity(process)
    {
        remove_matching_entry(&mut TASK_TABLE.lock(), &tid, |registered| {
            registered.scheduler_id() == scheduler_id
        });
        return Err(error);
    }

    Ok(PreparedTaskRegistration {
        tid,
        scheduler_id,
        process,
        committed: false,
    })
}

/// Detaches a live Linux TID from its scheduler-backed task.
///
/// Scheduler resources may remain reachable until deferred task work finishes,
/// but Linux-visible task lookup must stop at thread exit rather than inherit
/// that implementation lifetime.
pub fn remove_task_from_table(tid: Pid) {
    TASK_TABLE.lock().remove(&tid);
}

/// Lists all tasks.
pub fn tasks() -> AxResult<Vec<UserTaskRef>> {
    let table = TASK_TABLE.lock();
    let mut tasks = Vec::with_capacity(table.len());
    for task in table.values() {
        if let Some(task) = task.upgrade().map_err(|_| AxError::BadState)? {
            tasks.push(task);
        }
    }
    Ok(tasks)
}

/// Finds the task with the given TID.
pub fn get_task(tid: Pid) -> AxResult<UserTaskRef> {
    if tid == 0 {
        return Ok(current_user_task());
    }
    let weak = TASK_TABLE
        .lock()
        .get(&tid)
        .copied()
        .ok_or(AxError::NoSuchProcess)?;
    weak.upgrade()
        .map_err(|_| AxError::BadState)?
        .ok_or(AxError::NoSuchProcess)
}

/// Detach every live tracee that still points at `tracer_pid`.
///
/// A ptrace relationship must not outlive the tracer. Otherwise a tracee can
/// remain stuck in ptrace-stop with a dead tracer PID, or resume later with
/// stale ptrace state still armed. Either outcome is unsafe during task-exit
/// cleanup paths. Clearing the stop state wakes any tracee blocked in
/// `ptrace_stop_current()` so it can continue without consulting the dead
/// tracer again.
pub fn detach_live_tracees_of(tracer_pid: Pid) {
    for tracee in processes() {
        if tracee.ptrace_tracer_pid() != Some(tracer_pid) {
            continue;
        }
        tracee.clear_ptrace_stop();
        tracee.clear_ptrace_traceme();
        tracee.clear_ptrace_attached();
        tracee.clear_ptrace_tracer_pid();
        tracee.set_ptrace_options(0);
    }
}

/// Finds the credentials for a process that may already be a zombie.
pub fn get_process_cred(pid: Pid) -> AxResult<Arc<Cred>> {
    if pid == 0 {
        return Ok(current_user_task().as_thread().cred());
    }
    if let Ok(task) = get_task(pid) {
        return Ok(task.as_thread().cred());
    }
    get_zombie_cred(pid).ok_or(AxError::NoSuchProcess)
}

/// Finds the process group with the given PGID.
pub fn get_process_group(pgid: Pid) -> AxResult<Arc<ProcessGroup>> {
    if let Some(pg) = PROCESS_GROUP_TABLE.lock().get(&pgid) {
        return Ok(pg);
    }

    if let Some(pg) = find_process_group_by_member(pgid) {
        register_process_group(&pg);
        return Ok(pg);
    }

    Err(AxError::NoSuchProcess)
}

/// Registers a process group in the global table.
pub fn register_process_group(pg: &Arc<ProcessGroup>) {
    let mut pg_table = PROCESS_GROUP_TABLE.lock();
    pg_table.insert(pg.pgid(), pg);
}

fn find_process_group_by_member(pgid: Pid) -> Option<Arc<ProcessGroup>> {
    for proc_data in processes() {
        let pg = proc_data.proc.group();
        if pg.pgid() == pgid {
            return Some(pg);
        }
    }

    None
}

/// Registers a session in the global table.
pub fn register_session(session: &Arc<Session>) {
    let mut session_table = SESSION_TABLE.lock();
    session_table.insert(session.sid(), session);
}

/// Returns the accumulated `(utime, stime)` for a task without side effects.
pub fn task_cpu_time(task: &UserTaskRef) -> (TimeValue, TimeValue) {
    task.as_thread().cpu_time().output()
}

fn apply_process_timer_actions(pid: Pid, pending: PendingTimerActions) {
    for signo in pending.signals() {
        let _ = send_signal_to_process(pid, Some(SignalInfo::new_kernel(signo)));
    }
    pending.apply_alarms(AlarmTarget::Process(pid));
}

fn poll_interval_timers(proc_data: &ProcessData, token: Option<&AlarmToken>) {
    if !proc_data.has_active_interval_timers() {
        return;
    }
    let snapshot = proc_data.cpu_time_snapshot();
    if let Some(pending) = proc_data.poll_interval_timers(snapshot, token) {
        apply_process_timer_actions(proc_data.proc.pid(), pending);
    }
}

pub(crate) fn poll_process_cpu_timers_from_scheduler_tick(proc_data: &ProcessData) {
    if !proc_data.has_active_cpu_interval_timers() {
        return;
    }
    let snapshot = proc_data.scheduler_tick_cpu_time_snapshot();
    if let Some(pending) = proc_data.poll_cpu_interval_timers(snapshot) {
        apply_process_timer_actions(proc_data.proc.pid(), pending);
    }
}

/// Poll process interval timers and POSIX timers.
pub fn poll_process_timer(pid: Pid) {
    if let Ok(proc_data) = get_process_data(pid) {
        poll_interval_timers(&proc_data, None);
        if proc_data.posix_timers().has_armed_timers() {
            proc_data.posix_timers().poll_expired(pid, |sig| {
                let _ = send_signal_to_process(pid, Some(sig));
            });
        }
    }
}

pub(crate) fn poll_process_timer_for_alarm(pid: Pid, token: &AlarmToken) {
    if let Ok(proc_data) = get_process_data(pid) {
        poll_interval_timers(&proc_data, Some(token));
        proc_data
            .posix_timers()
            .poll_expired_for(pid, token, |sig| {
                let _ = send_signal_to_process(pid, Some(sig));
            });
    }
}

/// Sets the timer state.
pub fn set_timer_state(task: &UserTaskRef, state: TimerState) {
    let thr = task.as_thread();
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

fn robust_futex_address(entry: *mut RobustList, offset: i64) -> AxResult<usize> {
    let address = (entry as u64)
        .checked_add_signed(offset)
        .ok_or(AxError::InvalidInput)?;
    let address = usize::try_from(address).map_err(|_| AxError::InvalidInput)?;
    if address % size_of::<u32>() != 0 {
        return Err(AxError::InvalidInput);
    }
    Ok(address)
}

fn wake_robust_futex(proc_data: &ProcessData, address: usize) {
    let key = FutexKey::new_for_process_teardown(proc_data, address);

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
) -> AxResult<()> {
    let address = robust_futex_address(entry, offset)?;
    let futex_word = address as *mut u32;
    // Linux compares the robust-futex owner field against task_pid_vnr(curr),
    // i.e. the user-visible TID written by userspace through gettid().
    // After non-leader execve, that value is Thread::tid(), not the scheduler
    // task id.
    let owner_tid = thr.tid() & FUTEX_TID_MASK;
    let value = futex_word.vm_read()?;
    let owner = value & FUTEX_TID_MASK;

    if pending && owner == 0 {
        wake_robust_futex(&thr.proc_data, address);
        return Ok(());
    }

    if owner != owner_tid {
        return Ok(());
    }
    futex_word.vm_write((value & FUTEX_WAITERS) | FUTEX_OWNER_DIED)?;

    if value & FUTEX_WAITERS != 0 {
        wake_robust_futex(&thr.proc_data, address);
    }
    Ok(())
}

pub fn exit_robust_list(thr: &Thread, head: *const RobustListHead) -> AxResult<()> {
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
        let _decision = yield_current_cpu();
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
ktracepoint::define_event_trace!(
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

fn reap_shutdown_children(init: &Arc<ProcessData>, namespace: &crate::task::PidNamespaceRef) {
    for child in init.proc.children() {
        if !process_belongs_to_pid_namespace(&child, namespace) || !is_zombie_process(&child) {
            continue;
        }
        if let Some(cpu_time) = reap_process(&child) {
            init.add_child_cpu_time(cpu_time.user(), cpu_time.system());
        }
    }
}

fn terminate_pid_namespace_members(
    init: &Arc<ProcessData>,
    namespace: &crate::task::PidNamespaceRef,
) {
    let init_pid = init.proc.pid() as u64;

    let signal = SignalInfo::new_kernel(Signo::SIGKILL);
    for global_tid in published_victim_tids(namespace, init_pid) {
        let Ok(tid) = Pid::try_from(global_tid) else {
            continue;
        };
        let _ = send_signal_to_thread(None, tid, Some(signal));
        let _ = zap_thread(tid);
    }

    wait_for_victims(namespace, init_pid, || {
        reap_shutdown_children(init, namespace)
    });
}

pub fn do_exit(exit_code: i32, group_exit: bool) {
    let curr = current_user_task();
    let thr = curr.as_thread();
    if !thr.begin_exit() {
        return;
    }

    info!("{} exit with code: {}", curr.id_name(), exit_code);

    trace_sched_process_exit(curr.id().as_u64(), exit_code);

    if group_exit && let Some(tids) = thr.proc_data.proc.start_group_exit(exit_code) {
        let sig = SignalInfo::new_kernel(Signo::SIGKILL);
        for tid in tids {
            if tid == thr.tid() {
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
        && let Err(err) = exit_robust_list(thr, head)
    {
        warn!("exit robust list failed: {err:?}");
    }

    let clear_child_tid = thr.clear_child_tid() as *mut u32;
    if clear_child_tid.vm_write(0).is_ok() {
        let key = FutexKey::new_for_process_teardown(&thr.proc_data, clear_child_tid as usize);
        let table = futex_table_for_process(&thr.proc_data, &key);
        let guard = table.get(&key);
        if let Some(futex) = guard {
            futex.wq.wake(1, u32::MAX);
        }
        let _decision = yield_current_cpu();
    }

    let process = &thr.proc_data.proc;

    // A thread may own a private fd table after unshare(CLONE_FILES) or
    // close_range(CLOSE_RANGE_UNSHARE). Release it when that thread exits;
    // shared tables remain alive until their final sharer exits.
    crate::file::close_all_fds();

    // Use the user-visible TID (`thr.tid()`), not the scheduler ID. After
    // a non-leader `execve`'s de_thread the two differ, and the thread
    // group is keyed by the user-visible TID.
    let is_process_leader = thr.tid() == process.pid();
    if is_process_leader {
        // Preserve the leader's Linux priority before scheduler reachability is
        // removed. A peer may become the last exiting thread immediately after
        // the thread-group lock is released.
        thr.proc_data.retire_leader_nice(thr.nice());
    }
    thr.account_cpu_time_now();
    let (utime, stime) = task_cpu_time(&curr);
    let thread_exit = process.exit_thread(thr.tid(), exit_code, ProcessCpuTime::new(utime, stime));
    if !matches!(thread_exit, ThreadExit::Last(_)) {
        remove_task_from_table(thr.tid());
    }

    let mut shutdown_reap_parent = None;
    if let ThreadExit::Last(process_cpu_time) = thread_exit {
        crate::cgroup::exit_process(&thr.proc_data);
        thr.proc_data.release_cgroup_namespace();

        thr.proc_data
            .cancel_interval_timer_alarm()
            .apply_cancellation();
        thr.proc_data.posix_timers().clear();

        // AIO contexts pin the process address space and may have worker tasks
        // waiting on outstanding requests. Tear them down before releasing the
        // process address-space slot.
        crate::syscall::cleanup_aio_contexts_for_pid(process.pid());

        // Drop ptrace relationships owned by this process before publishing the
        // final zombie state. Tracees blocked in ptrace-stop must not retain a
        // dead tracer PID or stale stop context once the tracer is gone.
        detach_live_tracees_of(process.pid());

        // Release all POSIX (fcntl) locks held by this pid. Linux releases
        // them implicitly via fl_release_private when the last fd referring
        // to the inode is closed; we track POSIX locks by pid rather than
        // by fd, so the cleanup happens here at process-exit time. Without
        // this, a child fork → F_SETLK → exit would permanently pin the
        // record in FCNTL_LOCKS and block all later acquirers.
        crate::syscall::release_pid_locks(process.pid());
        crate::syscall::release_pid_flock_locks(process.pid());

        let (children_snapshot, shutting_down_pid_namespace) = loop {
            match orphan_reaper_for(&thr.proc_data) {
                OrphanReaper::ReparentTo(orphan_reaper) => {
                    if let Some(relations) = process.try_begin_exit_relations(&orphan_reaper) {
                        break (relations.into_reparented_children(), None);
                    }
                }
                OrphanReaper::ShutdownNamespace(namespace) => {
                    let publication = super::pid_namespace::lock_publication();
                    super::pid_namespace::begin_shutdown(
                        &publication,
                        &namespace,
                        process.pid() as u64,
                    );
                    let relations = process.begin_namespace_shutdown_relations();
                    drop(publication);
                    break (relations.into_retained_children(), Some(namespace));
                }
            }
        };

        if let Some(namespace) = shutting_down_pid_namespace.as_ref() {
            terminate_pid_namespace_members(&thr.proc_data, namespace);
        }

        // Freeze all Linux-visible exit data in the generation-specific PID
        // identity. This is the sole Live -> Zombie state transition.
        let zombie_cred = thr.cred();
        let ptrace_tracer_pid = thr.proc_data.ptrace_tracer_pid();
        let is_clone_child = thr.proc_data.is_clone_child();
        let wait_parent_tid = thr.proc_data.wait_parent_tid();

        // A parent that observes this child as a zombie must not see IPC
        // resources that still belong to the exiting process. In particular,
        // a vfork parent resumes only after this cleanup.
        crate::syscall::clear_proc_shm(process.pid(), &thr.proc_data.aspace());

        // Drop memfd inode accounting before waitpid returns (SMP); use
        // process_slots refcounting — not vm_aspace_shared + clear().
        thr.proc_data.release_aspace_slot_if_needed();

        let zombie_nice = if is_process_leader {
            thr.nice()
        } else {
            thr.proc_data.retired_leader_nice().unwrap_or_else(|| {
                warn!(
                    "missing retired leader nice for pid {}, using final thread {}",
                    process.pid(),
                    thr.tid()
                );
                thr.nice()
            })
        };
        publish_zombie(
            &thr.proc_data,
            ZombieSnapshot {
                cred: zombie_cred,
                nice: zombie_nice,
                ptrace_tracer_pid,
                is_clone_child,
                wait_parent_tid,
                cpu_time: process_cpu_time,
            },
        )
        .expect("last process thread must own one live PID identity");
        // Linux removes scheduler reachability independently of the stable PID
        // identity retained for wait and pidfd operations.
        remove_task_from_table(thr.tid());
        if let Some(parent) = process.parent() {
            if let Some(signo) = thr.proc_data.exit_signal() {
                use starry_signal::Signo;

                let child_uid = thr.cred().uid;
                let (code, status) = decode_wait_status(process.exit_code());

                let sig = if signo == Signo::SIGCHLD {
                    SignalInfo::new_sigchld(process.pid(), child_uid, code, status)
                } else {
                    SignalInfo::new_kernel(signo)
                };
                let _ = send_signal_to_process(parent.pid(), Some(sig));
            }
            if let Ok(data) = get_process_data(parent.pid()) {
                // Child exit state is published before waking waiters.
                unsafe { data.child_exit_event().wake(axpoll::IoEvents::IN) };
            }
        }
        if let Some(tracer_pid) = ptrace_tracer_pid
            && process
                .parent()
                .is_none_or(|parent| parent.pid() != tracer_pid)
            && let Ok(data) = get_process_data(tracer_pid)
        {
            // Child exit state is published before waking waiters.
            unsafe { data.child_exit_event().wake(axpoll::IoEvents::IN) };
        }
        // Send pdeathsig to child processes
        for child in children_snapshot {
            let child_pid = child.pid();
            if let Ok(child_task) = get_task(child_pid) {
                let child_thr = child_task.as_thread();
                let sig = child_thr.pdeathsig();
                if sig > 0
                    && let Some(signo) = Signo::from_repr(sig as u8)
                {
                    let _ = send_signal_to_process(child_pid, Some(SignalInfo::new_kernel(signo)));
                }
            }
        }

        // If this process was the init of a non-root PID namespace,
        // send SIGKILL to all remaining processes in that namespace
        // (Linux: zap_pid_ns_processes).
        // Process exit state is published before waking pidfd/wait waiters.
        unsafe {
            thr.proc_data
                .exit_event()
                .wake(IoEvents::IN | IoEvents::RDNORM);
        };

        // Unblock a vfork parent waiting for this child to exit.
        thr.proc_data.notify_vfork_done();
        shutdown_reap_parent = namespace_shutdown_parent(process);
    }

    // Publish the terminal thread state before waking pidfd and join waiters.
    thr.set_exit();
    unsafe { thr.exit_event().wake(axpoll::IoEvents::IN) };
    unsafe { thr.proc_data.thread_exit_event().wake(axpoll::IoEvents::IN) };

    if let Some(parent) = shutdown_reap_parent
        && let Some(cpu_time) = reap_process(process)
    {
        parent.add_child_cpu_time(cpu_time.user(), cpu_time.system());
    }
    release_thread_pid(&thr.proc_data.identity(), thr.tid() as u64);
}

/// Rebinds a task's user-visible TID in [`TASK_TABLE`] from `old_tid` to
/// `new_tid`.
///
/// Used by `execve`'s de_thread step: when a non-leader thread successfully
/// `execve`s, it inherits the leader's TID/TGID so that `gettid() == getpid()`
/// holds in the new image. This re-keys the global task lookup table so
/// signal/wait targeting the leader TID resolves to the renamed thread.
///
/// Caller is responsible for ensuring no other task currently occupies
/// `new_tid` (the original leader must already have been zapped and
/// removed from the table). The two updates are not atomic with respect
/// to each other; a brief window exists where both keys point at the same
/// task, which is harmless because both lookups resolve to the same task.
pub fn rebind_task_tid(task: &UserTaskRef, old_tid: Pid, new_tid: Pid) {
    let mut table = TASK_TABLE.lock();
    table.insert(new_tid, task.downgrade());
    table.remove(&old_tid);
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
pub fn zap_thread(tid: Pid) -> AxResult<()> {
    let task = get_task(tid)?;
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

#[cfg(axtest)]
pub(crate) fn decode_wait_status_rules_hold_for_test() -> bool {
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

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;

    use super::remove_matching_entry;

    #[test]
    fn rollback_removes_only_the_identity_it_registered() {
        let mut entries = BTreeMap::from([(7, 11_u64)]);

        assert!(!remove_matching_entry(&mut entries, &7, |identity| {
            *identity == 10
        }));
        assert_eq!(entries.get(&7), Some(&11));
        assert!(remove_matching_entry(&mut entries, &7, |identity| {
            *identity == 11
        }));
        assert!(!entries.contains_key(&7));
    }
}
