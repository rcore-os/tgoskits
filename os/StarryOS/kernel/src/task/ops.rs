use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{ffi::c_long, sync::atomic::Ordering};

use ax_errno::{AxError, AxResult};
use ax_hal::time::TimeValue;
use ax_task::{AxTaskRef, TaskInner, WeakAxTaskRef, current};
use bytemuck::AnyBitPattern;
use linux_raw_sys::general::ROBUST_LIST_LIMIT;
use spin::RwLock;
use starry_process::{Pid, Process, ProcessGroup, Session};
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};
use weak_map::WeakMap;

use super::{
    AsThread, Cred, FutexKey, ProcessData, TimerState, futex_table_for, send_signal_thread_inner,
    send_signal_to_process, send_signal_to_thread,
};

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
}

/// Zombie processes: exited but not yet reaped by waitpid().
///
/// Maps PID → [`ZombieEntry`].  Inserted by `register_zombie` (called from
/// `do_exit` before `process.exit()`), removed by `unregister_zombie` (called
/// from `waitpid` after `child.free()`).
static ZOMBIE_TABLE: RwLock<alloc::collections::BTreeMap<Pid, ZombieEntry>> =
    RwLock::new(alloc::collections::BTreeMap::new());

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
pub fn register_zombie(pid: Pid, proc: Arc<Process>, cred: Arc<Cred>) {
    ZOMBIE_TABLE.write().insert(pid, ZombieEntry { proc, cred });
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

/// Finds the process group with the given PGID.
pub fn get_process_group(pgid: Pid) -> AxResult<Arc<ProcessGroup>> {
    PROCESS_GROUP_TABLE
        .read()
        .get(&pgid)
        .ok_or(AxError::NoSuchProcess)
}

/// Registers a process group in the global table.
pub fn register_process_group(pg: &Arc<ProcessGroup>) {
    let mut pg_table = PROCESS_GROUP_TABLE.write();
    pg_table.insert(pg.pgid(), pg);
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

fn handle_futex_death(entry: *mut RobustList, offset: i64) -> AxResult<()> {
    let address = (entry as u64)
        .checked_add_signed(offset)
        .ok_or(AxError::InvalidInput)?;
    let address: usize = address.try_into().map_err(|_| AxError::InvalidInput)?;
    let key = FutexKey::new_current_teardown(address);

    let futex_table = futex_table_for(&key);

    let Some(futex) = futex_table.get(&key) else {
        return Ok(());
    };
    futex.owner_dead.store(true, Ordering::SeqCst);
    futex.wq.wake(1, u32::MAX);
    Ok(())
}

pub fn exit_robust_list(head: *const RobustListHead) -> AxResult<()> {
    // Reference: https://elixir.bootlin.com/linux/v6.13.6/source/kernel/futex/core.c#L777

    let mut limit = ROBUST_LIST_LIMIT;

    let end_ptr = unsafe { &raw const (*head).list };
    let head = head.vm_read()?;
    let mut entry = head.list.next;
    let offset = head.futex_offset;
    let pending = head.list_op_pending;

    while !core::ptr::eq(entry, end_ptr) {
        let next_entry = entry.vm_read()?.next;
        if entry != pending {
            handle_futex_death(entry, offset)?;
        }
        entry = next_entry;

        limit -= 1;
        if limit == 0 {
            return Err(AxError::FilesystemLoop);
        }
        ax_task::yield_now();
    }

    // Process the pending entry that was skipped in the loop
    if !pending.is_null() {
        handle_futex_death(pending, offset)?;
    }

    Ok(())
}

pub fn do_exit(exit_code: i32, group_exit: bool) {
    let curr = current();
    let thr = curr.as_thread();

    info!("{} exit with code: {}", curr.id_name(), exit_code);

    let clear_child_tid = thr.clear_child_tid() as *mut u32;
    if clear_child_tid.vm_write(0).is_ok() {
        let key = FutexKey::new_current_teardown(clear_child_tid as usize);
        let table = futex_table_for(&key);
        let guard = table.get(&key);
        if let Some(futex) = guard {
            futex.wq.wake(1, u32::MAX);
        }
        ax_task::yield_now();
    }
    let head = thr.robust_list_head() as *const RobustListHead;
    if !head.is_null()
        && let Err(err) = exit_robust_list(head)
    {
        warn!("exit robust list failed: {err:?}");
    }

    let process = &thr.proc_data.proc;
    if process.exit_thread(curr.id().as_u64() as Pid, exit_code) {
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
        register_zombie(process.pid(), process.clone(), zombie_cred);
        process.exit();
        if let Some(parent) = process.parent() {
            if let Some(signo) = thr.proc_data.exit_signal {
                let _ = send_signal_to_process(parent.pid(), Some(SignalInfo::new_kernel(signo)));
            }
            if let Ok(data) = get_process_data(parent.pid()) {
                data.child_exit_event.wake();
            }
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
    }
    thr.exit_event.wake();

    if group_exit && !process.is_group_exited() {
        process.group_exit();
        let sig = SignalInfo::new_kernel(Signo::SIGKILL);
        for tid in process.threads() {
            let _ = send_signal_to_thread(None, tid, Some(sig.clone()));
        }
    }
    thr.set_exit();
}
