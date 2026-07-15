use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};
use bitflags::bitflags;
use linux_raw_sys::general::{SI_TKILL, SI_USER};
use starry_signal::{SignalInfo, Signo};
use starry_vm::VmPtr;

use crate::{
    file::{FD_TABLE, FileLike, PidFd, add_file_like, current_fd_table},
    syscall::signal::check_kill_permission,
    task::{
        current_user_task, get_process_data, get_task, send_signal_to_process,
        send_signal_to_process_group, send_signal_to_thread,
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

fn parse_signo(signo: u32) -> AxResult<Signo> {
    Signo::from_repr(signo as u8).ok_or(AxError::InvalidInput)
}

fn make_pidfd_siginfo(signo: Signo, scope: PidFdSignalScope) -> SignalInfo {
    let code = if scope == PidFdSignalScope::Thread {
        SI_TKILL
    } else {
        SI_USER as _
    };
    let curr = current_user_task();
    let thread = curr.as_thread();
    SignalInfo::new_user(signo, code, thread.proc_data.proc.pid(), thread.cred().uid)
}

pub fn sys_pidfd_open(pid: u32, flags: u32) -> AxResult<isize> {
    debug!("sys_pidfd_open <= pid: {pid}, flags: {flags}");

    let flags = PidFdFlags::from_bits(flags).ok_or(AxError::InvalidInput)?;

    // Linux pidfd_open(2): EINVAL if pid is not valid (includes pid <= 0).
    if (pid as i32) <= 0 {
        return Err(AxError::InvalidInput);
    }

    let fd = if flags.contains(PidFdFlags::THREAD) {
        PidFd::new_thread(get_task(pid)?.as_thread(), pid)
    } else {
        // Without PIDFD_THREAD the target must be a thread-group leader.
        if let Ok(task) = get_task(pid)
            && task.as_thread().proc_data.proc.pid() != pid
        {
            return Err(AxError::NotFound);
        }
        PidFd::new_process(&get_process_data(pid)?)
    };
    if flags.contains(PidFdFlags::NONBLOCK) {
        fd.set_nonblocking(true)?;
    }

    fd.add_to_fd_table(true).map(|fd| fd as _)
}

pub fn sys_pidfd_getfd(pidfd: i32, target_fd: i32, flags: u32) -> AxResult<isize> {
    debug!("sys_pidfd_getfd <= pidfd: {pidfd}, target_fd: {target_fd}, flags: {flags}");

    if flags != 0 {
        return Err(AxError::InvalidInput);
    }

    let pidfd = PidFd::from_fd(pidfd)?;
    let proc_data = pidfd.process_data()?;
    let curr_proc_data = current_user_task().as_thread().proc_data.clone();
    let is_current = Arc::ptr_eq(&proc_data, &curr_proc_data);
    if !is_current {
        // Linux __pidfd_fget() uses ptrace_may_access(PTRACE_MODE_ATTACH_REALCREDS).
        // Until Starry has that, require at least kill-style credentials on the target.
        check_kill_permission(proc_data.proc.pid())?;
    }
    let fd_entry = if is_current {
        // Use the live fd table for the current process. `proc_data.scope` is only
        // refreshed on clone/dup paths; syscalls like pipe() update ActiveScope only.
        current_fd_table().read().get(target_fd as usize).cloned()
    } else {
        FD_TABLE
            .scope(&proc_data.scope.read())
            .read()
            .get(target_fd as usize)
            .cloned()
    };
    fd_entry.ok_or(AxError::BadFileDescriptor).and_then(|fd| {
        let fd = add_file_like(fd.inner.clone(), true)?;
        Ok(fd as isize)
    })
}

pub fn sys_pidfd_send_signal(
    pidfd: i32,
    signo: u32,
    sig: *mut SignalInfo,
    flags: u32,
) -> AxResult<isize> {
    let flags = PidFdSignalFlags::from_bits(flags).ok_or(AxError::InvalidInput)?;
    if flags.bits().count_ones() > 1 {
        return Err(AxError::InvalidInput);
    }

    let pidfd_obj = PidFd::from_fd(pidfd)?;
    let proc_data = pidfd_obj.process_data()?;
    let target_pid = proc_data.proc.pid();

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
        Some(make_pidfd_siginfo(signo, scope))
    } else {
        let signo_parsed = parse_signo(signo)?;
        let info = unsafe { sig.vm_read_uninit()?.assume_init() };
        if info.signo() != signo_parsed {
            return Err(AxError::InvalidInput);
        }
        if current_user_task().as_thread().proc_data.proc.pid() != target_pid
            && (info.code() >= 0 || info.code() == SI_TKILL)
        {
            return Err(AxError::OperationNotPermitted);
        }
        Some(info)
    };

    match scope {
        PidFdSignalScope::Thread => {
            let tid = pidfd_obj.tid().ok_or(AxError::InvalidInput)?;
            check_kill_permission(tid)?;
            send_signal_to_thread(Some(target_pid), tid, kinfo)?;
        }
        PidFdSignalScope::ThreadGroup => {
            check_kill_permission(target_pid)?;
            send_signal_to_process(target_pid, kinfo)?;
        }
        PidFdSignalScope::ProcessGroup => {
            let pgid = proc_data.proc.group().pgid();
            check_kill_permission(pgid)?;
            send_signal_to_process_group(pgid, kinfo)?;
        }
    }

    Ok(0)
}
