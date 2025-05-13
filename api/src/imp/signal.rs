use core::{mem, time::Duration};

use alloc::sync::Arc;
use axerrno::{LinuxError, LinuxResult};
use axhal::arch::TrapFrame;
use axprocess::{Pid, Thread};
use axsignal::{SignalInfo, SignalSet, SignalStack, Signo};
use axtask::{TaskExtRef, current};
use linux_raw_sys::general::{
    MINSIGSTKSZ, SI_TKILL, SI_USER, SIG_BLOCK, SIG_SETMASK, SIG_UNBLOCK, kernel_sigaction, siginfo,
    timespec,
};
use starry_core::task::{get_process, get_process_group, get_thread, processes};

use crate::{
    ptr::{UserConstPtr, UserPtr, nullable},
    signal::{check_signals, send_signal_process, send_signal_process_group, send_signal_thread},
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

    current()
        .task_ext()
        .thread_data()
        .signal
        .with_blocked_mut::<LinuxResult<_>>(|blocked| {
            if let Some(oldset) = nullable!(oldset.get_as_mut())? {
                *oldset = *blocked;
            }

            if let Some(set) = nullable!(set.get_as_ref())? {
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
    let mut actions = curr.task_ext().process_data().signal.actions.lock();
    if let Some(oldact) = nullable!(oldact.get_as_mut())? {
        actions[signo].to_ctype(oldact);
    }
    if let Some(act) = nullable!(act.get_as_ref())? {
        actions[signo] = (*act).try_into()?;
    }
    Ok(0)
}

pub fn sys_rt_sigpending(set: UserPtr<SignalSet>, sigsetsize: usize) -> LinuxResult<isize> {
    check_sigset_size(sigsetsize)?;
    *set.get_as_mut()? = current().task_ext().thread_data().signal.pending();
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
    let Some(sig) = make_siginfo(signo, SI_USER as _)? else {
        // TODO: should also check permissions
        return Ok(0);
    };

    let curr = current();
    match pid {
        1.. => {
            let proc = get_process(pid as Pid)?;
            send_signal_process(&proc, sig)?;
            Ok(1)
        }
        0 => {
            let pg = curr.task_ext().thread.process().group();
            Ok(send_signal_process_group(&pg, sig) as _)
        }
        -1 => {
            let mut count = 0;
            for proc in processes() {
                if proc.is_init() {
                    // init process
                    continue;
                }
                send_signal_process(&proc, sig.clone())?;
                count += 1;
            }
            Ok(count)
        }
        ..-1 => {
            let pg = get_process_group((-pid) as Pid)?;
            Ok(send_signal_process_group(&pg, sig) as _)
        }
    }
}

pub fn sys_tkill(tid: Pid, signo: u32) -> LinuxResult<isize> {
    let Some(sig) = make_siginfo(signo, SI_TKILL)? else {
        // TODO: should also check permissions
        return Ok(0);
    };

    let thr = get_thread(tid)?;
    send_signal_thread(&thr, sig)?;
    Ok(0)
}

pub fn sys_tgkill(tgid: Pid, tid: Pid, signo: u32) -> LinuxResult<isize> {
    let Some(sig) = make_siginfo(signo, SI_TKILL)? else {
        // TODO: should also check permissions
        return Ok(0);
    };

    send_signal_thread(find_thread_in_group(tgid, tid)?.as_ref(), sig)?;
    Ok(0)
}

fn find_thread_in_group(tgid: Pid, tid: Pid) -> LinuxResult<Arc<Thread>> {
    let thr = get_thread(tid)?;
    if thr.process().pid() != tgid {
        return Err(LinuxError::ESRCH);
    }
    Ok(thr)
}

fn make_queue_signal_info(
    tgid: Pid,
    signo: u32,
    sig: UserConstPtr<SignalInfo>,
) -> LinuxResult<SignalInfo> {
    let signo = parse_signo(signo)?;
    let mut sig = sig.get_as_ref()?.clone();
    sig.set_signo(signo);
    if current().task_ext().thread.process().pid() != tgid
        && (sig.code() >= 0 || sig.code() == SI_TKILL)
    {
        return Err(LinuxError::EPERM);
    }
    Ok(sig)
}

pub fn sys_rt_sigqueueinfo(
    tgid: Pid,
    signo: u32,
    sig: UserConstPtr<SignalInfo>,
    sigsetsize: usize,
) -> LinuxResult<isize> {
    check_sigset_size(sigsetsize)?;

    let sig = make_queue_signal_info(tgid, signo, sig)?;
    send_signal_process(get_process(tgid)?.as_ref(), sig)?;
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
    send_signal_thread(find_thread_in_group(tgid, tid)?.as_ref(), sig)?;
    Ok(0)
}

pub fn sys_rt_sigreturn(tf: &mut TrapFrame) -> LinuxResult<isize> {
    let curr = current();
    curr.task_ext().thread_data().signal.restore(tf);
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
    let timeout: Option<Duration> = nullable!(timeout.get_as_ref())?.map(|ts| ts.to_time_value());

    let Some(sig) = current()
        .task_ext()
        .thread_data()
        .signal
        .wait_timeout(set, timeout)
    else {
        return Err(LinuxError::EAGAIN);
    };

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
    let thr_data = curr.task_ext().thread_data();
    let mut set = *set.get_as_ref()?;

    set.remove(Signo::SIGKILL);
    set.remove(Signo::SIGSTOP);

    let old_blocked = thr_data
        .signal
        .with_blocked_mut(|blocked| mem::replace(blocked, set));

    tf.set_retval(-LinuxError::EINTR.code() as usize);

    loop {
        if check_signals(tf, Some(old_blocked)) {
            break;
        }
        curr.task_ext().process_data().signal.wait_signal();
    }

    Ok(0)
}

pub fn sys_sigaltstack(
    ss: UserConstPtr<SignalStack>,
    old_ss: UserPtr<SignalStack>,
) -> LinuxResult<isize> {
    current()
        .task_ext()
        .thread_data()
        .signal
        .with_stack_mut(|stack| {
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
