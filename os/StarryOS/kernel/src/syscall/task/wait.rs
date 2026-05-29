use alloc::{sync::Arc, vec::Vec};
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
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};

use crate::task::{
    AsThread, JobStatus, ProcessData, decode_wait_status, get_process_data, get_task,
    get_zombie_cred, processes, remove_process, traced_zombies_for, unregister_zombie,
};

const PTRACE_O_TRACESYSGOOD: usize = 1;

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

fn stopped_wait_signo(data: &ProcessData, signo: Signo) -> i32 {
    let event = data.ptrace_event().unwrap_or(0);
    let mut wait_signo = if event != 0 {
        Signo::SIGTRAP as i32
    } else {
        signo as i32
    };
    if event == 0
        && signo == Signo::SIGTRAP
        && data.is_ptrace_syscall_stop()
        && data.ptrace_options() & PTRACE_O_TRACESYSGOOD != 0
    {
        wait_signo |= 0x80;
    }
    wait_signo
}

fn stopped_wait_status(data: &ProcessData, signo: Signo) -> i32 {
    let event = data.ptrace_event().unwrap_or(0) as i32;
    let wait_signo = stopped_wait_signo(data, signo);
    (event << 16) | (wait_signo << 8) | 0x7f
}

fn child_uid(child: &Process) -> u32 {
    get_zombie_cred(child.pid())
        .map(|cred| cred.uid)
        .or_else(|| {
            child
                .threads()
                .into_iter()
                .find_map(|tid| get_task(tid).ok().map(|task| task.as_thread().cred().uid))
        })
        .unwrap_or(0)
}

fn waitable_processes(proc: &Process, target: WaitTarget, tracer_pid: Pid) -> Vec<Arc<Process>> {
    let mut candidates = proc
        .children()
        .into_iter()
        .filter(|child| target.matches(child))
        .collect::<Vec<_>>();

    for data in processes() {
        let traced = data.ptrace_tracer_pid() == Some(tracer_pid);
        let proc = data.proc.clone();
        if traced
            && target.matches(&proc)
            && !candidates
                .iter()
                .any(|candidate| candidate.pid() == proc.pid())
        {
            candidates.push(proc);
        }
    }

    for zombie in traced_zombies_for(tracer_pid) {
        if target.matches(&zombie)
            && !candidates
                .iter()
                .any(|candidate| candidate.pid() == zombie.pid())
        {
            candidates.push(zombie);
        }
    }

    candidates
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
    let children = waitable_processes(proc, target, proc.pid());
    if children.is_empty() {
        return Err(AxError::from(LinuxError::ECHILD));
    }

    let proc_data = curr.as_thread().proc_data.clone();
    let check_children = || {
        if let Some((child, data, signo)) = children.iter().find_map(|child| {
            get_process_data(child.pid())
                .ok()
                .filter(|data| !data.ptrace_stop_reported())
                .and_then(|data| data.ptrace_stop_signo().map(|signo| (child, data, signo)))
        }) {
            if let Some(exit_code) = exit_code.nullable() {
                exit_code.vm_write(stopped_wait_status(&data, signo))?;
            }
            data.mark_ptrace_stop_reported();
            return Ok(Some(child.pid() as _));
        } else if let Some(child) = children.iter().find(|child| child.is_zombie()) {
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
            return Ok(Some(child.pid() as _));
        }

        // Job-control status: a stopped (WUNTRACED) or continued (WCONTINUED)
        // child reports its status without being reaped, unlike a zombie.
        let want_stopped = options.contains(WaitPidOptions::WUNTRACED);
        let want_continued = options.contains(WaitPidOptions::WCONTINUED);
        if want_stopped || want_continued {
            for child in &children {
                let Ok(cdata) = get_process_data(child.pid()) else {
                    continue;
                };
                if let Some(status) = cdata.peek_job_status_if(want_stopped, want_continued) {
                    // Linux wait status encoding: stopped = (signo << 8) | 0x7f
                    // (W_STOPCODE), continued = 0xffff (__W_CONTINUED).
                    let raw = match status {
                        JobStatus::Stopped(signo) => ((signo as i32) << 8) | 0x7f,
                        JobStatus::Continued => 0xffff,
                    };
                    // Publish to userspace before consuming, so a faulting
                    // `exit_code` pointer leaves the report intact to retry
                    // (mirrors the zombie-reap ordering above).
                    if let Some(exit_code) = exit_code.nullable() {
                        exit_code.vm_write(raw)?;
                    }
                    cdata.take_job_status_if(want_stopped, want_continued);
                    return Ok(Some(child.pid() as _));
                }
            }
        }

        if options.contains(WaitPidOptions::WNOHANG) {
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

pub fn sys_waitid(
    idtype: u32,
    id: i32,
    infop: *mut linux_raw_sys::general::siginfo,
    options: u32,
) -> AxResult<isize> {
    use linux_raw_sys::general::P_PGID;

    let curr = current();
    let proc = &curr.as_thread().proc_data.proc;

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
            if id < 0 {
                return Err(AxError::InvalidInput);
            }
            let pgid = if id == 0 {
                proc.group().pgid()
            } else {
                id as Pid
            };
            WaitTarget::Pgid(pgid)
        }
        _ => return Err(AxError::InvalidInput),
    };

    let options = WaitIdOptions::from_bits(options).ok_or(AxError::InvalidInput)?;
    if !options
        .intersects(WaitIdOptions::WEXITED | WaitIdOptions::WUNTRACED | WaitIdOptions::WCONTINUED)
    {
        return Err(AxError::InvalidInput);
    }

    info!("sys_waitid <= idtype: {idtype}, id: {id}, options: {options:?}");

    let children = waitable_processes(proc, target, proc.pid());
    if children.is_empty() {
        return Err(AxError::from(LinuxError::ECHILD));
    }

    let proc_data = curr.as_thread().proc_data.clone();
    let check_children = || {
        if options.contains(WaitIdOptions::WUNTRACED)
            && let Some((child, data, signo)) = children.iter().find_map(|child| {
                get_process_data(child.pid())
                    .ok()
                    .filter(|data| !data.ptrace_stop_reported())
                    .and_then(|data| data.ptrace_stop_signo().map(|signo| (child, data, signo)))
            })
        {
            let child_pid = child.pid();
            let child_uid = child_uid(child);

            if let Some(infop) = infop.nullable() {
                let siginfo = SignalInfo::new_sigchld(
                    child_pid,
                    child_uid,
                    linux_raw_sys::general::CLD_TRAPPED as i32,
                    stopped_wait_signo(&data, signo),
                );
                infop.vm_write(siginfo.0)?;
            }
            if !options.contains(WaitIdOptions::WNOWAIT)
                && let Ok(data) = get_process_data(child_pid)
            {
                data.mark_ptrace_stop_reported();
            }

            return Ok(Some(0));
        }

        if options.contains(WaitIdOptions::WEXITED)
            && let Some(child) = children.iter().find(|child| child.is_zombie())
        {
            let child_pid = child.pid();
            let (code, status) = decode_wait_status(child.exit_code());
            let child_uid = child_uid(child);

            if let Some(infop) = infop.nullable() {
                let siginfo = SignalInfo::new_sigchld(child_pid, child_uid, code, status);
                infop.vm_write(siginfo.0)?;
            }

            if !options.contains(WaitIdOptions::WNOWAIT) {
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
            return Ok(Some(0));
        }

        if options.contains(WaitIdOptions::WNOHANG) {
            if let Some(infop) = infop.nullable() {
                let zeroed: linux_raw_sys::general::siginfo = unsafe { core::mem::zeroed() };
                infop.vm_write(zeroed)?;
            }
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
