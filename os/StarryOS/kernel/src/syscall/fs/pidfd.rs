use alloc::sync::Arc;

use ax_task::current;
use bitflags::bitflags;
use linux_raw_sys::general::{SI_TKILL, SI_USER};
use starry_signal::{SignalInfo, Signo};
use starry_vm::VmPtr;

use crate::{
    Errno, StarryError, StarryResult,
    file::{FD_TABLE, FileLike, PidFd, add_file_like},
    syscall::signal::check_kill_permission_identity,
    task::{
        AsThread, Tgid, TgidNumber, TidNumber, current_pid_view, get_user_task_by_number,
        send_signal_to_process_data, send_signal_to_process_group_ref, send_signal_to_task,
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

enum PidFdOpenTarget {
    Process(TgidNumber),
    Thread(TidNumber),
}

impl PidFdOpenTarget {
    fn parse(pid: u32, flags: PidFdFlags) -> StarryResult<Self> {
        if (pid as i32) <= 0 {
            return Err(StarryError::InvalidInput);
        }
        if flags.contains(PidFdFlags::THREAD) {
            Ok(Self::Thread(TidNumber::try_from(pid)?))
        } else {
            Ok(Self::Process(TgidNumber::try_from(pid)?))
        }
    }
}

fn parse_signo(signo: u32) -> StarryResult<Signo> {
    Signo::from_repr(signo as u8).ok_or(StarryError::InvalidInput)
}

fn make_pidfd_siginfo(signo: Signo, scope: PidFdSignalScope) -> SignalInfo {
    let code = if scope == PidFdSignalScope::Thread {
        SI_TKILL
    } else {
        SI_USER as _
    };
    let curr = current();
    let thread = curr.as_thread();
    let sender = current_pid_view()
        .visible_number(&thread.proc_data.identity())
        .expect("current process is visible in its active PID namespace")
        .get();
    SignalInfo::new_user(signo, code, sender, thread.cred().uid)
}

pub fn sys_pidfd_open(pid: u32, flags: u32) -> StarryResult<isize> {
    debug!("sys_pidfd_open <= pid: {pid}, flags: {flags}");

    let flags = PidFdFlags::from_bits(flags).ok_or(StarryError::InvalidInput)?;

    let fd = match PidFdOpenTarget::parse(pid, flags)? {
        PidFdOpenTarget::Thread(tid) => match get_user_task_by_number(tid) {
            Ok(task) => {
                let identity = task.as_thread().pid_identity();
                PidFd::new_thread(identity, task.as_thread(), tid)
            }
            Err(StarryError::NoSuchProcess) => {
                let identity =
                    current_pid_view().resolve_process(TgidNumber::from(tid.pid_number()))?;
                if !identity.is_zombie() {
                    return Err(StarryError::NoSuchProcess);
                }
                PidFd::new_exited_thread(identity)
            }
            Err(error) => return Err(error),
        },
        PidFdOpenTarget::Process(tgid) => {
            // Without PIDFD_THREAD the target must be a thread-group leader.
            let view = current_pid_view();
            let identity = view.resolve_identity(tgid.pid_number())?;
            if !identity.has_role::<Tgid>() {
                return Err(Errno::ENOENT.into());
            }
            identity.public_process()?;
            PidFd::new_process(identity)
        }
    };
    if flags.contains(PidFdFlags::NONBLOCK) {
        fd.set_nonblocking(true)?;
    }

    fd.add_to_fd_table(true).map(|fd| fd as _)
}

pub fn sys_pidfd_getfd(pidfd: i32, target_fd: i32, flags: u32) -> StarryResult<isize> {
    debug!("sys_pidfd_getfd <= pidfd: {pidfd}, target_fd: {target_fd}, flags: {flags}");

    if flags != 0 {
        return Err(StarryError::InvalidInput);
    }

    let pidfd = PidFd::from_fd(pidfd)?;
    let proc_data = pidfd.process_data()?;
    let curr_proc_data = current().as_thread().proc_data.clone();
    let is_current = Arc::ptr_eq(&proc_data, &curr_proc_data);
    if !is_current {
        // Linux __pidfd_fget() uses ptrace_may_access(PTRACE_MODE_ATTACH_REALCREDS).
        // Until Starry has that, require at least kill-style credentials on the target.
        check_kill_permission_identity(&proc_data.identity())?;
    }
    let fd_entry = if is_current {
        // Use the calling thread's live fd table, including any table installed
        // by unshare(CLONE_FILES) or close_range(CLOSE_RANGE_UNSHARE).
        crate::file::current_fd_table()
            .read()
            .get(target_fd as usize)
            .cloned()
    } else {
        let task = pidfd
            .process_identity()
            .live_task()
            .ok_or(StarryError::NoSuchProcess)?;
        FD_TABLE
            .scope(&task.as_thread().scope.read())
            .read()
            .get(target_fd as usize)
            .cloned()
    };
    fd_entry
        .ok_or(StarryError::BadFileDescriptor)
        .and_then(|fd| {
            let fd = add_file_like(fd.inner.clone(), true)?;
            Ok(fd as isize)
        })
}

pub fn sys_pidfd_send_signal(
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
    let target_process = pidfd_obj.process_identity();

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
            return Err(StarryError::InvalidInput);
        }
        if !Arc::ptr_eq(&current().as_thread().proc_data.identity(), &target_process)
            && (info.code() >= 0 || info.code() == SI_TKILL)
        {
            return Err(StarryError::OperationNotPermitted);
        }
        Some(info)
    };

    match scope {
        PidFdSignalScope::Thread => {
            check_kill_permission_identity(&target_process)?;
            if pidfd_obj.is_zombie() {
                return Ok(0);
            }
            let task = pidfd_obj.signal_thread()?;
            send_signal_to_task(&task, Some(target_process), kinfo)?;
        }
        PidFdSignalScope::ThreadGroup => {
            check_kill_permission_identity(&target_process)?;
            if let Some(proc_data) = target_process.live_data() {
                send_signal_to_process_data(&proc_data, kinfo)?;
            } else if !target_process.is_zombie() {
                return Err(StarryError::NoSuchProcess);
            }
        }
        PidFdSignalScope::ProcessGroup => {
            let process = pidfd_obj.signal_process()?;
            check_kill_permission_identity(&target_process)?;
            send_signal_to_process_group_ref(&process.group(), kinfo)?;
        }
    }

    Ok(0)
}

#[cfg(all(test, not(axtest)))]
fn pidfd_flags_and_signal_validation_rules_hold_for_test() -> bool {
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

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn pidfd_flags_and_signal_validation_rules_hold() {
        assert!(super::pidfd_flags_and_signal_validation_rules_hold_for_test());
    }
}
