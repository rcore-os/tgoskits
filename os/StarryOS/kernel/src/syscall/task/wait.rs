use alloc::vec::Vec;
use core::{future::poll_fn, task::Poll};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_task::{
    current,
    future::{block_on, interruptible},
};
use bitflags::bitflags;
use linux_raw_sys::general::{__WALL, __WCLONE, __WNOTHREAD, WCONTINUED, WNOHANG, WUNTRACED};
use starry_process::{Pid, Process};
use starry_vm::{VmMutPtr, VmPtr};

use crate::task::{AsThread, get_task, remove_process, unregister_zombie};

bitflags! {
    #[derive(Debug)]
    struct WaitOptions: u32 {
        /// Do not block when there are no processes wishing to report status.
        const WNOHANG = WNOHANG;
        /// Report the status of selected processes which are stopped due to a
        /// `SIGTTIN`, `SIGTTOU`, `SIGTSTP`, or `SIGSTOP` signal.
        const WUNTRACED = WUNTRACED;
        /// Report the status of selected processes that have continued from a
        /// job control stop by receiving a `SIGCONT` signal.
        const WCONTINUED = WCONTINUED;

        /// Don't wait on children of other threads in this group
        const WNOTHREAD = __WNOTHREAD;
        /// Wait on all children, regardless of type
        const WALL = __WALL;
        /// Wait for "clone" children only.
        const WCLONE = __WCLONE;
    }
}

#[derive(Debug, Clone, Copy)]
enum WaitPid {
    /// Wait for any child process
    Any,
    /// Wait for the child whose process ID is equal to the value.
    Pid(Pid),
    /// Wait for any child process whose process group ID is equal to the value.
    Pgid(Pid),
}

impl WaitPid {
    fn apply(&self, child: &Process) -> bool {
        match self {
            WaitPid::Any => true,
            WaitPid::Pid(pid) => child.pid() == *pid,
            WaitPid::Pgid(pgid) => child.group().pgid() == *pgid,
        }
    }
}

pub fn sys_waitpid(pid: i32, exit_code: *mut i32, options: u32) -> AxResult<isize> {
    let options = WaitOptions::from_bits(options).ok_or(AxError::InvalidInput)?;
    info!("sys_waitpid <= pid: {pid:?}, options: {options:?}");

    let curr = current();
    let proc = &curr.as_thread().proc_data.proc;

    let pid = if pid == -1 {
        WaitPid::Any
    } else if pid == 0 {
        WaitPid::Pgid(proc.group().pgid())
    } else if pid > 0 {
        WaitPid::Pid(pid as _)
    } else {
        WaitPid::Pgid(-pid as _)
    };

    // FIXME: add back support for WALL & WCLONE, since ProcessData may drop before
    // Process now.
    let children = proc
        .children()
        .into_iter()
        .filter(|child| pid.apply(child))
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
        } else if options.contains(WaitOptions::WNOHANG) {
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
