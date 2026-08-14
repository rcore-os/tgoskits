use alloc::sync::Arc;

use bitflags::bitflags;
use linux_raw_sys::general::{SI_TKILL, SI_USER};
use starry_signal::{SignalInfo, Signo};

use crate::{
    StarryError, StarryResult,
    file::{FD_TABLE, FileLike, PidFd, add_file_like, current_fd_table},
    mm::VmPtr,
    syscall::signal::check_kill_permission,
    task::{
        get_task, pidfd_process_identity, pidfd_thread_identity, resolve_user_pid,
        send_signal_to_process, send_signal_to_process_group, send_signal_to_thread,
        visible_user_pid,
    },
};

bitflags! {
    #[derive(Debug, Clone, Copy, Default)]
    pub struct PidFdFlags: u32 {
        const NONBLOCK = 2048;
        const THREAD = 128;
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, Default)]
    struct PidFdSignalFlags: u32 {
        const THREAD = 1 << 0;
        const THREAD_GROUP = 1 << 1;
        const PROCESS_GROUP = 1 << 2;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PidFdSignalScope {
    Thread,
    ThreadGroup,
    ProcessGroup,
}

fn parse_signo(signo: u32) -> StarryResult<Signo> {
    Signo::from_repr(signo as u8).ok_or(StarryError::InvalidInput)
}

fn make_pidfd_siginfo(
    current: &crate::task::UserTaskRef,
    signo: Signo,
    scope: PidFdSignalScope,
) -> SignalInfo {
    let code = if scope == PidFdSignalScope::Thread {
        SI_TKILL
    } else {
        SI_USER as _
    };
    let curr = current;
    let thread = curr.as_thread();
    SignalInfo::new_user(
        signo,
        code,
        visible_user_pid(current, thread.proc_data.proc.pid() as u64),
        thread.cred().uid,
    )
}

pub fn sys_pidfd_open(
    current: &crate::task::UserTaskRef,
    pid: u32,
    flags: u32,
) -> crate::StarryResult<isize> {
    debug!("sys_pidfd_open <= pid: {pid}, flags: {flags}");

    let flags = PidFdFlags::from_bits(flags).ok_or(StarryError::InvalidInput)?;

    // Linux pidfd_open(2): EINVAL if pid is not valid (includes pid <= 0).
    if (pid as i32) <= 0 {
        return Err(StarryError::InvalidInput);
    }
    let pid = resolve_user_pid(current, pid)?;

    let fd = if flags.contains(PidFdFlags::THREAD) {
        match get_task(pid) {
            Ok(task) => {
                let identity = pidfd_thread_identity(&task.as_thread().proc_data.proc)
                    .ok_or(StarryError::NoSuchProcess)?;
                PidFd::new_thread(identity, task.as_thread(), pid)
            }
            Err(StarryError::NoSuchProcess) => {
                let identity = pidfd_process_identity(pid)?;
                if !identity.is_zombie() {
                    return Err(StarryError::NoSuchProcess);
                }
                PidFd::new_exited_thread(identity)
            }
            Err(error) => return Err(error),
        }
    } else {
        // Without PIDFD_THREAD the target must be a thread-group leader.
        if let Ok(task) = get_task(pid)
            && task.as_thread().proc_data.proc.pid() != pid
        {
            return Err(StarryError::NotFound);
        }
        PidFd::new_process(pidfd_process_identity(pid)?)
    };
    if flags.contains(PidFdFlags::NONBLOCK) {
        fd.set_nonblocking(true)?;
    }

    fd.add_to_fd_table(true).map(|fd| fd as _)
}

pub fn sys_pidfd_getfd(
    current: &crate::task::UserTaskRef,
    pidfd: i32,
    target_fd: i32,
    flags: u32,
) -> crate::StarryResult<isize> {
    debug!("sys_pidfd_getfd <= pidfd: {pidfd}, target_fd: {target_fd}, flags: {flags}");

    if flags != 0 {
        return Err(StarryError::InvalidInput);
    }

    let pidfd = PidFd::from_fd(pidfd)?;
    let proc_data = pidfd.process_data()?;
    let curr_proc_data = current.as_thread().proc_data.clone();
    let is_current = Arc::ptr_eq(&proc_data, &curr_proc_data);
    if !is_current {
        // Linux __pidfd_fget() uses ptrace_may_access(PTRACE_MODE_ATTACH_REALCREDS).
        // Until Starry has that, require at least kill-style credentials on the target.
        check_kill_permission(current, proc_data.proc.pid())?;
    }
    let fd_entry = if is_current {
        // Use the calling thread's live fd table, including any table installed
        // by unshare(CLONE_FILES) or close_range(CLOSE_RANGE_UNSHARE).
        current_fd_table().read().get(target_fd as usize).cloned()
    } else {
        let task = get_task(proc_data.proc.pid())?;
        let fd_table = task.as_thread().clone_scope_item(&FD_TABLE);
        fd_table.read().get(target_fd as usize).cloned()
    };
    fd_entry
        .ok_or(StarryError::BadFileDescriptor)
        .and_then(|fd| {
            let fd = add_file_like(fd.inner.clone(), true)?;
            Ok(fd as isize)
        })
}

pub fn sys_pidfd_send_signal(
    current: &crate::task::UserTaskRef,
    pidfd: i32,
    signo: u32,
    sig: *mut SignalInfo,
    flags: u32,
) -> StarryResult<isize> {
    let flags = PidFdSignalFlags::from_bits(flags).ok_or(StarryError::InvalidInput)?;
    if flags.bits().count_ones() > 1 {
        return Err(StarryError::InvalidInput);
    }

    let pidfd_obj = PidFd::from_fd(pidfd)?;
    let target_pid = pidfd_obj.process_pid();

    let scope = if flags.contains(PidFdSignalFlags::THREAD)
        || (flags.is_empty() && pidfd_obj.is_thread())
    {
        PidFdSignalScope::Thread
    } else if flags.contains(PidFdSignalFlags::PROCESS_GROUP) {
        PidFdSignalScope::ProcessGroup
    } else {
        PidFdSignalScope::ThreadGroup
    };

    let kinfo = if signo == 0 {
        None
    } else if sig.is_null() {
        let signo = parse_signo(signo)?;
        Some(make_pidfd_siginfo(current, signo, scope))
    } else {
        let signo_parsed = parse_signo(signo)?;
        let info = unsafe { sig.vm_read_uninit(current)?.assume_init() };
        if info.signo() != signo_parsed {
            return Err(StarryError::InvalidInput);
        }
        if current.as_thread().proc_data.proc.pid() != target_pid
            && (info.code() >= 0 || info.code() == SI_TKILL)
        {
            return Err(StarryError::OperationNotPermitted);
        }
        Some(info)
    };

    match scope {
        PidFdSignalScope::Thread => {
            let (process, tid) = pidfd_obj.signal_thread()?;
            check_kill_permission(current, process.pid())?;
            if pidfd_obj.is_zombie() {
                return Ok(0);
            }
            send_signal_to_thread(Some(target_pid), tid, kinfo)?;
        }
        PidFdSignalScope::ThreadGroup => {
            let process = pidfd_obj.signal_process()?;
            debug_assert_eq!(process.pid(), target_pid);
            check_kill_permission(current, target_pid)?;
            send_signal_to_process(target_pid, kinfo)?;
        }
        PidFdSignalScope::ProcessGroup => {
            let process = pidfd_obj.signal_process()?;
            let pgid = process.group().pgid();
            check_kill_permission(current, pgid)?;
            send_signal_to_process_group(pgid, kinfo)?;
        }
    }

    Ok(0)
}

#[cfg(axtest)]
pub(crate) fn pidfd_flags_and_signal_validation_rules_hold_for_test() -> bool {
    // Test PidFdFlags validation
    let valid_flags = 0u32;
    assert!(PidFdFlags::from_bits(valid_flags).is_some());

    let nonblock_only = 2048u32;
    assert!(PidFdFlags::from_bits(nonblock_only).is_some());

    let thread_only = 128u32;
    assert!(PidFdFlags::from_bits(thread_only).is_some());

    let all_valid = 2048u32 | 128u32;
    assert!(PidFdFlags::from_bits(all_valid).is_some());

    // Invalid flag should return None
    let invalid_flags = 0xFFFF;
    assert!(PidFdFlags::from_bits(invalid_flags).is_none());

    // Test parse_signo
    assert!(parse_signo(1).is_ok()); // SIGHUP
    assert!(parse_signo(9).is_ok()); // SIGKILL
    assert!(parse_signo(0).is_err()); // Invalid signo
    assert!(parse_signo(255).is_err()); // Out of range

    true
}
