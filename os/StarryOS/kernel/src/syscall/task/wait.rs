use alloc::vec::Vec;
use core::{future::poll_fn, task::Poll};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_task::{
    current,
    future::{block_on, interruptible},
};
use bitflags::bitflags;
use linux_raw_sys::general::{
    __WALL, __WCLONE, __WNOTHREAD, P_ALL, P_PID, WCONTINUED, WEXITED, WNOHANG, WNOWAIT, WUNTRACED,
};
use starry_process::{Pid, Process};
use starry_signal::SignalInfo;
use starry_vm::{VmMutPtr, VmPtr};

use crate::task::{AsThread, get_task, get_zombie_cred, remove_process, unregister_zombie};

bitflags! {
    /// Options accepted by wait4 / waitpid.
    #[derive(Debug)]
    struct WaitPidOptions: u32 {
        const WNOHANG = WNOHANG;
        const WUNTRACED = WUNTRACED;
        const WCONTINUED = WCONTINUED;
        const WNOTHREAD = __WNOTHREAD;
        const WALL = __WALL;
        const WCLONE = __WCLONE;
    }
}

bitflags! {
    /// Options accepted by waitid.
    #[derive(Debug)]
    struct WaitIdOptions: u32 {
        const WNOHANG = WNOHANG;
        const WUNTRACED = WUNTRACED;
        const WEXITED = WEXITED;
        const WCONTINUED = WCONTINUED;
        const WNOWAIT = WNOWAIT;
        const WNOTHREAD = __WNOTHREAD;
        const WALL = __WALL;
        const WCLONE = __WCLONE;
    }
}

#[derive(Debug, Clone, Copy)]
enum WaitTarget {
    /// Wait for any child process
    Any,
    /// Wait for the child whose process ID is equal to the value.
    Pid(Pid),
    /// Wait for any child process whose process group ID is equal to the value.
    Pgid(Pid),
}

impl WaitTarget {
    fn matches(&self, child: &Process) -> bool {
        match self {
            WaitTarget::Any => true,
            WaitTarget::Pid(pid) => child.pid() == *pid,
            WaitTarget::Pgid(pgid) => child.group().pgid() == *pgid,
        }
    }
}

pub fn sys_waitpid(pid: i32, exit_code: *mut i32, options: u32) -> AxResult<isize> {
    let options = WaitPidOptions::from_bits(options).ok_or(AxError::InvalidInput)?;
    info!("sys_waitpid <= pid: {pid:?}, options: {options:?}");

    let curr = current();
    let proc = &curr.as_thread().proc_data.proc;

    let target = if pid == -1 {
        WaitTarget::Any
    } else if pid == 0 {
        WaitTarget::Pgid(proc.group().pgid())
    } else if pid > 0 {
        WaitTarget::Pid(pid as _)
    } else {
        WaitTarget::Pgid(-pid as _)
    };

    // FIXME: add back support for WALL & WCLONE, since ProcessData may drop before
    // Process now.
    let children = proc
        .children()
        .into_iter()
        .filter(|child| target.matches(child))
        .collect::<Vec<_>>();
    if children.is_empty() {
        return Err(AxError::from(LinuxError::ECHILD));
    }

    let proc_data = curr.as_thread().proc_data.clone();
    let check_children = || {
        if let Some(child) = children.iter().find(|child| child.is_zombie()) {
            // Accumulate child's CPU time before freeing.
            for tid in child.threads() {
                if let Ok(task) = get_task(tid) {
                    let thr = task.as_thread();
                    let (utime, stime) = thr.time.borrow().output();
                    proc_data.add_child_cpu_time(utime, stime);
                }
            }
            // Copy status to userspace before `free` / `unregister_zombie`. If
            // `vm_write` fails we must leave the zombie intact so the parent can
            // retry; freeing first would strand the process and corrupt wait
            // accounting (Linux also publishes the status byte before full reap).
            if let Some(exit_code) = exit_code.nullable() {
                exit_code.vm_write(child.exit_code())?;
            }
            child.free();
            remove_process(child.pid());
            unregister_zombie(child.pid());
            Ok(Some(child.pid() as _))
        } else if options.contains(WaitPidOptions::WNOHANG) {
            Ok(Some(0))
        } else {
            Ok(None)
        }
    };

    block_on(interruptible(poll_fn(|cx| {
        match check_children().transpose() {
            Some(res) => Poll::Ready(res),
            None => {
                proc_data.child_exit_event.register(cx.waker());
                // A child may exit between the check above and waker
                // registration. Recheck after registering so that wakeup is
                // not lost in that race window.
                match check_children().transpose() {
                    Some(res) => Poll::Ready(res),
                    None => Poll::Pending,
                }
            }
        }
    })))?
}

/// Decode the Linux wait-status encoding into (si_code, si_status) for siginfo.
///
/// - Normal exit (`_exit`/`exit_group`): `(CLD_EXITED, exit_value)`
/// - Killed by signal: `(CLD_KILLED, signum)` or `(CLD_DUMPED, signum)`
fn decode_wait_status(raw: i32) -> (i32, i32) {
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

pub fn sys_waitid(
    idtype: u32,
    id: i32,
    infop: *mut linux_raw_sys::general::siginfo,
    options: u32,
) -> AxResult<isize> {
    use linux_raw_sys::general::P_PGID;

    // Validate idtype
    let target = match idtype {
        P_ALL => WaitTarget::Any,
        P_PID => {
            if id <= 0 {
                return Err(AxError::InvalidInput);
            }
            WaitTarget::Pid(id as Pid)
        }
        P_PGID => {
            // Not yet supported; P_PIDFD also unsupported.
            return Err(AxError::InvalidInput);
        }
        _ => return Err(AxError::InvalidInput),
    };

    // Validate options — WEXITED must be present (we don't support WSTOPPED/WCONTINUED yet).
    let options = WaitIdOptions::from_bits(options).ok_or(AxError::InvalidInput)?;
    if !options.contains(WaitIdOptions::WEXITED) {
        return Err(AxError::InvalidInput);
    }
    if options.contains(WaitIdOptions::WUNTRACED) || options.contains(WaitIdOptions::WCONTINUED) {
        return Err(AxError::InvalidInput);
    }

    info!("sys_waitid <= idtype: {idtype}, id: {id}, options: {options:?}");

    let curr = current();
    let proc = &curr.as_thread().proc_data.proc;

    let children: Vec<_> = proc
        .children()
        .into_iter()
        .filter(|child| target.matches(child))
        .collect();
    if children.is_empty() {
        return Err(AxError::from(LinuxError::ECHILD));
    }

    let proc_data = curr.as_thread().proc_data.clone();
    let check_children = || {
        if let Some(child) = children.iter().find(|child| child.is_zombie()) {
            let child_pid = child.pid();
            let raw_status = child.exit_code();
            let (code, status) = decode_wait_status(raw_status);
            let child_uid = get_zombie_cred(child_pid).map(|c| c.uid).unwrap_or(0);

            let siginfo = SignalInfo::new_sigchld(child_pid, child_uid, code, status);
            infop.vm_write(siginfo.0)?;

            if !options.contains(WaitIdOptions::WNOWAIT) {
                // Accumulate child's CPU time before freeing.
                for tid in child.threads() {
                    if let Ok(task) = get_task(tid) {
                        let thr = task.as_thread();
                        let (utime, stime) = thr.time.borrow().output();
                        proc_data.add_child_cpu_time(utime, stime);
                    }
                }
                child.free();
                remove_process(child_pid);
                unregister_zombie(child_pid);
            }
            // waitid(2): on success returns 0 (not the child PID).
            Ok(Some(0))
        } else if options.contains(WaitIdOptions::WNOHANG) {
            // WNOHANG with no ready child: return 0, zero out siginfo.
            let zeroed: linux_raw_sys::general::siginfo = unsafe { core::mem::zeroed() };
            infop.vm_write(zeroed)?;
            Ok(Some(0))
        } else {
            Ok(None)
        }
    };

    block_on(interruptible(poll_fn(|cx| {
        match check_children().transpose() {
            Some(res) => Poll::Ready(res),
            None => {
                proc_data.child_exit_event.register(cx.waker());
                match check_children().transpose() {
                    Some(res) => Poll::Ready(res),
                    None => Poll::Pending,
                }
            }
        }
    })))?
}
