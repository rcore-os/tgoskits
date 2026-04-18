use core::sync::atomic::{AtomicBool, Ordering};

use ax_errno::{AxError, AxResult};
use ax_hal::uspace::UserContext;
use ax_task::{TaskInner, current};
use starry_process::Pid;
use starry_signal::{SignalActionFlags, SignalInfo, SignalOSAction, SignalSet};

use super::{
    AsThread, SYSCALL_INSN_LEN, Thread, do_exit, get_process_data, get_process_group, get_task,
};

/// Information needed to restart a syscall if SA_RESTART applies.
pub struct SyscallRestartInfo {
    /// First argument register value before the syscall overwrote it.
    pub saved_a0: usize,
    /// Syscall number register value. On x86_64 rax holds both the
    /// syscall number and the return value, so restarting requires
    /// restoring it to the syscall number.
    pub saved_sysno: usize,
}

pub fn check_signals(
    thr: &Thread,
    uctx: &mut UserContext,
    restore_blocked: Option<SignalSet>,
    restart_info: Option<&SyscallRestartInfo>,
) -> bool {
    let blocked = thr.signal.blocked();
    let mask = !blocked;
    let restore_blocked = restore_blocked.unwrap_or(blocked);

    let Some(sig) = thr.signal.dequeue_signal(&mask) else {
        return false;
    };

    let signo = sig.signo();
    let mut actions = thr.signal.process().actions.lock();
    let action = actions[signo].clone();

    // Apply the SA_RESTART decision once per interrupted syscall. Callers
    // pass `Some(info)` only for the first signal delivered; for later
    // iterations they pass `None` so a second signal cannot reapply the
    // decision. When SA_RESTART is not set we leave retval at -EINTR so
    // handle_signal captures it into the signal frame and sigreturn
    // restores -EINTR to user space (non-restart semantics).
    if let Some(info) = restart_info
        && (uctx.retval() as isize) == -(ax_errno::LinuxError::EINTR.code() as isize)
        && action.flags.contains(SignalActionFlags::RESTART)
    {
        uctx.set_ip(uctx.ip() - SYSCALL_INSN_LEN);
        uctx.set_arg0(info.saved_a0);
        // On x86_64, rax holds both the syscall number and the return
        // value, so the syscall entry path clobbered sysno with -EINTR.
        // Restore it before the syscall instruction re-executes. On
        // RISC-V/AArch64/LoongArch64 sysno lives in a separate register
        // (a7/x8/a7) that was not touched, so no restore is needed.
        #[cfg(target_arch = "x86_64")]
        uctx.set_sysno(info.saved_sysno);
        #[cfg(not(target_arch = "x86_64"))]
        let _ = info.saved_sysno;
    }

    let Some(os_action) =
        thr.signal
            .handle_signal(uctx, restore_blocked, &sig, &action, &mut actions)
    else {
        return true;
    };

    match os_action {
        SignalOSAction::Terminate => do_exit(signo as i32, true),
        SignalOSAction::CoreDump => do_exit(128 + signo as i32, true),
        SignalOSAction::Stop => do_exit(1, true),
        SignalOSAction::Continue => {}
        SignalOSAction::Handler => {}
    }
    true
}

static BLOCK_NEXT_SIGNAL_CHECK: AtomicBool = AtomicBool::new(false);

pub fn block_next_signal() {
    BLOCK_NEXT_SIGNAL_CHECK.store(true, Ordering::SeqCst);
}

pub fn unblock_next_signal() -> bool {
    BLOCK_NEXT_SIGNAL_CHECK.swap(false, Ordering::SeqCst)
}

pub fn with_blocked_signals<R>(
    blocked: Option<SignalSet>,
    f: impl FnOnce() -> AxResult<R>,
) -> AxResult<R> {
    let curr = current();
    let sig = &curr.as_thread().signal;

    let old_blocked = blocked.map(|set| sig.set_blocked(set));
    f().inspect(|_| {
        if let Some(old) = old_blocked {
            sig.set_blocked(old);
        }
    })
}

pub(super) fn send_signal_thread_inner(task: &TaskInner, thr: &Thread, sig: SignalInfo) {
    if thr.signal.send_signal(sig) {
        task.interrupt();
    }
}

/// Sends a signal to a thread.
pub fn send_signal_to_thread(tgid: Option<Pid>, tid: Pid, sig: Option<SignalInfo>) -> AxResult<()> {
    let task = get_task(tid)?;
    let thread = task.try_as_thread().ok_or(AxError::OperationNotPermitted)?;
    if tgid.is_some_and(|tgid| thread.proc_data.proc.pid() != tgid) {
        return Err(AxError::NoSuchProcess);
    }

    if let Some(sig) = sig {
        info!("Send signal {:?} to thread {}", sig.signo(), tid);
        send_signal_thread_inner(&task, thread, sig);
    }

    Ok(())
}

/// Sends a signal to a process.
pub fn send_signal_to_process(pid: Pid, sig: Option<SignalInfo>) -> AxResult<()> {
    let proc_data = get_process_data(pid)?;

    if let Some(sig) = sig {
        let signo = sig.signo();
        info!("Send signal {signo:?} to process {pid}");
        if let Some(tid) = proc_data.signal.send_signal(sig)
            && let Ok(task) = get_task(tid)
        {
            task.interrupt();
        }
    }

    Ok(())
}

/// Sends a signal to a process group.
pub fn send_signal_to_process_group(pgid: Pid, sig: Option<SignalInfo>) -> AxResult<()> {
    let pg = get_process_group(pgid)?;

    if let Some(sig) = sig {
        info!("Send signal {:?} to process group {}", sig.signo(), pgid);
        for proc in pg.processes() {
            send_signal_to_process(proc.pid(), Some(sig.clone()))?;
        }
    }

    Ok(())
}

/// Sends a fatal signal to the current process.
pub fn raise_signal_fatal(sig: SignalInfo) -> AxResult<()> {
    let curr = current();
    let proc_data = &curr.as_thread().proc_data;

    let signo = sig.signo();
    info!("Send fatal signal {signo:?} to the current process");
    if let Some(tid) = proc_data.signal.send_signal(sig)
        && let Ok(task) = get_task(tid)
    {
        task.interrupt();
    } else {
        // No task wants to handle the signal, abort the task
        do_exit(signo as i32, true);
    }

    Ok(())
}
