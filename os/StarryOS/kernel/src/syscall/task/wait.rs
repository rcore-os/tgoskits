use alloc::{sync::Arc, vec::Vec};

use ax_errno::{AxError, AxResult, LinuxError};
use bitflags::bitflags;
use linux_raw_sys::general::{
    __WALL, __WCLONE, __WNOTHREAD, P_ALL, P_PGID, P_PID, P_PIDFD, WCONTINUED, WEXITED, WNOHANG,
    WNOWAIT, WUNTRACED,
};
use starry_process::{Pid, Process};
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    file::{PidFd, get_file_like},
    task::{
        JobStatus, ProcessData, current_user_task, decode_wait_status,
        future::{block_on_user, interruptible_for},
        get_process_data, get_task, get_zombie_cred, is_zombie_clone_child, processes,
        remove_process, traced_zombies_for, unregister_zombie, wait_on_pollset,
        zombie_wait_parent_tid,
    },
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

    fn matches_process_or_thread(&self, child: &Process) -> bool {
        self.matches(child) || matches!(self, WaitTarget::Pid(pid) if child.threads().contains(pid))
    }

    fn ptrace_report_pid(&self, child: &Process, data: &ProcessData) -> Pid {
        match self {
            WaitTarget::Pid(pid) if *pid == child.pid() || child.threads().contains(pid) => *pid,
            _ => data.ptrace_stop_tid().unwrap_or(child.pid()),
        }
    }

    fn ptrace_preferred_stop_tid(&self, child: &Process) -> Option<Pid> {
        match self {
            WaitTarget::Pid(pid) if *pid != child.pid() && child.threads().contains(pid) => {
                Some(*pid)
            }
            WaitTarget::Pid(pid) if *pid == child.pid() => Some(*pid),
            _ => None,
        }
    }

    fn ptrace_requires_exact_stop(&self, child: &Process) -> bool {
        matches!(self, WaitTarget::Pid(pid) if *pid != child.pid() && child.threads().contains(pid))
    }
}

fn waitid_pidfd_target(fd: i32) -> AxResult<WaitTarget> {
    if fd < 0 {
        return Err(AxError::InvalidInput);
    }
    let pidfd = get_file_like(fd)?
        .downcast_arc::<PidFd>()
        .map_err(|_| AxError::BadFileDescriptor)?;
    Ok(WaitTarget::Pid(pidfd.pid()))
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

#[derive(Debug, Clone, Copy)]
struct WaitChildFilter {
    wall: bool,
    clone: bool,
    no_thread: bool,
}

impl WaitChildFilter {
    fn from_waitpid_options(options: &WaitPidOptions) -> Self {
        Self {
            wall: options.contains(WaitPidOptions::WALL),
            clone: options.contains(WaitPidOptions::WCLONE),
            no_thread: options.contains(WaitPidOptions::WNOTHREAD),
        }
    }

    fn from_waitid_options(options: &WaitIdOptions) -> Self {
        Self {
            wall: options.contains(WaitIdOptions::WALL),
            clone: options.contains(WaitIdOptions::WCLONE),
            no_thread: options.contains(WaitIdOptions::WNOTHREAD),
        }
    }

    fn matches_clone_kind(&self, is_clone_child: bool) -> bool {
        self.wall || is_clone_child == self.clone
    }

    fn matches_process(&self, child: &Process, current_tid: Pid) -> bool {
        if self.no_thread {
            let wait_parent_tid = get_process_data(child.pid())
                .ok()
                .map(|data| data.wait_parent_tid)
                .or_else(|| zombie_wait_parent_tid(child.pid()));
            if wait_parent_tid != Some(current_tid) {
                return false;
            }
        }

        let is_clone_child = get_process_data(child.pid())
            .ok()
            .map(|data| data.is_clone_child())
            .or_else(|| is_zombie_clone_child(child.pid()))
            .unwrap_or(false);
        self.matches_clone_kind(is_clone_child)
    }
}

fn waitable_processes(
    proc: &Process,
    target: WaitTarget,
    tracer_pid: Pid,
    current_tid: Pid,
    filter: WaitChildFilter,
) -> Vec<Arc<Process>> {
    let mut candidates = proc
        .children()
        .into_iter()
        .filter(|child| target.matches(child) && filter.matches_process(child, current_tid))
        .collect::<Vec<_>>();

    for data in processes() {
        let traced = data.ptrace_tracer_pid() == Some(tracer_pid);
        let proc = data.proc.clone();
        if traced
            && target.matches_process_or_thread(&proc)
            && filter.matches_process(&proc, current_tid)
            && !candidates
                .iter()
                .any(|candidate| candidate.pid() == proc.pid())
        {
            candidates.push(proc);
        }
    }

    for zombie in traced_zombies_for(tracer_pid) {
        if target.matches(&zombie)
            && filter.matches_process(&zombie, current_tid)
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

    let curr = current_user_task();
    let thr = curr.as_thread();
    let proc = &thr.proc_data.proc;

    let target = if pid == -1 {
        WaitTarget::Any
    } else if pid == 0 {
        WaitTarget::Pgid(proc.group().pgid())
    } else if pid > 0 {
        WaitTarget::Pid(pid as _)
    } else {
        WaitTarget::Pgid(-pid as _)
    };

    let children = waitable_processes(
        proc,
        target,
        proc.pid(),
        thr.tid(),
        WaitChildFilter::from_waitpid_options(&options),
    );
    if children.is_empty() {
        return Err(AxError::from(LinuxError::ECHILD));
    }

    let proc_data = curr.as_thread().proc_data.clone();
    let check_children = || {
        if let Some((child, data, stop_tid, signo)) = children.iter().find_map(|child| {
            get_process_data(child.pid()).ok().and_then(|data| {
                let preferred_tid = target.ptrace_preferred_stop_tid(child);
                let stop = if target.ptrace_requires_exact_stop(child) {
                    preferred_tid.and_then(|tid| data.ptrace_unreported_stop_for(tid))
                } else {
                    data.ptrace_unreported_stop(preferred_tid)
                };
                stop.map(|(stop_tid, signo)| (child, data, stop_tid, signo))
            })
        }) {
            data.select_ptrace_stop(stop_tid);
            let wait_pid = target.ptrace_report_pid(child, &data);
            let status = stopped_wait_status(&data, signo);
            if let Some(exit_code) = exit_code.nullable() {
                exit_code.vm_write(status)?;
            }
            data.mark_ptrace_stop_reported_for(stop_tid);
            return Ok(Some(wait_pid as _));
        } else if let Some(child) = children.iter().find(|child| child.is_zombie()) {
            // Accumulate child's CPU time before freeing.
            for tid in child.threads() {
                if let Ok(task) = get_task(tid) {
                    let thr = task.as_thread();
                    let (utime, stime) = thr.cpu_time.output();
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

    let task = current_user_task();
    block_on_user(
        &task,
        interruptible_for(
            &task,
            wait_on_pollset(&proc_data.child_exit_event, || check_children().transpose()),
        ),
    )?
}

pub fn sys_waitid(
    idtype: u32,
    id: i32,
    infop: *mut linux_raw_sys::general::siginfo,
    options: u32,
) -> AxResult<isize> {
    let curr = current_user_task();
    let thr = curr.as_thread();
    let proc = &thr.proc_data.proc;

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
        P_PIDFD => waitid_pidfd_target(id)?,
        _ => return Err(AxError::InvalidInput),
    };

    let options = WaitIdOptions::from_bits(options).ok_or(AxError::InvalidInput)?;
    if !options
        .intersects(WaitIdOptions::WEXITED | WaitIdOptions::WUNTRACED | WaitIdOptions::WCONTINUED)
    {
        return Err(AxError::InvalidInput);
    }

    info!("sys_waitid <= idtype: {idtype}, id: {id}, options: {options:?}");

    let children = waitable_processes(
        proc,
        target,
        proc.pid(),
        thr.tid(),
        WaitChildFilter::from_waitid_options(&options),
    );
    if children.is_empty() {
        return Err(AxError::from(LinuxError::ECHILD));
    }

    let proc_data = curr.as_thread().proc_data.clone();
    let check_children = || {
        if options.contains(WaitIdOptions::WUNTRACED)
            && let Some((child, data, stop_tid, signo)) = children.iter().find_map(|child| {
                get_process_data(child.pid()).ok().and_then(|data| {
                    let preferred_tid = target.ptrace_preferred_stop_tid(child);
                    let stop = if target.ptrace_requires_exact_stop(child) {
                        preferred_tid.and_then(|tid| data.ptrace_unreported_stop_for(tid))
                    } else {
                        data.ptrace_unreported_stop(preferred_tid)
                    };
                    stop.map(|(stop_tid, signo)| (child, data, stop_tid, signo))
                })
            })
        {
            let child_pid = target.ptrace_report_pid(child, &data);
            let child_uid = child_uid(child);
            data.select_ptrace_stop(stop_tid);

            if let Some(infop) = infop.nullable() {
                let siginfo = SignalInfo::new_sigchld(
                    child_pid,
                    child_uid,
                    linux_raw_sys::general::CLD_TRAPPED as i32,
                    stopped_wait_signo(&data, signo),
                );
                // SAFETY: new_sigchld zeroes the complete siginfo storage
                // before setting the active union fields.
                unsafe { infop.vm_write_abi(&siginfo.0)? };
            }
            if !options.contains(WaitIdOptions::WNOWAIT) {
                data.mark_ptrace_stop_reported_for(stop_tid);
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
                // SAFETY: new_sigchld zeroes the complete siginfo storage
                // before setting the active union fields.
                unsafe { infop.vm_write_abi(&siginfo.0)? };
            }

            if !options.contains(WaitIdOptions::WNOWAIT) {
                for tid in child.threads() {
                    if let Ok(task) = get_task(tid) {
                        let thr = task.as_thread();
                        let (utime, stime) = thr.cpu_time.output();
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
                // SAFETY: zeroed initializes all bytes of the siginfo union and
                // is the Linux waitid WNOHANG sentinel representation.
                unsafe { infop.vm_write_abi(&zeroed)? };
            }
            Ok(Some(0))
        } else {
            Ok(None)
        }
    };

    let task = current_user_task();
    block_on_user(
        &task,
        interruptible_for(
            &task,
            wait_on_pollset(&proc_data.child_exit_event, || check_children().transpose()),
        ),
    )?
}
