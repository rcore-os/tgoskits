use alloc::sync::Arc;
use core::{future::poll_fn, task::Poll};

use ax_runtime::hal::cpu::uspace::UserContext;
use ax_task::{
    current,
    future::{self, block_on},
};
use linux_raw_sys::general::{
    MINSIGSTKSZ, SI_TKILL, SI_USER, SIG_BLOCK, SIG_SETMASK, SIG_UNBLOCK, SS_DISABLE, SS_FLAG_BITS,
    SS_ONSTACK, kernel_sigaction, siginfo, timespec,
};
use starry_signal::{SignalInfo, SignalSet, SignalStack, Signo};
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    Errno, StarryError, StarryResult,
    task::{
        AsThread, PgidNumber, PidIdentity, TgidNumber, TidNumber, block_next_signal, check_signals,
        current_pid_view, get_user_task_by_number, processes, send_signal_to_process_data,
        send_signal_to_task,
    },
    time::TimeValueLike,
};

pub(crate) fn check_sigset_size(size: usize) -> StarryResult<()> {
    // Align with Linux raw syscall semantics (for ABI param 'sigmask'): when sigsetsize is checked,
    // it must exactly match the kernel SignalSet size (8 bytes).
    if size != size_of::<SignalSet>() {
        return Err(StarryError::InvalidInput);
    }
    Ok(())
}

fn parse_signo(signo: u32) -> StarryResult<Signo> {
    Signo::from_repr(signo as u8).ok_or(StarryError::InvalidInput)
}

pub fn sys_rt_sigprocmask(
    how: i32,
    set: *const SignalSet,
    oldset: *mut SignalSet,
    sigsetsize: usize,
) -> StarryResult<isize> {
    check_sigset_size(sigsetsize)?;

    let curr = current();
    let sig = &curr.as_thread().signal;
    let old = sig.blocked();

    if let Some(oldset) = oldset.nullable() {
        oldset.vm_write(old)?;
    }

    if let Some(set) = set.nullable() {
        let set = unsafe { set.vm_read_uninit()?.assume_init() };

        let set = match how as u32 {
            SIG_BLOCK => old | set,
            SIG_UNBLOCK => old & !set,
            SIG_SETMASK => set,
            _ => return Err(StarryError::InvalidInput),
        };

        debug!("sys_rt_sigprocmask <= {set:?}");
        sig.set_blocked(set);
    }

    Ok(0)
}

pub fn sys_rt_sigaction(
    signo: u32,
    act: *const kernel_sigaction,
    oldact: *mut kernel_sigaction,
    sigsetsize: usize,
) -> StarryResult<isize> {
    check_sigset_size(sigsetsize)?;

    let signo = parse_signo(signo)?;
    if matches!(signo, Signo::SIGKILL | Signo::SIGSTOP) {
        return Err(StarryError::InvalidInput);
    }

    Ok(current()
        .as_thread()
        .proc_data
        .signal
        .set_action(signo, act, oldact)?)
}

pub fn sys_rt_sigpending(set: *mut SignalSet, sigsetsize: usize) -> StarryResult<isize> {
    check_sigset_size(sigsetsize)?;
    set.vm_write(current().as_thread().signal.pending())?;
    Ok(0)
}

pub(crate) fn make_siginfo(signo: u32, code: i32) -> StarryResult<Option<SignalInfo>> {
    if signo == 0 {
        return Ok(None);
    }
    let signo = parse_signo(signo)?;
    let curr = current();
    let thread = curr.as_thread();
    Ok(Some(SignalInfo::new_user(
        signo,
        code,
        current_pid_view()
            .visible_number(&thread.proc_data.identity())
            .expect("current process is visible in its active PID namespace")
            .get(),
        thread.cred().uid,
    )))
}

/// Check whether the current process has permission to send a signal to
/// `target_pid`.
///
/// Permission rules:
/// - Root (euid==0, approximating CAP_KILL) can signal anyone
/// - Same process is always allowed
/// - Otherwise: sender's {euid, uid} must match target's {uid, euid, suid}
///
/// TODO: SIGCONT is allowed to any process in the same session (job control).
/// Implementing this requires passing the signal number into this function
/// and checking session membership.
pub(crate) fn check_kill_permission_identity(target: &PidIdentity) -> StarryResult<()> {
    let sender = current().as_thread().cred();
    if sender.euid == 0 {
        return Ok(());
    }
    let self_identity = current().as_thread().proc_data.identity();
    if core::ptr::eq(target, Arc::as_ref(&self_identity)) {
        return Ok(());
    }
    let target_cred = if let Some(task) = target.live_task() {
        task.as_thread().cred()
    } else {
        target
            .zombie_snapshot(|zombie| zombie.cred.clone())
            .ok_or(StarryError::NoSuchProcess)?
    };
    if sender.euid == target_cred.uid
        || sender.euid == target_cred.euid
        || sender.euid == target_cred.suid
        || sender.uid == target_cred.uid
        || sender.uid == target_cred.euid
        || sender.uid == target_cred.suid
    {
        Ok(())
    } else {
        Err(StarryError::OperationNotPermitted)
    }
}

fn signal_user_process(identity: &PidIdentity, sig: Option<SignalInfo>) -> StarryResult<()> {
    if let Some(proc_data) = identity.live_data() {
        send_signal_to_process_data(&proc_data, sig)
    } else if identity.is_zombie() {
        Ok(())
    } else {
        Err(StarryError::NoSuchProcess)
    }
}

/// Send a signal to each member of a process group, checking
/// per-member permission. EPERM for individual members is swallowed
/// (matches Linux behavior).
fn kill_process_group_checked(pgid: PgidNumber, sig: Option<SignalInfo>) -> StarryResult<()> {
    let pg = current_pid_view().resolve_group(pgid)?;
    if let Some(sig) = sig {
        for proc in pg.processes() {
            if check_kill_permission_identity(&proc.identity()).is_ok()
                && let Some(proc_data) = proc.identity().live_data()
            {
                let _ = send_signal_to_process_data(&proc_data, Some(sig.clone()));
            }
        }
    }
    Ok(())
}

enum KillTarget {
    Process(TgidNumber),
    CurrentProcessGroup,
    AllPermittedProcesses,
    ProcessGroup(PgidNumber),
}

impl TryFrom<i32> for KillTarget {
    type Error = StarryError;

    fn try_from(pid: i32) -> Result<Self, Self::Error> {
        match pid {
            1.. => Ok(Self::Process(TgidNumber::try_from(pid as u32)?)),
            0 => Ok(Self::CurrentProcessGroup),
            -1 => Ok(Self::AllPermittedProcesses),
            ..-1 => Ok(Self::ProcessGroup(PgidNumber::try_from(
                pid.checked_neg().ok_or(StarryError::InvalidInput)? as u32,
            )?)),
        }
    }
}

pub fn sys_kill(pid: i32, signo: u32) -> StarryResult<isize> {
    debug!("sys_kill: pid = {pid}, signo = {signo}");
    let sig = make_siginfo(signo, SI_USER as _)?;

    match KillTarget::try_from(pid)? {
        KillTarget::Process(tgid) => {
            let identity = current_pid_view().resolve_process(tgid)?;
            check_kill_permission_identity(&identity)?;
            if let Some(sig) = sig {
                let curr = current();
                let thread = curr.as_thread();
                let signo = sig.signo();
                if Arc::ptr_eq(&identity, &thread.proc_data.identity())
                    && !thread.signal.signal_blocked(signo)
                {
                    // A process-directed signal may be delivered to any
                    // unblocked thread. Prefer the current thread for
                    // self-signals so `kill(getpid(), SIGSTOP)` cannot return
                    // to userspace and race into the next syscall before this
                    // thread observes the stop.
                    send_signal_to_task(&curr, None, Some(sig))?;
                } else {
                    signal_user_process(&identity, Some(sig))?;
                }
            } else {
                signal_user_process(&identity, None)?;
            }
        }
        KillTarget::CurrentProcessGroup => {
            let pgid = current().as_thread().proc_data.proc.group().pgid_number();
            kill_process_group_checked(pgid, sig)?;
        }
        KillTarget::AllPermittedProcesses => {
            // Broadcast: send to all processes the caller may signal,
            // except init and self. EPERM is silently swallowed per Linux.
            let curr_pid = current().as_thread().proc_data.proc.pid();
            if let Some(sig) = sig {
                for proc_data in processes() {
                    if proc_data.proc.is_init() || proc_data.proc.pid() == curr_pid {
                        continue;
                    }
                    if check_kill_permission_identity(&proc_data.identity()).is_ok() {
                        let _ = send_signal_to_process_data(&proc_data, Some(sig.clone()));
                    }
                }
            }
        }
        KillTarget::ProcessGroup(pgid) => {
            kill_process_group_checked(pgid, sig)?;
        }
    }
    Ok(0)
}

pub fn sys_tkill(tid: i32, signo: u32) -> StarryResult<isize> {
    if tid <= 0 {
        return Err(StarryError::InvalidInput);
    }
    let tid = TidNumber::try_from(tid as u32)?;
    let task = get_user_task_by_number(tid)?;
    check_kill_permission_identity(&task.as_thread().proc_data.identity())?;
    let sig = make_siginfo(signo, SI_TKILL)?;
    send_signal_to_task(&task, None, sig)?;
    Ok(0)
}

pub fn sys_tgkill(tgid: i32, tid: i32, signo: u32) -> StarryResult<isize> {
    if tgid <= 0 || tid <= 0 {
        return Err(StarryError::InvalidInput);
    }
    let process = current_pid_view().resolve_process(TgidNumber::try_from(tgid as u32)?)?;
    check_kill_permission_identity(&process)?;
    let task = get_user_task_by_number(TidNumber::try_from(tid as u32)?)?;
    let sig = make_siginfo(signo, SI_TKILL)?;
    send_signal_to_task(&task, Some(process), sig)?;
    Ok(0)
}

pub(crate) fn make_queue_signal_info(
    tgid: TgidNumber,
    signo: u32,
    sig: *const SignalInfo,
) -> StarryResult<Option<SignalInfo>> {
    if signo == 0 {
        return Ok(None);
    }

    let signo = parse_signo(signo)?;
    let mut sig = unsafe { sig.vm_read_uninit()?.assume_init() };
    sig.set_signo(signo);
    if !Arc::ptr_eq(
        &current().as_thread().proc_data.identity(),
        &current_pid_view().resolve_process(tgid)?,
    ) && (sig.code() >= 0 || sig.code() == SI_TKILL)
    {
        return Err(StarryError::OperationNotPermitted);
    }
    Ok(Some(sig))
}

pub fn sys_rt_sigqueueinfo(
    tgid: u32,
    signo: u32,
    sig: *const SignalInfo,
    sigsetsize: usize,
) -> StarryResult<isize> {
    check_sigset_size(sigsetsize)?;

    let tgid = TgidNumber::try_from(tgid)?;
    let sig = make_queue_signal_info(tgid, signo, sig)?;
    let process = current_pid_view().resolve_process(tgid)?;
    signal_user_process(&process, sig)?;
    Ok(0)
}

pub fn sys_rt_tgsigqueueinfo(
    tgid: u32,
    tid: u32,
    signo: u32,
    sig: *const SignalInfo,
    sigsetsize: usize,
) -> StarryResult<isize> {
    check_sigset_size(sigsetsize)?;

    let tgid = TgidNumber::try_from(tgid)?;
    let sig = make_queue_signal_info(tgid, signo, sig)?;
    let process = current_pid_view().resolve_process(tgid)?;
    let task = get_user_task_by_number(TidNumber::try_from(tid)?)?;
    send_signal_to_task(&task, Some(process), sig)?;
    Ok(0)
}

pub fn sys_rt_sigreturn(uctx: &mut UserContext) -> StarryResult<isize> {
    block_next_signal();
    current().as_thread().signal.restore(uctx)?;
    Ok(uctx.retval() as isize)
}

pub fn sys_rt_sigtimedwait(
    uctx: &mut UserContext,
    set: *const SignalSet,
    info: *mut siginfo,
    timeout: *const timespec,
    sigsetsize: usize,
) -> StarryResult<isize> {
    check_sigset_size(sigsetsize)?;

    let set = unsafe { set.vm_read_uninit()?.assume_init() };

    let timeout = if let Some(ts) = timeout.nullable() {
        let ts = unsafe { ts.vm_read_uninit()?.assume_init() };
        Some(ts.try_into_time_value()?)
    } else {
        None
    };

    debug!("sys_rt_sigtimedwait => set = {set:?}, timeout = {timeout:?}");

    let curr = current();
    let thr = curr.as_thread();
    let signal = &thr.signal;

    let old_blocked = signal.blocked();
    // Publish sigwait_set so that send_signal skips is_ignore() for signals
    // this thread is waiting for.  We do NOT unblock the waited signals:
    // dequeue_signal(&set) can already retrieve blocked pending signals, and
    // keeping them blocked prevents check_signals from racing to dequeue and
    // discard them as default-ignore (e.g. SIGCHLD/SIGURG).
    *signal.sigwait_set.lock() = Some(set);

    uctx.set_retval(-Errno::EINTR.into_raw() as usize);
    let fut = poll_fn(|cx| {
        if let Some(sig) = signal.dequeue_signal(&set) {
            Poll::Ready(Some(sig))
        } else if check_signals(thr, uctx, Some(old_blocked), None) {
            Poll::Ready(None)
        } else {
            let _ = curr.poll_interrupt(cx);
            Poll::Pending
        }
    });

    let Ok(sig) = block_on(future::timeout(timeout, fut)) else {
        // Timeout
        *signal.sigwait_set.lock() = None;
        return Err(StarryError::WouldBlock);
    };
    let Some(sig) = sig else {
        // Interrupted
        *signal.sigwait_set.lock() = None;
        return Ok(0);
    };

    *signal.sigwait_set.lock() = None;

    if let Some(info) = info.nullable() {
        info.vm_write(sig.0)?;
    }

    Ok(sig.signo() as _)
}

pub fn sys_rt_sigsuspend(
    uctx: &mut UserContext,
    set: *const SignalSet,
    sigsetsize: usize,
) -> StarryResult<isize> {
    check_sigset_size(sigsetsize)?;

    let curr = current();
    let thr = curr.as_thread();

    let set = unsafe { set.vm_read_uninit()?.assume_init() };
    let old_blocked = thr.signal.set_blocked(set);

    // sigsuspend always returns -EINTR when a signal is caught
    // We set this in uctx before check_signals so it's saved in SignalFrame
    uctx.set_retval(-Errno::EINTR.into_raw() as usize);

    block_on(poll_fn(|cx| {
        if check_signals(thr, uctx, Some(old_blocked), None) {
            return Poll::Ready(());
        }
        let _ = curr.poll_interrupt(cx);
        Poll::Pending
    }));

    // sigsuspend always returns -EINTR
    Err(StarryError::Interrupted)
}

pub fn sys_sigaltstack(ss: *const SignalStack, old_ss: *mut SignalStack) -> StarryResult<isize> {
    let curr = current();
    let sig = &curr.as_thread().signal;

    if let Some(old_ss) = old_ss.nullable() {
        old_ss.vm_write(sig.stack())?;
    }

    if let Some(ss) = ss.nullable() {
        let ss = unsafe { ss.vm_read_uninit()?.assume_init() };
        if sig.stack_active() {
            return Err(StarryError::OperationNotPermitted);
        }
        if ss.flags & !(SS_DISABLE | SS_ONSTACK | SS_FLAG_BITS) != 0 {
            return Err(StarryError::InvalidInput);
        }
        if ss.flags & SS_DISABLE == 0 && ss.size < MINSIGSTKSZ as usize {
            return Err(StarryError::NoMemory);
        }
        sig.set_stack(ss);
    }
    Ok(0)
}

#[cfg(all(test, not(axtest)))]
fn signal_sigset_size_and_signo_validation_rules_hold_for_test() -> bool {
    use core::mem::size_of;

    use starry_signal::SignalSet;

    // check_sigset_size: only accepts exact size of SignalSet.
    let correct_size = size_of::<SignalSet>();
    let ok = check_sigset_size(correct_size).is_ok();
    let too_small = check_sigset_size(correct_size - 1).is_err();
    let too_big = check_sigset_size(correct_size + 1).is_err();
    let zero = check_sigset_size(0).is_err();

    // parse_signo: valid signos (1-31 typically) parse, 0 and out-of-range fail.
    // SIGKILL=9, SIGSTOP=19 on Linux x86_64.
    let valid_signo = parse_signo(9).is_ok(); // SIGKILL
    let valid_signo2 = parse_signo(19).is_ok(); // SIGSTOP
    let zero_signo = parse_signo(0).is_err(); // 0 is not a valid signo
    // Signo::from_repr uses u8, so values > 255 fail
    let overflow = parse_signo(256).is_err();

    ok && too_small && too_big && zero && valid_signo && valid_signo2 && zero_signo && overflow
}

#[cfg(all(test, not(axtest)))]
fn signal_sigset_and_signo_validation_rules_hold_for_test() -> bool {
    use core::mem::size_of;

    use starry_signal::SignalSet;

    // Test check_sigset_size
    let correct_size = size_of::<SignalSet>();
    assert!(check_sigset_size(correct_size).is_ok());
    assert!(check_sigset_size(correct_size - 1).is_err());
    assert!(check_sigset_size(correct_size + 1).is_err());
    assert!(check_sigset_size(0).is_err());

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
    fn signal_sigset_size_and_signo_validation_rules_hold() {
        assert!(super::signal_sigset_size_and_signo_validation_rules_hold_for_test());
    }

    #[test]
    fn signal_sigset_and_signo_validation_rules_hold() {
        assert!(super::signal_sigset_and_signo_validation_rules_hold_for_test());
    }
}
