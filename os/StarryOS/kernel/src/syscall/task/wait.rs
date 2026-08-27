use alloc::{sync::Arc, vec::Vec};

use ax_task::{
    current,
    future::{block_on, interruptible},
};
use bitflags::bitflags;
use linux_raw_sys::general::{
    __WALL, __WCLONE, __WNOTHREAD, P_ALL, P_PGID, P_PID, P_PIDFD, WCONTINUED, WEXITED, WNOHANG,
    WNOWAIT, WUNTRACED,
};
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};

use super::ptrace::PTRACE_EVENT_STOP;
use crate::{
    Errno, StarryError, StarryResult,
    file::{PidFd, get_file_like},
    task::{
        AsThread, JobStatus, PgidNumber, PidIdentity, PidIdentityId, PidNumber, Process,
        ProcessData, ProcessGroup, ROOT_PID_NS, Tgid, Tid, TidNumber, current_pid_view,
        decode_wait_status, get_process_data_by_number, get_task_by_number, get_zombie_cred,
        is_reaped_process, is_zombie_clone_child, is_zombie_process, processes, reap_process,
        traced_zombies_for, wait_on_pollset, zombie_wait_parent_tid,
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

#[derive(Clone)]
enum WaitTarget {
    /// Wait for any child process
    Any,
    /// Wait for the exact process or traced-thread generation.
    Identity(Arc<PidIdentity>),
    /// Wait for children in one exact process-group generation.
    Group(Arc<ProcessGroup>),
    /// Wait for the exact generation referenced by a pidfd.
    PidFd(Arc<PidIdentity>),
}

enum WaitSelector {
    AnyChild,
    CurrentProcessGroup,
    ProcessOrThread(WaitProcessOrThreadNumber),
    ProcessGroup(PgidNumber),
}

/// A positive wait selector whose Linux semantics intentionally accept either
/// a child TGID or a ptrace-visible child TID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WaitProcessOrThreadNumber(PidNumber);

impl WaitProcessOrThreadNumber {
    fn parse(number: u32) -> StarryResult<Self> {
        Ok(Self(PidNumber::try_from(number)?))
    }

    const fn pid_number(self) -> PidNumber {
        self.0
    }
}

impl TryFrom<i32> for WaitSelector {
    type Error = StarryError;

    fn try_from(pid: i32) -> Result<Self, Self::Error> {
        match pid {
            -1 => Ok(Self::AnyChild),
            0 => Ok(Self::CurrentProcessGroup),
            1.. => Ok(Self::ProcessOrThread(WaitProcessOrThreadNumber::parse(
                pid as u32,
            )?)),
            ..-1 => Ok(Self::ProcessGroup(PgidNumber::try_from(
                pid.checked_neg()
                    .ok_or_else(|| StarryError::from(Errno::ESRCH))? as u32,
            )?)),
        }
    }
}

enum WaitIdSelector {
    All,
    ProcessOrThread(WaitProcessOrThreadNumber),
    CurrentProcessGroup,
    ProcessGroup(PgidNumber),
    PidFd(i32),
}

impl WaitIdSelector {
    fn parse(idtype: u32, id: i32) -> StarryResult<Self> {
        match idtype {
            P_ALL => Ok(Self::All),
            P_PID if id > 0 => Ok(Self::ProcessOrThread(WaitProcessOrThreadNumber::parse(
                id as u32,
            )?)),
            P_PID => Err(StarryError::InvalidInput),
            P_PGID if id == 0 => Ok(Self::CurrentProcessGroup),
            P_PGID if id > 0 => Ok(Self::ProcessGroup(PgidNumber::try_from(id as u32)?)),
            P_PGID => Err(StarryError::InvalidInput),
            P_PIDFD => Ok(Self::PidFd(id)),
            _ => Err(StarryError::InvalidInput),
        }
    }
}

impl WaitTarget {
    fn identity_matches_thread(identity: &PidIdentity, child: &Process) -> bool {
        identity
            .live_task()
            .is_some_and(|task| core::ptr::eq(Arc::as_ref(&task.as_thread().proc_data.proc), child))
    }

    fn matches(&self, child: &Process) -> bool {
        match self {
            WaitTarget::Any => true,
            WaitTarget::Identity(identity) => identity.matches_process(child),
            WaitTarget::Group(group) => Arc::ptr_eq(group, &child.group()),
            WaitTarget::PidFd(identity) => identity.matches_process(child),
        }
    }

    fn matches_process_or_thread(&self, child: &Process) -> bool {
        self.matches(child)
            || matches!(self, WaitTarget::Identity(identity) if Self::identity_matches_thread(identity, child))
    }

    fn ptrace_report_pid(&self, child: &Process, data: &crate::task::ProcessData) -> u32 {
        match self {
            WaitTarget::Identity(identity)
                if identity.matches_process(child)
                    || Self::identity_matches_thread(identity, child) =>
            {
                visible_identity(identity)
            }
            WaitTarget::PidFd(identity) => visible_identity(identity),
            _ => visible_root_tid(
                data.ptrace_stop_tid()
                    .unwrap_or_else(|| TidNumber::from(child.pid().pid_number())),
            ),
        }
    }

    fn ptrace_preferred_stop_tid(&self, child: &Process) -> Option<TidNumber> {
        match self {
            WaitTarget::Identity(identity)
                if identity.matches_process(child)
                    || Self::identity_matches_thread(identity, child) =>
            {
                Some(TidNumber::from(identity.root_number()))
            }
            WaitTarget::PidFd(identity) => Some(TidNumber::from(identity.root_number())),
            _ => None,
        }
    }

    fn ptrace_requires_exact_stop(&self, child: &Process) -> bool {
        matches!(self, WaitTarget::Identity(identity)
            if !identity.matches_process(child) && Self::identity_matches_thread(identity, child))
    }
}

fn visible_identity(identity: &PidIdentity) -> u32 {
    current_pid_view()
        .visible_number(identity)
        .expect("wait target lost visibility before reporting")
        .get()
}

fn visible_process(process: &Process) -> u32 {
    visible_identity(&process.identity())
}

fn visible_root_tid(tid: TidNumber) -> u32 {
    ROOT_PID_NS
        .lookup(tid.pid_number())
        .map(|identity| visible_identity(&identity))
        .unwrap_or_else(|| tid.get())
}

fn waitid_pidfd_target(fd: i32) -> StarryResult<WaitTarget> {
    if fd < 0 {
        return Err(StarryError::InvalidInput);
    }
    let pidfd = get_file_like(fd)?
        .downcast_arc::<PidFd>()
        .map_err(|_| StarryError::BadFileDescriptor)?;
    Ok(WaitTarget::PidFd(pidfd.identity()))
}
fn stopped_wait_signo(data: &ProcessData, signo: Signo) -> i32 {
    let event = data.ptrace_event().unwrap_or(0);
    let mut wait_signo = if event != 0 && event != PTRACE_EVENT_STOP {
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
    get_zombie_cred(child.pid_number())
        .map(|cred| cred.uid)
        .or_else(|| {
            child.threads().into_iter().find_map(|tid| {
                get_task_by_number(tid)
                    .ok()
                    .map(|task| task.as_thread().cred().uid)
            })
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

    fn matches_process(&self, child: &Process, current_tid: TidNumber) -> bool {
        if self.no_thread {
            let wait_parent_tid = get_process_data_by_number(child.pid_number())
                .ok()
                .map(|data| data.wait_parent_tid)
                .or_else(|| zombie_wait_parent_tid(child.pid_number()));
            if wait_parent_tid != Some(current_tid) {
                return false;
            }
        }

        let is_clone_child = get_process_data_by_number(child.pid_number())
            .ok()
            .map(|data| data.is_clone_child())
            .or_else(|| is_zombie_clone_child(child.pid_number()))
            .unwrap_or(false);
        self.matches_clone_kind(is_clone_child)
    }
}

fn waitable_processes(
    proc: &Process,
    target: &WaitTarget,
    tracer: PidIdentityId,
    current_tid: TidNumber,
    filter: WaitChildFilter,
) -> Vec<Arc<Process>> {
    let mut candidates = match target {
        WaitTarget::PidFd(identity) => identity
            .public_process()
            .ok()
            .filter(|child| {
                child
                    .parent()
                    .is_some_and(|parent| core::ptr::eq(Arc::as_ref(&parent), proc))
                    && filter.matches_process(child, current_tid)
            })
            .into_iter()
            .collect::<Vec<_>>(),
        _ => proc
            .children()
            .into_iter()
            .filter(|child| target.matches(child) && filter.matches_process(child, current_tid))
            .collect::<Vec<_>>(),
    };

    for data in processes() {
        let traced = data
            .ptrace_tracer_identity()
            .is_some_and(|identity| identity.id() == tracer);
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

    for zombie in traced_zombies_for(tracer) {
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

pub fn sys_waitpid(pid: i32, exit_code: *mut i32, options: u32) -> StarryResult<isize> {
    let options = WaitPidOptions::from_bits(options).ok_or(StarryError::InvalidInput)?;
    info!("sys_waitpid <= pid: {pid:?}, options: {options:?}");

    let curr = current();
    let thr = curr.as_thread();
    let proc = &thr.proc_data.proc;

    let target = match WaitSelector::try_from(pid)? {
        WaitSelector::AnyChild => WaitTarget::Any,
        WaitSelector::CurrentProcessGroup => WaitTarget::Group(proc.group()),
        WaitSelector::ProcessOrThread(number) => {
            let identity = current_pid_view()
                .resolve_identity(number.pid_number())
                .and_then(|identity| {
                    (identity.has_role::<Tgid>() || identity.has_role::<Tid>())
                        .then_some(identity)
                        .ok_or(StarryError::NoSuchProcess)
                })
                .map_err(|_| StarryError::from(Errno::ECHILD))?;
            WaitTarget::Identity(identity)
        }
        WaitSelector::ProcessGroup(pgid) => WaitTarget::Group(
            current_pid_view()
                .resolve_group(pgid)
                .map_err(|_| StarryError::from(Errno::ECHILD))?,
        ),
    };

    let scan_children = || {
        waitable_processes(
            proc,
            &target,
            proc.identity().id(),
            thr.tid_number(),
            WaitChildFilter::from_waitpid_options(&options),
        )
    };
    if scan_children().is_empty() {
        return Err(StarryError::from(Errno::ECHILD));
    }

    let proc_data = curr.as_thread().proc_data.clone();
    let check_children = || {
        // Linux rescans the authoritative child and ptrace relationships after
        // every wake; another thread can publish an eligible child while this
        // waiter is blocked.
        let children = scan_children();
        if let Some((child, data, stop_tid, signo)) = children.iter().find_map(|child| {
            get_process_data_by_number(child.pid_number())
                .ok()
                .and_then(|data| {
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
        } else if let Some(child) = children.iter().find(|child| is_zombie_process(child)) {
            // Copy status before claiming the unique reap transition. A failed
            // user write leaves the zombie available for a later retry.
            if let Some(exit_code) = exit_code.nullable() {
                exit_code.vm_write(child.exit_code())?;
            }
            let reported_pid = visible_process(child);
            if let Some(cpu_time) = reap_process(child) {
                proc_data.add_child_cpu_time(cpu_time.user(), cpu_time.system());
                return Ok(Some(reported_pid as _));
            }
        }

        // Job-control status: a stopped (WUNTRACED) or continued (WCONTINUED)
        // child reports its status without being reaped, unlike a zombie.
        let want_stopped = options.contains(WaitPidOptions::WUNTRACED);
        let want_continued = options.contains(WaitPidOptions::WCONTINUED);
        if want_stopped || want_continued {
            for child in &children {
                let Ok(cdata) = get_process_data_by_number(child.pid_number()) else {
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
                    return Ok(Some(visible_process(child) as _));
                }
            }
        }

        if children.iter().all(is_reaped_process) {
            Err(StarryError::from(Errno::ECHILD))
        } else if options.contains(WaitPidOptions::WNOHANG) {
            Ok(Some(0))
        } else {
            Ok(None)
        }
    };

    block_on(interruptible(wait_on_pollset(
        &proc_data.child_exit_event,
        || check_children().transpose(),
    )))?
}

pub fn sys_waitid(
    idtype: u32,
    id: i32,
    infop: *mut linux_raw_sys::general::siginfo,
    options: u32,
) -> StarryResult<isize> {
    let curr = current();
    let thr = curr.as_thread();
    let proc = &thr.proc_data.proc;

    let target = match WaitIdSelector::parse(idtype, id)? {
        WaitIdSelector::All => WaitTarget::Any,
        WaitIdSelector::ProcessOrThread(number) => {
            let identity = current_pid_view()
                .resolve_identity(number.pid_number())
                .and_then(|identity| {
                    (identity.has_role::<Tgid>() || identity.has_role::<Tid>())
                        .then_some(identity)
                        .ok_or(StarryError::NoSuchProcess)
                })
                .map_err(|_| StarryError::from(Errno::ECHILD))?;
            WaitTarget::Identity(identity)
        }
        WaitIdSelector::CurrentProcessGroup => WaitTarget::Group(proc.group()),
        WaitIdSelector::ProcessGroup(pgid) => WaitTarget::Group(
            current_pid_view()
                .resolve_group(pgid)
                .map_err(|_| StarryError::from(Errno::ECHILD))?,
        ),
        WaitIdSelector::PidFd(fd) => waitid_pidfd_target(fd)?,
    };

    let options = WaitIdOptions::from_bits(options).ok_or(StarryError::InvalidInput)?;
    if !options
        .intersects(WaitIdOptions::WEXITED | WaitIdOptions::WUNTRACED | WaitIdOptions::WCONTINUED)
    {
        return Err(StarryError::InvalidInput);
    }

    info!("sys_waitid <= idtype: {idtype}, id: {id}, options: {options:?}");

    let scan_children = || {
        waitable_processes(
            proc,
            &target,
            proc.identity().id(),
            thr.tid_number(),
            WaitChildFilter::from_waitid_options(&options),
        )
    };
    if scan_children().is_empty() {
        return Err(StarryError::from(Errno::ECHILD));
    }

    let proc_data = curr.as_thread().proc_data.clone();
    let check_children = || {
        let children = scan_children();
        if options.contains(WaitIdOptions::WUNTRACED)
            && let Some((child, data, stop_tid, signo)) = children.iter().find_map(|child| {
                get_process_data_by_number(child.pid_number())
                    .ok()
                    .and_then(|data| {
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
                infop.vm_write(siginfo.0)?;
            }
            if !options.contains(WaitIdOptions::WNOWAIT) {
                data.mark_ptrace_stop_reported_for(stop_tid);
            }

            return Ok(Some(0));
        }

        let want_stopped = options.contains(WaitIdOptions::WUNTRACED);
        let want_continued = options.contains(WaitIdOptions::WCONTINUED);
        if want_stopped || want_continued {
            for child in &children {
                let Ok(data) = get_process_data_by_number(child.pid_number()) else {
                    continue;
                };
                if let Some(status) = data.peek_job_status_if(want_stopped, want_continued) {
                    let (code, status) = match status {
                        JobStatus::Stopped(signo) => {
                            (linux_raw_sys::general::CLD_STOPPED as i32, signo as i32)
                        }
                        JobStatus::Continued => (
                            linux_raw_sys::general::CLD_CONTINUED as i32,
                            Signo::SIGCONT as i32,
                        ),
                    };
                    if let Some(infop) = infop.nullable() {
                        let siginfo = SignalInfo::new_sigchld(
                            visible_process(child),
                            child_uid(child),
                            code,
                            status,
                        );
                        infop.vm_write(siginfo.0)?;
                    }
                    if !options.contains(WaitIdOptions::WNOWAIT) {
                        data.take_job_status_if(want_stopped, want_continued);
                    }
                    return Ok(Some(0));
                }
            }
        }

        if options.contains(WaitIdOptions::WEXITED)
            && let Some(child) = children.iter().find(|child| is_zombie_process(child))
        {
            let child_pid = visible_process(child);
            let (code, status) = decode_wait_status(child.exit_code());
            let child_uid = child_uid(child);

            if let Some(infop) = infop.nullable() {
                let siginfo = SignalInfo::new_sigchld(child_pid, child_uid, code, status);
                infop.vm_write(siginfo.0)?;
            }

            if options.contains(WaitIdOptions::WNOWAIT) {
                return Ok(Some(0));
            }
            if let Some(cpu_time) = reap_process(child) {
                proc_data.add_child_cpu_time(cpu_time.user(), cpu_time.system());
                return Ok(Some(0));
            }
        }

        if children.iter().all(is_reaped_process) {
            Err(StarryError::from(Errno::ECHILD))
        } else if options.contains(WaitIdOptions::WNOHANG) {
            if let Some(infop) = infop.nullable() {
                let zeroed: linux_raw_sys::general::siginfo = unsafe { core::mem::zeroed() };
                infop.vm_write(zeroed)?;
            }
            Ok(Some(0))
        } else {
            Ok(None)
        }
    };

    block_on(interruptible(wait_on_pollset(
        &proc_data.child_exit_event,
        || check_children().transpose(),
    )))?
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn waitpid_selector_preserves_linux_role_semantics() {
        assert!(matches!(
            WaitSelector::try_from(-1),
            Ok(WaitSelector::AnyChild)
        ));
        assert!(matches!(
            WaitSelector::try_from(0),
            Ok(WaitSelector::CurrentProcessGroup)
        ));
        assert!(matches!(
            WaitSelector::try_from(1),
            Ok(WaitSelector::ProcessOrThread(number)) if number.pid_number().get() == 1
        ));
        assert!(matches!(
            WaitSelector::try_from(-2),
            Ok(WaitSelector::ProcessGroup(pgid)) if pgid.get() == 2
        ));
        let error = match WaitSelector::try_from(i32::MIN) {
            Ok(_) => panic!("i32::MIN must not identify a process group"),
            Err(error) => error,
        };
        assert_eq!(error.linux_errno(), Errno::ESRCH);
    }

    #[test]
    fn waitid_selector_rejects_invalid_role_values() {
        assert!(matches!(
            WaitIdSelector::parse(P_PID, 1),
            Ok(WaitIdSelector::ProcessOrThread(number)) if number.pid_number().get() == 1
        ));
        assert!(matches!(
            WaitIdSelector::parse(P_PID, 0),
            Err(StarryError::InvalidInput)
        ));
        assert!(matches!(
            WaitIdSelector::parse(P_PGID, 0),
            Ok(WaitIdSelector::CurrentProcessGroup)
        ));
        assert!(matches!(
            WaitIdSelector::parse(P_PGID, -1),
            Err(StarryError::InvalidInput)
        ));
        assert!(matches!(
            WaitIdSelector::parse(P_PIDFD, -1),
            Ok(WaitIdSelector::PidFd(-1))
        ));
        assert!(matches!(
            WaitIdSelector::parse(u32::MAX, 1),
            Err(StarryError::InvalidInput)
        ));
    }
}
