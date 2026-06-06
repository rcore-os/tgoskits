use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::ffi::c_long;

use ax_errno::{AxError, AxResult};
use ax_runtime::hal::time::TimeValue;
use ax_task::{AxTaskRef, TaskInner, WeakAxTaskRef, current};
use bytemuck::AnyBitPattern;
use linux_raw_sys::general::ROBUST_LIST_LIMIT;
use spin::RwLock;
use starry_process::{Pid, Process, ProcessGroup, Session};
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};
use weak_map::WeakMap;

use super::{
    AsThread, Cred, FutexKey, ProcessData, Thread, TimerState, futex_table_for_process,
    send_signal_thread_inner, send_signal_to_process, send_signal_to_thread,
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

static TASK_TABLE: RwLock<WeakMap<Pid, WeakAxTaskRef>> = RwLock::new(WeakMap::new());

static PROCESS_TABLE: RwLock<WeakMap<Pid, Weak<ProcessData>>> = RwLock::new(WeakMap::new());

/// Per-zombie data retained until `waitpid()` reaps the process.
///
/// - `proc`: keeps `getsid`, `getpgid`, `getpriority`, etc. working.
/// - `cred`: snapshot of the exiting thread's final credentials, used by
///   `check_kill_permission` when the task has already been GC'd.  On Linux
///   the `task_struct` (and its `cred`) lives until the zombie is reaped;
///   we replicate that guarantee here.
struct ZombieEntry {
    proc: Arc<Process>,
    cred: Arc<Cred>,
    ptrace_tracer_pid: Option<Pid>,
}

/// Zombie processes: exited but not yet reaped by waitpid().
///
/// Maps PID → [`ZombieEntry`].  Inserted by `register_zombie` (called from
/// `do_exit` before `process.exit()`), removed by `unregister_zombie` (called
/// from `waitpid` after `child.free()`).
static ZOMBIE_TABLE: RwLock<BTreeMap<Pid, ZombieEntry>> = RwLock::new(BTreeMap::new());

static PROCESS_GROUP_TABLE: RwLock<WeakMap<Pid, Weak<ProcessGroup>>> = RwLock::new(WeakMap::new());

static SESSION_TABLE: RwLock<WeakMap<Pid, Weak<Session>>> = RwLock::new(WeakMap::new());

/// Cleanup expired entries in the task tables.
///
/// This function is intended to be used during memory leak analysis to remove
/// possible noise caused by expired entries in the [`WeakMap`].
#[cfg(feature = "memtrack")]
pub fn cleanup_task_tables() {
    TASK_TABLE.write().cleanup();
    PROCESS_TABLE.write().cleanup();
    PROCESS_GROUP_TABLE.write().cleanup();
    SESSION_TABLE.write().cleanup();
}

/// Add the task, the thread and possibly its process, process group and session
/// to the corresponding tables.
pub fn add_task_to_table(task: &AxTaskRef) {
    let tid = task.id().as_u64() as Pid;

    let mut task_table = TASK_TABLE.write();
    task_table.insert(tid, task);

    let proc_data = &task.as_thread().proc_data;
    let proc = &proc_data.proc;
    let pid = proc.pid();
    let mut proc_table = PROCESS_TABLE.write();
    if proc_table.contains_key(&pid) {
        return;
    }
    proc_table.insert(pid, proc_data);

    let pg = proc.group();
    let mut pg_table = PROCESS_GROUP_TABLE.write();
    if pg_table.contains_key(&pg.pgid()) {
        return;
    }
    pg_table.insert(pg.pgid(), &pg);

    let session = pg.session();
    let mut session_table = SESSION_TABLE.write();
    if session_table.contains_key(&session.sid()) {
        return;
    }
    session_table.insert(session.sid(), &session);
}

/// Lists all tasks.
pub fn tasks() -> Vec<AxTaskRef> {
    TASK_TABLE.read().values().collect()
}

/// Finds the task with the given TID.
pub fn get_task(tid: Pid) -> AxResult<AxTaskRef> {
    if tid == 0 {
        return Ok(current().clone());
    }
    TASK_TABLE.read().get(&tid).ok_or(AxError::NoSuchProcess)
}

/// Lists all processes.
pub fn processes() -> Vec<Arc<ProcessData>> {
    PROCESS_TABLE.read().values().collect()
}

/// Finds the process with the given PID.
pub fn get_process_data(pid: Pid) -> AxResult<Arc<ProcessData>> {
    if pid == 0 {
        return Ok(current().as_thread().proc_data.clone());
    }
    PROCESS_TABLE.read().get(&pid).ok_or(AxError::NoSuchProcess)
}

/// Explicitly removes a process from the process table.
///
/// Called after [`Process::free`] to ensure `get_process_data(pid)` returns
/// `NoSuchProcess` immediately, regardless of whether any other strong
/// [`Arc<ProcessData>`] references (e.g. task objects) are still alive.
pub fn remove_process(pid: Pid) {
    PROCESS_TABLE.write().remove(&pid);
}

/// Records a PID as zombie (exited but not yet reaped).
///
/// Called from `do_exit` before `process.exit()`.  Stores both the
/// `Arc<Process>` (for `getsid`/`getpgid`/etc.) and a snapshot of the
/// exiting thread's final credentials (for `check_kill_permission` after
/// the task has been GC'd).
pub fn register_zombie(
    pid: Pid,
    proc: Arc<Process>,
    cred: Arc<Cred>,
    ptrace_tracer_pid: Option<Pid>,
) {
    ZOMBIE_TABLE.write().insert(
        pid,
        ZombieEntry {
            proc,
            cred,
            ptrace_tracer_pid,
        },
    );
}

/// Removes a PID from the zombie table.
///
/// Called from `waitpid` after `child.free()`.  Drops the stored entry.
pub fn unregister_zombie(pid: Pid) {
    ZOMBIE_TABLE.write().remove(&pid);
}

/// Returns `true` if `pid` is a zombie (exited but not yet reaped).
pub fn is_zombie_pid(pid: Pid) -> bool {
    ZOMBIE_TABLE.read().contains_key(&pid)
}

/// Returns the `Arc<Process>` for a zombie PID, or `None` if not a zombie.
///
/// Used by syscalls that must return valid data for zombie processes
/// (e.g. `getsid`, `getpgid`).
pub fn get_zombie_process(pid: Pid) -> Option<Arc<Process>> {
    ZOMBIE_TABLE.read().get(&pid).map(|e| e.proc.clone())
}

/// Returns the credential snapshot for a zombie PID, or `None` if not a zombie.
///
/// Used by `check_kill_permission` to authorise signals to zombies whose
/// task has already been GC'd.  Mirrors Linux behaviour where `task_struct`
/// (and its `cred`) lives until the zombie is reaped by `waitpid`.
pub fn get_zombie_cred(pid: Pid) -> Option<Arc<Cred>> {
    ZOMBIE_TABLE.read().get(&pid).map(|e| e.cred.clone())
}

pub fn traced_zombies_for(tracer_pid: Pid) -> Vec<Arc<Process>> {
    ZOMBIE_TABLE
        .read()
        .values()
        .filter(|entry| entry.ptrace_tracer_pid == Some(tracer_pid))
        .map(|entry| entry.proc.clone())
        .collect()
}

/// Finds the process with the given PID.
///
/// A zombie process may no longer have live [`ProcessData`] after its last
/// thread exits, but POSIX process-id queries such as `getpgid(pid)` and
/// `kill(pid, 0)` must still see it until the parent reaps it.
pub fn get_process(pid: Pid) -> AxResult<Arc<Process>> {
    if pid == 0 {
        return Ok(current().as_thread().proc_data.proc.clone());
    }
    if let Ok(proc_data) = get_process_data(pid) {
        return Ok(proc_data.proc.clone());
    }
    get_zombie_process(pid).ok_or(AxError::NoSuchProcess)
}

/// Finds the credentials for a process that may already be a zombie.
pub fn get_process_cred(pid: Pid) -> AxResult<Arc<Cred>> {
    if pid == 0 {
        return Ok(current().as_thread().cred());
    }
    if let Ok(task) = get_task(pid)
        && let Some(thr) = task.try_as_thread()
    {
        return Ok(thr.cred());
    }
    get_zombie_cred(pid).ok_or(AxError::NoSuchProcess)
}

/// Finds the process group with the given PGID.
pub fn get_process_group(pgid: Pid) -> AxResult<Arc<ProcessGroup>> {
    if let Some(pg) = PROCESS_GROUP_TABLE.read().get(&pgid) {
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
    let mut pg_table = PROCESS_GROUP_TABLE.write();
    pg_table.insert(pg.pgid(), pg);
}

fn find_process_group_by_member(pgid: Pid) -> Option<Arc<ProcessGroup>> {
    for proc_data in PROCESS_TABLE.read().values() {
        let pg = proc_data.proc.group();
        if pg.pgid() == pgid {
            return Some(pg);
        }
    }

    for zombie in ZOMBIE_TABLE.read().values() {
        let pg = zombie.proc.group();
        if pg.pgid() == pgid {
            return Some(pg);
        }
    }

    None
}

/// Registers a session in the global table.
pub fn register_session(session: &Arc<Session>) {
    let mut session_table = SESSION_TABLE.write();
    session_table.insert(session.sid(), session);
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
pub fn poll_process_timer(pid: Pid) {
    if let Ok(proc_data) = get_process_data(pid) {
        proc_data.posix_timers.poll_expired(pid, |sig| {
            let _ = send_signal_to_process(pid, Some(sig));
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

pub fn do_exit(exit_code: i32, group_exit: bool) {
    let curr = current();
    let thr = curr.as_thread();

    info!("{} exit with code: {}", curr.id_name(), exit_code);

    trace_sched_process_exit(curr.id().as_u64(), exit_code);

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
        ax_task::yield_now();
    }

    let process = &thr.proc_data.proc;

    // Update cgroup: remove process and decrement pids counter
    {
        let pid = process.pid();
        let cgroup = thr.proc_data.cgroup.read().clone();
        let mut procs = cgroup.procs.lock();
        if let Some(pos) = procs.iter().position(|&p| p == pid) {
            procs.swap_remove(pos);
            cgroup.pids.exit();
        }
    }

    // Use the user-visible TID (`thr.tid()`), not the scheduler ID. After
    // a non-leader `execve`'s de_thread the two differ, and the thread
    // group is keyed by the user-visible TID.
    if process.exit_thread(thr.tid(), exit_code) {
        // AIO contexts pin the process address space and may have worker tasks
        // waiting on outstanding requests. Tear them down before releasing the
        // process address-space slot.
        crate::syscall::cleanup_aio_contexts_for_pid(process.pid());

        // Close all file descriptors before marking the process as exited.
        // This ensures pipe write ends and other resources are properly released,
        // so parent processes blocking on pipe reads will receive EOF.
        crate::file::close_all_fds();

        // Release all POSIX (fcntl) locks held by this pid. Linux releases
        // them implicitly via fl_release_private when the last fd referring
        // to the inode is closed; we track POSIX locks by pid rather than
        // by fd, so the cleanup happens here at process-exit time. Without
        // this, a child fork → F_SETLK → exit would permanently pin the
        // record in FCNTL_LOCKS and block all later acquirers.
        crate::syscall::release_pid_locks(process.pid());

        // Snapshot children BEFORE process.exit() reparents them to init
        // via mem::take. Otherwise process.children() returns an empty
        // list and pdeathsig never reaches the real children.
        let children_snapshot = process.children();

        // Register the zombie BEFORE process.exit() publishes is_zombie=true.
        // This closes a race where the parent's waitpid(WNOHANG) could observe
        // is_zombie=true, complete the reap (free + unregister_zombie), and
        // then this thread would late-insert a stale zombie entry that is
        // never cleaned up.  By inserting first, any reap that sees
        // is_zombie=true is guaranteed to find (and remove) the entry.
        //
        // Snapshot the exiting thread's final credentials so that
        // check_kill_permission can still authorise signals to this zombie
        // after the task has been GC'd (mirrors Linux task_struct lifetime).
        let zombie_cred = thr.cred();
        let ptrace_tracer_pid = thr.proc_data.ptrace_tracer_pid();
        register_zombie(
            process.pid(),
            process.clone(),
            zombie_cred,
            ptrace_tracer_pid,
        );
        process.exit();
        if let Some(parent) = process.parent() {
            if let Some(signo) = thr.proc_data.exit_signal {
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
                data.child_exit_event.wake();
            }
        }
        if let Some(tracer_pid) = ptrace_tracer_pid
            && process
                .parent()
                .is_none_or(|parent| parent.pid() != tracer_pid)
            && let Ok(data) = get_process_data(tracer_pid)
        {
            data.child_exit_event.wake();
        }
        // Send pdeathsig to child processes
        for child in children_snapshot {
            let child_pid = child.pid();
            if let Ok(child_task) = get_task(child_pid)
                && let Some(child_thr) = child_task.try_as_thread()
            {
                let sig = child_thr.pdeathsig();
                if sig > 0
                    && let Some(signo) = Signo::from_repr(sig as u8)
                {
                    let _ = send_signal_to_process(child_pid, Some(SignalInfo::new_kernel(signo)));
                }
            }
        }

        thr.proc_data.exit_event.wake();

        // Unblock a vfork parent waiting for this child to exit.
        thr.proc_data.notify_vfork_done();

        crate::syscall::clear_proc_shm(process.pid(), &thr.proc_data.aspace());

        // Drop memfd inode accounting before waitpid returns (SMP); use
        // process_slots refcounting — not vm_aspace_shared + clear().
        thr.proc_data.release_aspace_slot_if_needed();
    }
    thr.exit_event.wake();
    thr.proc_data.thread_exit_event.wake();

    if group_exit && !process.is_group_exited() {
        process.group_exit();
        let sig = SignalInfo::new_kernel(Signo::SIGKILL);
        for tid in process.threads() {
            let _ = send_signal_to_thread(None, tid, Some(sig.clone()));
        }
    }
    thr.set_exit();
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
pub fn rebind_task_tid(task: &AxTaskRef, old_tid: Pid, new_tid: Pid) {
    let mut table = TASK_TABLE.write();
    table.insert(new_tid, task);
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
    let thr = task.try_as_thread().ok_or(AxError::OperationNotPermitted)?;
    thr.set_exit_request();
    task.interrupt();
    Ok(())
}
