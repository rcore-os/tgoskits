use ax_errno::{AxError, AxResult};
use ax_hal::uspace::UserContext;
use ax_task::{TaskInner, current};
use starry_process::Pid;
use starry_signal::{SignalInfo, SignalOSAction, SignalSet};

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

/// Dump user-mode register state once the signal disposition really terminates.
fn dump_user_crash_context(uctx: &UserContext) {
    #[cfg(target_arch = "riscv64")]
    {
        let r = &uctx.regs;
        warn!(
            "user register dump:\n  pc(sepc)={:#018x} ra={:#018x} sp={:#018x}\n  gp={:#018x}  \
             tp={:#018x}  s0={:#018x}\n  a0={:#018x} a1={:#018x} a2={:#018x} a3={:#018x}\n  \
             a4={:#018x} a5={:#018x} a6={:#018x} a7={:#018x}",
            uctx.sepc, r.ra, r.sp, r.gp, r.tp, r.s0, r.a0, r.a1, r.a2, r.a3, r.a4, r.a5, r.a6, r.a7,
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        warn!(
            "user register dump:\n  pc(elr)={:#018x} spsr={:#018x}\n  x0={:#018x} x1={:#018x} \
             x2={:#018x} x3={:#018x}\n  x29(fp)={:#018x} x30(lr)={:#018x}",
            uctx.elr, uctx.spsr, uctx.x[0], uctx.x[1], uctx.x[2], uctx.x[3], uctx.x[29], uctx.x[30],
        );
    }
    #[cfg(target_arch = "x86_64")]
    {
        warn!(
            "user register dump:\n  rip={:#018x} rsp={:#018x} rflags={:#018x}\n  rax={:#018x} \
             rdi={:#018x} rsi={:#018x} rdx={:#018x}",
            uctx.rip, uctx.rsp, uctx.rflags, uctx.rax, uctx.rdi, uctx.rsi, uctx.rdx,
        );
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let r = &uctx.regs;
        warn!(
            "user register dump:\n  era={:#018x} ra={:#018x} sp={:#018x} tp={:#018x}\n  \
             a0={:#018x} a1={:#018x} a2={:#018x} a3={:#018x}",
            uctx.era, r.ra, r.sp, r.tp, r.a0, r.a1, r.a2, r.a3,
        );
    }
    #[cfg(not(any(
        target_arch = "riscv64",
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64",
    )))]
    {
        warn!("user register dump: not implemented for this arch");
    }
}

pub fn check_signals(
    thr: &Thread,
    uctx: &mut UserContext,
    restore_blocked: Option<SignalSet>,
    restart_info: Option<&SyscallRestartInfo>,
) -> bool {
    let Some((sig, os_action)) =
        thr.signal
            .check_signals_with(uctx, restore_blocked, |uctx, _sig, restartable| {
                // Apply the SA_RESTART decision once per interrupted syscall.
                // Callers pass `Some(info)` only for the first delivered signal;
                // later iterations pass `None`, so the restart adjustment remains
                // single-shot.
                if let Some(info) = restart_info
                    && (uctx.retval() as isize) == -(ax_errno::LinuxError::EINTR.code() as isize)
                    && restartable
                {
                    let new_ip = uctx.ip() - SYSCALL_INSN_LEN;
                    uctx.set_ip(new_ip);
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
            })
    else {
        return false;
    };

    let signo = sig.signo();

    // Only dump register state when the terminating signal originated as a
    // synchronous fault delivered to the current thread. Group-exit SIGKILL
    // sent to peer threads (and any other externally-injected fatal
    // signal) leaves `fault_dump_pending` cleared, so peers terminate
    // silently and the dump remains the faulting thread's signal.
    let dump_on_terminate = thr
        .fault_dump_pending
        .swap(false, core::sync::atomic::Ordering::AcqRel);

    match os_action {
        SignalOSAction::Terminate => {
            if dump_on_terminate {
                dump_user_crash_context(uctx);
            }
            do_exit(signo as i32, true);
        }
        SignalOSAction::CoreDump => {
            if dump_on_terminate {
                dump_user_crash_context(uctx);
            }
            do_exit(128 + signo as i32, true);
        }
        SignalOSAction::Stop => do_exit(1, true),
        SignalOSAction::Continue => {}
        SignalOSAction::NoFurtherAction => {}
    }
    true
}

pub fn block_next_signal() {
    current().as_thread().block_next_signal_check();
}

pub fn unblock_next_signal() -> bool {
    current().as_thread().unblock_next_signal_check()
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

/// Deliver a fatal signal raised by a synchronous exception (page
/// fault, illegal instruction, divide-by-zero, etc.) on the current
/// thread. Linux's `force_sig_info` semantics: the signal is bound to
/// the faulting thread and cannot be masked, so the register dump
/// printed during termination always describes the thread that took
/// the exception rather than an arbitrary peer that happened to have
/// the signal unblocked.
///
/// Process-wide fatal signals (signals raised on someone else's
/// behalf) still go through [`send_signal_to_process`] and can land
/// on any unmasked thread.
pub fn raise_signal_fatal(sig: SignalInfo, uctx: &UserContext) -> AxResult<()> {
    let curr = current();
    let thread = curr.as_thread();
    let signo = sig.signo();
    info!(
        "Synchronous-exception fatal signal {:?} on tid={}",
        signo,
        thread.proc_data.proc.pid()
    );

    // Force-deliver to the faulting thread. Mirrors Linux's
    //   force_sig_info():
    //     - Reset SIG_IGN to SIG_DFL so the signal cannot be silently
    //       swallowed: a synchronous SIGSEGV/SIGILL/SIGBUS on an
    //       address the user-space program told us to ignore would
    //       otherwise loop on the same fault forever.
    //     - Clear the per-thread mask bit so a thread that blocked
    //       the signal still terminates on a sync fault.
    //     - Then enqueue normally. If the disposition was a user
    //       handler, it still gets to run; the bypass only flips
    //       Ignore.
    {
        use starry_signal::SignalDisposition;
        let mut actions = thread.proc_data.signal.actions.lock();
        let act = &mut actions[signo];
        let force_default = matches!(act.disposition, SignalDisposition::Ignore)
            || (matches!(act.disposition, SignalDisposition::Default)
                && matches!(
                    signo.default_action(),
                    starry_signal::DefaultSignalAction::Ignore
                ));
        if force_default {
            *act = starry_signal::SignalAction::default();
        }
    }
    let mut mask = thread.signal.blocked();
    if mask.has(signo) {
        mask.remove(signo);
        thread.signal.set_blocked(mask);
    }

    // Mark this thread so the next `check_signals` consumes the dump
    // alongside the terminating disposition. Group-exit SIGKILLs sent
    // to peers via `send_signal_to_process` skip this path and leave
    // the flag clear, so they terminate without printing a dump.
    thread
        .fault_dump_pending
        .store(true, core::sync::atomic::Ordering::Release);

    if thread.signal.send_signal(sig) {
        curr.interrupt();
    } else {
        // send_signal returning false means the signal was rejected
        // (already pending). Either way the faulting thread is the
        // right one to terminate, so dump and exit here directly so
        // userspace cannot lose the register state.
        thread
            .fault_dump_pending
            .store(false, core::sync::atomic::Ordering::Release);
        dump_user_crash_context(uctx);
        do_exit(signo as i32, true);
    }

    Ok(())
}
