use core::{future::poll_fn, task::Poll};

use axerrno::{AxError, AxResult, LinuxError};
use axhal::uspace::UserContext;
use axtask::{
    current,
    future::{self, block_on},
};
use linux_raw_sys::general::{
    MINSIGSTKSZ, SI_TKILL, SI_USER, SIG_BLOCK, SIG_SETMASK, SIG_UNBLOCK, kernel_sigaction, siginfo,
    timespec,
};
use starry_core::task::{
    AsThread, processes, send_signal_to_process, send_signal_to_process_group,
    send_signal_to_thread,
};
use starry_process::Pid;
use starry_signal::{SignalInfo, SignalSet, SignalStack, Signo};
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    signal::{block_next_signal, check_signals},
    time::TimeValueLike,
};

pub(crate) fn check_sigset_size(size: usize) -> AxResult<()> {
    if size != size_of::<SignalSet>() && size != 0 {
        return Err(AxError::InvalidInput);
    }
    Ok(())
}

fn parse_signo(signo: u32) -> AxResult<Signo> {
    Signo::from_repr(signo as u8).ok_or(AxError::InvalidInput)
}

pub fn sys_rt_sigprocmask(
    how: i32,
    set: *const SignalSet,
    oldset: *mut SignalSet,
    sigsetsize: usize,
) -> AxResult<isize> {
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
            _ => return Err(AxError::InvalidInput),
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
) -> AxResult<isize> {
    check_sigset_size(sigsetsize)?;

    let signo = parse_signo(signo)?;
    if matches!(signo, Signo::SIGKILL | Signo::SIGSTOP) {
        return Err(AxError::InvalidInput);
    }

    let curr = current();
    let mut actions = curr.as_thread().proc_data.signal.actions.lock();
    if let Some(oldact) = oldact.nullable() {
        oldact.vm_write(actions[signo].clone().into())?;
    }
    if let Some(act) = act.nullable() {
        let act = unsafe { act.vm_read_uninit()?.assume_init() }.into();
        debug!("sys_rt_sigaction <= signo: {signo:?}, act: {act:?}");
        actions[signo] = act;
    }
    Ok(0)
}

pub fn sys_rt_sigpending(set: *mut SignalSet, sigsetsize: usize) -> AxResult<isize> {
    check_sigset_size(sigsetsize)?;
    set.vm_write(current().as_thread().signal.pending())?;
    Ok(0)
}

fn make_siginfo(signo: u32, code: i32) -> AxResult<Option<SignalInfo>> {
    if signo == 0 {
        return Ok(None);
    }
    let signo = parse_signo(signo)?;
    Ok(Some(SignalInfo::new_user(
        signo,
        code,
        current().as_thread().proc_data.proc.pid(),
    )))
}

pub fn sys_kill(pid: i32, signo: u32) -> AxResult<isize> {
    debug!("sys_kill: pid = {pid}, signo = {signo}");
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
            let curr_pid = current().as_thread().proc_data.proc.pid();
            if let Some(sig) = sig {
                for proc_data in processes() {
                    // POSIX.1 requires that kill(-1,sig) send sig to all processes that
                    //    the calling process may send signals to, except possibly for some
                    //    implementation-defined system processes.  Linux allows a process
                    //    to signal itself, but on Linux the call kill(-1,sig) does not
                    //    signal the calling process.
                    if proc_data.proc.is_init() || proc_data.proc.pid() == curr_pid {
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

pub fn sys_tkill(tid: Pid, signo: u32) -> AxResult<isize> {
    let sig = make_siginfo(signo, SI_TKILL)?;
    send_signal_to_thread(None, tid, sig)?;
    Ok(0)
}

pub fn sys_tgkill(tgid: Pid, tid: Pid, signo: u32) -> AxResult<isize> {
    let sig = make_siginfo(signo, SI_TKILL)?;
    send_signal_to_thread(Some(tgid), tid, sig)?;
    Ok(0)
}

pub(crate) fn make_queue_signal_info(
    tgid: Pid,
    signo: u32,
    sig: *const SignalInfo,
) -> AxResult<Option<SignalInfo>> {
    if signo == 0 {
        return Ok(None);
    }

    let signo = parse_signo(signo)?;
    let mut sig = unsafe { sig.vm_read_uninit()?.assume_init() };
    sig.set_signo(signo);
    if current().as_thread().proc_data.proc.pid() != tgid
        && (sig.code() >= 0 || sig.code() == SI_TKILL)
    {
        return Err(AxError::OperationNotPermitted);
    }
    Ok(Some(sig))
}

pub fn sys_rt_sigqueueinfo(
    tgid: Pid,
    signo: u32,
    sig: *const SignalInfo,
    sigsetsize: usize,
) -> AxResult<isize> {
    check_sigset_size(sigsetsize)?;

    let sig = make_queue_signal_info(tgid, signo, sig)?;
    send_signal_to_process(tgid, sig)?;
    Ok(0)
}

pub fn sys_rt_tgsigqueueinfo(
    tgid: Pid,
    tid: Pid,
    signo: u32,
    sig: *const SignalInfo,
    sigsetsize: usize,
) -> AxResult<isize> {
    check_sigset_size(sigsetsize)?;

    let sig = make_queue_signal_info(tgid, signo, sig)?;
    send_signal_to_thread(Some(tgid), tid, sig)?;
    Ok(0)
}

pub fn sys_rt_sigreturn(uctx: &mut UserContext) -> AxResult<isize> {
    block_next_signal();
    current().as_thread().signal.restore(uctx);
    Ok(uctx.retval() as isize)
}

pub fn sys_rt_sigtimedwait(
    uctx: &mut UserContext,
    set: *const SignalSet,
    info: *mut siginfo,
    timeout: *const timespec,
    sigsetsize: usize,
) -> AxResult<isize> {
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
    signal.set_blocked(old_blocked & !set);

    uctx.set_retval(-LinuxError::EINTR.code() as usize);
    let fut = poll_fn(|context| {
        if let Some(sig) = signal.dequeue_signal(&set) {
            signal.set_blocked(old_blocked);
            Poll::Ready(Some(sig))
        } else if check_signals(thr, uctx, Some(old_blocked)) {
            Poll::Ready(None)
        } else {
            curr.on_interrupt(context.waker());
            Poll::Pending
        }
    });

    let Ok(sig) = block_on(future::timeout(timeout, fut)) else {
        // Timeout
        signal.set_blocked(old_blocked);
        return Err(AxError::WouldBlock);
    };
    let Some(sig) = sig else {
        // Interrupted
        return Ok(0);
    };

    if let Some(info) = info.nullable() {
        info.vm_write(sig.0)?;
    }

    Ok(sig.signo() as _)
}

pub fn sys_rt_sigsuspend(
    uctx: &mut UserContext,
    set: *const SignalSet,
    sigsetsize: usize,
) -> AxResult<isize> {
    check_sigset_size(sigsetsize)?;

    let curr = current();
    let thr = curr.as_thread();

    let set = unsafe { set.vm_read_uninit()?.assume_init() };
    let old_blocked = thr.signal.set_blocked(set);

    uctx.set_retval(-LinuxError::EINTR.code() as usize);

    block_on(poll_fn(|context| {
        if check_signals(thr, uctx, Some(old_blocked)) {
            return Poll::Ready(());
        }
        curr.on_interrupt(context.waker());
        Poll::Pending
    }));

    Ok(0)
}

pub fn sys_sigaltstack(ss: *const SignalStack, old_ss: *mut SignalStack) -> AxResult<isize> {
    let curr = current();
    let sig = &curr.as_thread().signal;

    if let Some(old_ss) = old_ss.nullable() {
        old_ss.vm_write(sig.stack())?;
    }

    if let Some(ss) = ss.nullable() {
        let ss = unsafe { ss.vm_read_uninit()?.assume_init() };
        if ss.size <= MINSIGSTKSZ as usize {
            return Err(AxError::NoMemory);
        }
        sig.set_stack(ss);
    }
    Ok(0)
}
