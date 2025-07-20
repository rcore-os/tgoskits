use core::{mem, sync::atomic::Ordering};

use axerrno::{LinuxError, LinuxResult};
use axhal::context::TrapFrame;
use axprocess::Pid;
use axsignal::{SignalInfo, SignalSet, SignalStack, Signo};
use axtask::{
    current,
    future::{self, block_on},
};
use linux_raw_sys::general::{
    MINSIGSTKSZ, SI_TKILL, SI_USER, SIG_BLOCK, SIG_SETMASK, SIG_UNBLOCK, kernel_sigaction, siginfo,
    timespec,
};
use starry_core::task::{AsThread, processes};

use crate::{
    mm::{UserConstPtr, UserPtr, nullable},
    signal::{
        BLOCK_NEXT_SIGNAL_CHECK, check_signals, send_signal_to_process,
        send_signal_to_process_group, send_signal_to_thread,
    },
    time::TimeValueLike,
};

fn check_sigset_size(size: usize) -> LinuxResult<()> {
    if size != size_of::<SignalSet>() {
        return Err(LinuxError::EINVAL);
    }
    Ok(())
}

fn parse_signo(signo: u32) -> LinuxResult<Signo> {
    Signo::from_repr(signo as u8).ok_or(LinuxError::EINVAL)
}

pub fn sys_rt_sigprocmask(
    how: i32,
    set: UserConstPtr<SignalSet>,
    oldset: UserPtr<SignalSet>,
    sigsetsize: usize,
) -> LinuxResult<isize> {
    check_sigset_size(sigsetsize)?;

    let oldset = nullable!(oldset.get_as_mut())?;
    let set = nullable!(set.get_as_ref())?;

    current()
        .as_thread()
        .signal
        .with_blocked_mut::<LinuxResult<_>>(|blocked| {
            if let Some(oldset) = oldset {
                *oldset = *blocked;
            }

            if let Some(set) = set {
                match how as u32 {
                    SIG_BLOCK => *blocked |= *set,
                    SIG_UNBLOCK => *blocked &= !*set,
                    SIG_SETMASK => *blocked = *set,
                    _ => return Err(LinuxError::EINVAL),
                }
            }
            Ok(())
        })?;

    Ok(0)
}

pub fn sys_rt_sigaction(
    signo: u32,
    act: UserConstPtr<kernel_sigaction>,
    oldact: UserPtr<kernel_sigaction>,
    sigsetsize: usize,
) -> LinuxResult<isize> {
    check_sigset_size(sigsetsize)?;

    let signo = parse_signo(signo)?;
    if matches!(signo, Signo::SIGKILL | Signo::SIGSTOP) {
        return Err(LinuxError::EINVAL);
    }

    let curr = current();
    let mut actions = curr.as_thread().proc_data.signal.actions.lock();
    if let Some(oldact) = nullable!(oldact.get_as_mut())? {
        actions[signo].to_ctype(oldact);
    }
    if let Some(act) = nullable!(act.get_as_ref())? {
        actions[signo] = (*act).into();
    }
    Ok(0)
}

pub fn sys_rt_sigpending(set: UserPtr<SignalSet>, sigsetsize: usize) -> LinuxResult<isize> {
    check_sigset_size(sigsetsize)?;
    *set.get_as_mut()? = current().as_thread().proc_data.signal.pending();
    Ok(0)
}

fn make_siginfo(signo: u32, code: i32) -> LinuxResult<Option<SignalInfo>> {
    if signo == 0 {
        return Ok(None);
    }
    let signo = parse_signo(signo)?;
    Ok(Some(SignalInfo::new(signo, code)))
}

pub fn sys_kill(pid: i32, signo: u32) -> LinuxResult<isize> {
    debug!("sys_kill: pid = {}, signo = {}", pid, signo);
    let sig = make_siginfo(signo, SI_USER as _)?;

    match pid {
        1.. => {
            send_signal_to_process(pid as _, sig)?;
        }
        0 => {
            let pgid = current().as_thread().proc_data.proc.group().pgid();
            send_signal_to_process_group(pgid, sig)?;
        }
        -1 => {
            if let Some(sig) = sig {
                for proc_data in processes() {
                    if proc_data.proc.is_init() {
                        // init process
                        continue;
                    }
                    let _ = send_signal_to_process(proc_data.proc.pid(), Some(sig.clone()));
                }
            }
        }
        ..-1 => {
            send_signal_to_process_group((-pid) as Pid, sig)?;
        }
    }
    Ok(0)
}

pub fn sys_tkill(tid: Pid, signo: u32) -> LinuxResult<isize> {
    let sig = make_siginfo(signo, SI_TKILL)?;
    send_signal_to_thread(None, tid, sig)?;
    Ok(0)
}

pub fn sys_tgkill(tgid: Pid, tid: Pid, signo: u32) -> LinuxResult<isize> {
    let sig = make_siginfo(signo, SI_TKILL)?;
    send_signal_to_thread(Some(tgid), tid, sig)?;
    Ok(0)
}

fn make_queue_signal_info(
    tgid: Pid,
    signo: u32,
    sig: UserConstPtr<SignalInfo>,
) -> LinuxResult<Option<SignalInfo>> {
    if signo == 0 {
        return Ok(None);
    }

    let signo = parse_signo(signo)?;
    let mut sig = sig.get_as_ref()?.clone();
    sig.set_signo(signo);
    if current().as_thread().proc_data.proc.pid() != tgid
        && (sig.code() >= 0 || sig.code() == SI_TKILL)
    {
        return Err(LinuxError::EPERM);
    }
    Ok(Some(sig))
}

pub fn sys_rt_sigqueueinfo(
    tgid: Pid,
    signo: u32,
    sig: UserConstPtr<SignalInfo>,
    sigsetsize: usize,
) -> LinuxResult<isize> {
    check_sigset_size(sigsetsize)?;

    let sig = make_queue_signal_info(tgid, signo, sig)?;
    send_signal_to_process(tgid, sig)?;
    Ok(0)
}

pub fn sys_rt_tgsigqueueinfo(
    tgid: Pid,
    tid: Pid,
    signo: u32,
    sig: UserConstPtr<SignalInfo>,
    sigsetsize: usize,
) -> LinuxResult<isize> {
    check_sigset_size(sigsetsize)?;

    let sig = make_queue_signal_info(tgid, signo, sig)?;
    send_signal_to_thread(Some(tgid), tid, sig)?;
    Ok(0)
}

pub fn sys_rt_sigreturn(tf: &mut TrapFrame) -> LinuxResult<isize> {
    BLOCK_NEXT_SIGNAL_CHECK.store(true, Ordering::SeqCst);
    current().as_thread().signal.restore(tf);
    Ok(tf.retval() as isize)
}

pub fn sys_rt_sigtimedwait(
    set: UserConstPtr<SignalSet>,
    info: UserPtr<siginfo>,
    timeout: UserConstPtr<timespec>,
    sigsetsize: usize,
) -> LinuxResult<isize> {
    check_sigset_size(sigsetsize)?;

    let set = *set.get_as_ref()?;
    let timeout = nullable!(timeout.get_as_ref())?
        .map(|ts| ts.try_into_time_value())
        .transpose()?;

    debug!(
        "sys_rt_sigtimedwait => set = {:?}, timeout = {:?}",
        set, timeout
    );

    let curr = current();
    let fut = curr.as_thread().signal.wait(set);

    let sig = block_on(async {
        if let Some(timeout) = timeout {
            future::timeout(fut, timeout)
                .await
                .ok_or(LinuxError::EAGAIN)
        } else {
            Ok(fut.await)
        }
    })?;

    if let Some(info) = nullable!(info.get_as_mut())? {
        *info = sig.0;
    }

    Ok(0)
}

pub fn sys_rt_sigsuspend(
    tf: &mut TrapFrame,
    set: UserConstPtr<SignalSet>,
    sigsetsize: usize,
) -> LinuxResult<isize> {
    check_sigset_size(sigsetsize)?;

    let curr = current();
    let thr = curr.as_thread();
    let mut set = *set.get_as_ref()?;

    set.remove(Signo::SIGKILL);
    set.remove(Signo::SIGSTOP);

    let old_blocked = thr
        .signal
        .with_blocked_mut(|blocked| mem::replace(blocked, set));

    tf.set_retval(-LinuxError::EINTR.code() as usize);

    block_on(async move {
        loop {
            if check_signals(tf, Some(old_blocked)) {
                return;
            }
            thr.proc_data.signal.wait().await;
        }
    });

    Ok(0)
}

pub fn sys_sigaltstack(
    ss: UserConstPtr<SignalStack>,
    old_ss: UserPtr<SignalStack>,
) -> LinuxResult<isize> {
    current().as_thread().signal.with_stack_mut(|stack| {
        if let Some(old_ss) = nullable!(old_ss.get_as_mut())? {
            *old_ss = stack.clone();
        }
        if let Some(ss) = nullable!(ss.get_as_ref())? {
            if ss.size <= MINSIGSTKSZ as usize {
                return Err(LinuxError::ENOMEM);
            }
            let stack_ptr: UserConstPtr<u8> = ss.sp.into();
            let _ = stack_ptr.get_as_slice(ss.size)?;

            *stack = ss.clone();
        }
        Ok(0)
    })
}
