#[cfg(target_arch = "riscv64")]
use core::mem::{MaybeUninit, align_of, size_of};
use core::{future::poll_fn, task::Poll};

use ax_errno::{AxError, AxResult};
use ax_runtime::hal::cpu::uspace::UserContext;
use ax_task::{
    TaskInner, current,
    future::{block_on, interruptible},
};
use axpoll::IoEvents;
use linux_raw_sys::general::{CLD_CONTINUED, CLD_STOPPED, CLD_TRAPPED};
use starry_process::Pid;
use starry_signal::{SignalInfo, SignalOSAction, SignalSet, Signo};
#[cfg(target_arch = "riscv64")]
use starry_vm::vm_read_slice;

use super::{
    AsThread, ProcessData, SYSCALL_INSN_LEN, Thread, do_exit, get_process_data, get_process_group,
    get_task, is_zombie_pid,
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

#[cfg(target_arch = "riscv64")]
#[derive(Clone, Copy)]
struct UserStackFrame {
    fp: usize,
    ra: usize,
}

#[cfg(target_arch = "riscv64")]
fn read_user_stack_frame(fp: usize) -> Option<UserStackFrame> {
    let frame_addr = fp.checked_sub(size_of::<UserStackFrame>())?;
    if frame_addr == 0 || !frame_addr.is_multiple_of(align_of::<usize>()) {
        return None;
    }

    let mut words = [MaybeUninit::<usize>::uninit(); 2];
    vm_read_slice(frame_addr as *const usize, &mut words).ok()?;

    Some(UserStackFrame {
        fp: unsafe { words[0].assume_init() },
        ra: unsafe { words[1].assume_init() },
    })
}

#[cfg(target_arch = "riscv64")]
fn dump_user_backtrace(uctx: &UserContext) {
    const MAX_USER_FRAMES: usize = 32;

    let mut fp = uctx.regs.s0;
    let sp = uctx.regs.sp;
    warn!(
        "user backtrace:\n  #00 pc={:#018x} ra={:#018x} sp={:#018x} fp={:#018x}",
        uctx.sepc, uctx.regs.ra, sp, fp
    );

    for depth in 1..MAX_USER_FRAMES {
        let Some(frame) = read_user_stack_frame(fp) else {
            warn!("  <unwind stopped: unreadable frame at fp={:#018x}>", fp);
            break;
        };

        if frame.fp == 0 || frame.ra == 0 {
            break;
        }
        if frame.fp <= fp {
            warn!(
                "  <unwind stopped: non-growing fp {:#018x} after {:#018x}>",
                frame.fp, fp
            );
            break;
        }

        let frame_sp = frame.fp - size_of::<UserStackFrame>();
        warn!(
            "  #{:02} pc={:#018x} sp={:#018x} fp={:#018x}",
            depth, frame.ra, frame_sp, frame.fp
        );
        fp = frame.fp;
    }
}

#[cfg(not(target_arch = "riscv64"))]
fn dump_user_backtrace(_uctx: &UserContext) {}

/// Dump user-mode register state once the signal disposition really terminates.
fn dump_user_crash_context(uctx: &UserContext) {
    #[cfg(target_arch = "riscv64")]
    {
        let r = &uctx.regs;
        warn!(
            "user register dump:\n  pc(sepc)={:#018x} ra={:#018x} sp={:#018x}\n  gp={:#018x}  \
             tp={:#018x}  s0/fp={:#018x} s1={:#018x}\n  a0={:#018x} a1={:#018x} a2={:#018x} \
             a3={:#018x}\n  a4={:#018x} a5={:#018x} a6={:#018x} a7={:#018x}\n  s2={:#018x} \
             s3={:#018x} s4={:#018x} s5={:#018x}\n  s6={:#018x} s7={:#018x} s8={:#018x} \
             s9={:#018x}\n  s10={:#018x} s11={:#018x} t3={:#018x} t4={:#018x}\n  t5={:#018x} \
             t6={:#018x}",
            uctx.sepc,
            r.ra,
            r.sp,
            r.gp,
            r.tp,
            r.s0,
            r.s1,
            r.a0,
            r.a1,
            r.a2,
            r.a3,
            r.a4,
            r.a5,
            r.a6,
            r.a7,
            r.s2,
            r.s3,
            r.s4,
            r.s5,
            r.s6,
            r.s7,
            r.s8,
            r.s9,
            r.s10,
            r.s11,
            r.t3,
            r.t4,
            r.t5,
            r.t6,
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

    dump_user_backtrace(uctx);
}

/// Block the current thread in a ptrace stop.
///
/// Returns `Some(resume_signo)` if the thread was traced and is now being
/// resumed by the tracer. `None` means the thread was not traced (no
/// `PTRACE_TRACEME`). The optional `resume_signo` is the signal the tracer
/// chose to inject on resume (via `PTRACE_CONT(sig)`); `None` within the
/// outer `Some` means suppress the original signal.
pub fn ptrace_stop_current(
    thr: &Thread,
    signo: Signo,
    uctx: &mut UserContext,
) -> Option<Option<Signo>> {
    ptrace_stop_current_impl(thr, signo, uctx, false)
}

pub fn ptrace_syscall_stop_current(
    thr: &Thread,
    signo: Signo,
    uctx: &mut UserContext,
) -> Option<Option<Signo>> {
    ptrace_stop_current_impl(thr, signo, uctx, true)
}

pub fn wait_existing_ptrace_stop_current(thr: &Thread, uctx: &mut UserContext) {
    let tid = thr.tid();
    if let Some(signo) = thr.proc_data.ptrace_stop_signo_for(tid) {
        notify_ptrace_waiter(thr, signo);
    }
    wait_ptrace_resume(thr, tid, uctx);
}

fn wait_ptrace_resume(thr: &Thread, tid: u32, uctx: &mut UserContext) {
    current().clear_interrupt();
    let wait_result = block_on(interruptible(poll_fn(|cx| {
        if thr.proc_data.ptrace_stop_signo_for(tid).is_none() {
            Poll::Ready(())
        } else {
            thr.proc_data.register_ptrace_stop_waker(cx.waker());
            if thr.proc_data.ptrace_stop_signo_for(tid).is_none() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    })));

    if wait_result.is_err() {
        thr.proc_data.clear_ptrace_stop();
    } else if let Some(resume_uctx) = thr.proc_data.take_ptrace_stop_user_context_for(tid) {
        *uctx = resume_uctx;
        thr.proc_data.restore_current_fp_for_ptrace(tid, uctx);
    }
}

fn ptrace_stop_current_impl(
    thr: &Thread,
    signo: Signo,
    uctx: &mut UserContext,
    is_syscall_stop: bool,
) -> Option<Option<Signo>> {
    if !thr.proc_data.is_ptrace_traceme() && !thr.proc_data.is_ptrace_attached() {
        return None;
    }

    let tid = thr.tid();
    while !thr.proc_data.claim_ptrace_stop(tid) {
        block_on(poll_fn(|cx| {
            if !thr.proc_data.has_ptrace_stop(tid) {
                Poll::Ready(())
            } else {
                thr.proc_data.register_ptrace_stop_waker(cx.waker());
                if !thr.proc_data.has_ptrace_stop(tid) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }
        }));
    }

    #[cfg(any(
        target_arch = "riscv64",
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "x86_64"
    ))]
    {
        thr.proc_data.save_current_fp_for_ptrace(tid);
    }
    if is_syscall_stop {
        thr.proc_data.set_ptrace_syscall_stop(tid, signo, uctx);
    } else {
        thr.proc_data.set_ptrace_stop(tid, signo, uctx);
    }
    notify_ptrace_waiter(thr, signo);

    wait_ptrace_resume(thr, tid, uctx);
    Some(thr.proc_data.take_ptrace_resume_signo_for(tid))
}

fn notify_ptrace_waiter(thr: &Thread, signo: Signo) {
    let waiter_pid = thr
        .proc_data
        .ptrace_tracer_pid()
        .or_else(|| thr.proc_data.proc.parent().map(|parent| parent.pid()));
    if let Some(waiter_pid) = waiter_pid
        && let Ok(parent_data) = get_process_data(waiter_pid)
    {
        let sigchld = SignalInfo::new_sigchld(
            thr.proc_data.proc.pid(),
            thr.cred().uid,
            CLD_TRAPPED as i32,
            signo as i32,
        );
        let _ = send_signal_to_process(waiter_pid, Some(sigchld));
        // Ptrace stop report is published before waking waiters.
        unsafe { parent_data.child_exit_event.wake(axpoll::IoEvents::IN) };
    }
}

pub fn check_signals(
    thr: &Thread,
    uctx: &mut UserContext,
    restore_blocked: Option<SignalSet>,
    restart_info: Option<&SyscallRestartInfo>,
) -> bool {
    // Honor zap requests before consulting the signal queue. A sibling
    // performing `execve` set this flag, and we must do a thread-only
    // exit (no `group_exit`) so the new image is left intact.
    //
    // `take_exit_request` consumes the flag atomically so the outer
    // `while check_signals(...)` drain loop (see `task/user.rs`) doesn't
    // re-enter `do_exit` for the same zap. After `do_exit` runs, the
    // task's `exit` flag is set; control returns through the drain loop
    // and the user-task outer loop bails on `pending_exit()`.
    if thr.take_exit_request() {
        do_exit(0, false);
        return true;
    }

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

    if signo != Signo::SIGKILL
        && !thr
            .proc_data
            .take_ptrace_resume_signal_bypass_for(thr.tid(), signo)
        && let Some(resume_signo) = ptrace_stop_current(thr, signo, uctx)
    {
        match resume_signo {
            None => return true,
            Some(new_signo) if new_signo != signo => {
                thr.proc_data
                    .set_ptrace_resume_signal_bypass_for(thr.tid(), new_signo);
                let _ = thr.signal.send_signal(SignalInfo::new_kernel(new_signo));
                return true;
            }
            Some(_) => {}
        }
    }

    // Only dump register state when the terminating signal is the same
    // synchronous fault signo that raise_signal_fatal force-delivered to
    // this thread. Matching by signo prevents a low-numbered pending
    // signal (e.g. a queued SIGTERM that landed before the SIGSEGV from
    // a page fault) from consuming the flag and either dumping in the
    // wrong context or swallowing the dump entirely when it had a user
    // handler. `compare_exchange` clears the slot only on a match, so
    // unrelated signals leave the flag intact for the real fault
    // signal that follows.
    let dump_on_terminate = thr
        .fault_dump_signo
        .compare_exchange(
            signo as u8,
            0,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Relaxed,
        )
        .is_ok();

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
        SignalOSAction::Stop => do_job_stop(thr, signo),
        SignalOSAction::Continue => {}
        SignalOSAction::NoFurtherAction => {}
    }
    true
}

/// Notify a process's parent of a job-control state change by sending it
/// `SIGCHLD` (with `CLD_STOPPED`/`CLD_CONTINUED`) and waking its `waitpid`.
fn notify_parent_job_change(proc_data: &ProcessData, code: i32, status: i32) {
    let proc = &proc_data.proc;
    let Some(parent) = proc.parent() else {
        return;
    };
    // si_uid carries the child's real UID; read it from any live thread.
    let child_uid = proc
        .threads()
        .into_iter()
        .next()
        .and_then(|tid| get_task(tid).ok())
        .map_or(0, |task| task.as_thread().cred().uid);
    let sig = SignalInfo::new_sigchld(proc.pid(), child_uid, code, status);
    let _ = send_signal_to_process(parent.pid(), Some(sig));
    if let Ok(data) = get_process_data(parent.pid()) {
        // Job-control report is published before waking waiters.
        unsafe { data.child_exit_event.wake(axpoll::IoEvents::IN) };
    }
}

/// Enter a job-control stop: record the stop, notify the parent, then park the
/// current thread until `SIGCONT` clears the stop (or `SIGKILL` force-resumes it
/// so the kill can proceed).
///
/// Uses a plain block — not [`interruptible`](ax_task::future::interruptible) —
/// because an ordinary signal must **not** wake a stopped process; only
/// continue/kill clear `is_job_stopped`.
///
/// The STOP-immediately-followed-by-CONT race (e.g. busybox `killall5 -STOP`
/// then `-CONT`) is closed by snapshotting `continue_generation` *before*
/// recording the stop: if a `SIGCONT` bumped the generation in between,
/// [`ProcessData::set_job_stopped`] returns `false` and we never park. This
/// replaces the pending-signal scrubbing the reference design used (which would
/// require modifying `starry-signal`).
///
/// Known limitations (acceptable for the single-threaded shells/tools this
/// targets):
/// - Only the thread that dequeues the stop signal parks; sibling threads of a
///   multi-threaded process keep running until they next hit a stop signal.
///   Linux stops every thread in the group.
fn do_job_stop(thr: &Thread, signo: Signo) {
    let proc_data = &thr.proc_data;
    // Snapshot before recording the stop so a racing SIGCONT (which advances the
    // generation) cancels this stop.
    let continue_gen = proc_data.continue_generation();
    if !proc_data.set_job_stopped(signo, continue_gen) {
        return;
    }
    notify_parent_job_change(proc_data, CLD_STOPPED as i32, signo as i32);

    let cont_event = proc_data.cont_event();
    block_on(poll_fn(|cx| {
        if !proc_data.is_job_stopped() {
            return Poll::Ready(());
        }
        // Registration happens from the stopped task context.
        unsafe { cont_event.register(cx.waker(), axpoll::IoEvents::IN) };
        // Re-check after registering to avoid a lost wakeup if the continue
        // landed between the check above and registration.
        if proc_data.is_job_stopped() {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }));
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
    let result = f();
    if let Some(old) = old_blocked {
        sig.set_blocked(old);
    }
    result
}

pub(super) fn send_signal_thread_inner(task: &TaskInner, thr: &Thread, sig: SignalInfo) {
    let accepted = thr.signal.send_signal(sig);
    // Always wake signalfd waiters so a signalfd monitoring for this signal
    // (even a blocked one) can become readable in epoll/poll.  Without this,
    // a process using signalfd + SA_RESTART or signalfd + blocked signals
    // would never observe newly-pending signals from the event loop.
    unsafe { thr.signalfd_waker.wake(IoEvents::IN) };
    if accepted {
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
        // Only wake the target thread when the signal is deliverable
        // (not blocked/not ignored).  Sending a blocked signal via
        // tkill/tgkill must NOT interrupt the target per POSIX; the signal
        // is queued as pending and stays invisible until unblocked.
        if thread.signal.send_signal(sig) {
            task.interrupt();
        }
        // Always wake signalfd waiters — even blocked signals should be
        // visible via signalfd in an epoll event loop.
        unsafe { thread.signalfd_waker.wake(IoEvents::IN) };
    }

    Ok(())
}

/// Sends a signal to a process.
pub fn send_signal_to_process(pid: Pid, sig: Option<SignalInfo>) -> AxResult<()> {
    let proc_data = match get_process_data(pid) {
        Ok(proc_data) => proc_data,
        Err(_) => {
            // A zombie process has exited but not yet been reaped by waitpid().
            // Its ProcessData is gone, but the PID still exists: kill(pid, 0)
            // must return 0, and signals are silently dropped (no live threads).
            if is_zombie_pid(pid) {
                return Ok(());
            }
            return Err(AxError::NoSuchProcess);
        }
    };

    // Job-control side effects must run at send time: a stopped process is
    // parked in the kernel and cannot dequeue SIGCONT itself.
    if let Some(sig) = &sig {
        match sig.signo() {
            // POSIX: SIGCONT resumes a stopped process and reports CLD_CONTINUED.
            // `set_job_continued` (evaluated in the guard) always advances the
            // process's continue generation as a side effect — so a stop signal
            // already dequeued but not yet parked (e.g. killall5's
            // kill(-1,SIGSTOP) immediately followed by kill(-1,SIGCONT)) observes
            // the continue and skips parking, closing the STOP-then-CONT race
            // without scrubbing the pending queue — and returns whether the
            // process had actually been stopped; only then do we notify the parent.
            Signo::SIGCONT if proc_data.set_job_continued() => {
                notify_parent_job_change(&proc_data, CLD_CONTINUED as i32, Signo::SIGCONT as i32);
            }
            Signo::SIGKILL => proc_data.clear_job_stop_for_kill(),
            _ => {}
        }
    }

    if let Some(sig) = sig {
        let signo = sig.signo();
        info!("Send signal {signo:?} to process {pid}");
        if signo == Signo::SIGKILL && proc_data.ptrace_stop_signo().is_some() {
            proc_data.clear_ptrace_stop();
        }
        if let Some(tid) = proc_data.signal.send_signal(sig) {
            // A thread was found that doesn't have the signal blocked.
            // Mark it interrupted so blocking syscalls wrapped by
            // `future::interruptible` can return EINTR promptly.
            if let Ok(task) = get_task(tid) {
                task.interrupt();
            }
        } else {
            // All threads have this signal blocked — the signal is now pending
            // at the process level.  Only wake threads that are sleeping
            // in rt_sigtimedwait/sigwaitinfo waiting for this specific signal:
            // those are the only threads that can dequeue a blocked signal.
            // Waking other threads (e.g. ones blocked in waitpid) would cause
            // spurious EINTR.
            for tid in proc_data.proc.threads() {
                if let Ok(task) = get_task(tid)
                    && task
                        .as_thread()
                        .signal
                        .sigwait_set
                        .lock()
                        .is_some_and(|s| s.has(signo))
                {
                    ax_task::wake_task(&task);
                }
            }
        }
        // Wake signalfd waiters on every thread: even blocked process-level
        // signals must be visible from signalfd in an epoll event loop.
        for tid in proc_data.proc.threads() {
            if let Ok(task) = get_task(tid)
                && let Some(thr) = task.try_as_thread()
            {
                unsafe { thr.signalfd_waker.wake(IoEvents::IN) };
            }
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
            // A zombie's ProcessData may already be freed; skip it so live
            // siblings still receive the signal.
            if let Err(e) = send_signal_to_process(proc.pid(), Some(sig.clone())) {
                debug!(
                    "send_signal_to_process_group: skipped pid {}: {:?}",
                    proc.pid(),
                    e
                );
            }
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
        let actions_arc = thread.proc_data.signal.actions();
        let mut actions = actions_arc.lock();
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

    // Tag the dump request with the specific fault signo so a later
    // `check_signals` only consumes it when that signal is the one
    // being delivered. Group-exit SIGKILLs sent to peers via
    // `send_signal_to_process` skip this path and leave the slot at
    // zero, so peers terminate silently. Storing 0 elsewhere is the
    // "no dump" sentinel — signo values start at 1.
    thread
        .fault_dump_signo
        .store(signo as u8, core::sync::atomic::Ordering::Release);

    if thread.signal.send_signal(sig) {
        curr.interrupt();
    } else {
        // send_signal returning false means the signal was rejected
        // (already pending). Either way the faulting thread is the
        // right one to terminate, so dump and exit here directly so
        // userspace cannot lose the register state.
        thread
            .fault_dump_signo
            .store(0, core::sync::atomic::Ordering::Release);
        dump_user_crash_context(uctx);
        do_exit(signo as i32, true);
    }

    Ok(())
}
