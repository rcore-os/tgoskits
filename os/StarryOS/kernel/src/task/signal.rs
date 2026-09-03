use alloc::sync::Arc;
#[cfg(target_arch = "riscv64")]
use core::mem::{MaybeUninit, align_of, size_of};

use ax_runtime::hal::cpu::uspace::UserContext;
use linux_raw_sys::general::{CLD_CONTINUED, CLD_STOPPED, CLD_TRAPPED, RLIMIT_RTTIME};
use starry_signal::{SignalInfo, SignalOSAction, SignalSet, Signo};

use super::{
    PgidNumber, PidIdentity, PidView, ProcessData, ProcessGroup, ROOT_PID_NS, RttimeLimitAction,
    TgidNumber, Thread, TidNumber, UserTaskRef, current_user_task, do_exit,
    get_process_data_by_number, get_process_group_by_number, get_task_by_number,
    signal_publication::publish_before_fatal_stop_release,
};
#[cfg(target_arch = "riscv64")]
use crate::mm::vm_read_slice;
use crate::{
    StarryError, StarryResult,
    mm::UserMemoryProvider,
    task::future::{UserWaitOutcome, block_on, block_on_user},
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
fn read_user_stack_frame(current: &UserTaskRef, fp: usize) -> Option<UserStackFrame> {
    let frame_addr = fp.checked_sub(size_of::<UserStackFrame>())?;
    if frame_addr == 0 || !frame_addr.is_multiple_of(align_of::<usize>()) {
        return None;
    }

    let mut words = [MaybeUninit::<usize>::uninit(); 2];
    vm_read_slice(current, frame_addr as *const usize, &mut words).ok()?;

    Some(UserStackFrame {
        fp: unsafe { words[0].assume_init() },
        ra: unsafe { words[1].assume_init() },
    })
}

#[cfg(target_arch = "riscv64")]
fn dump_user_backtrace(current: &UserTaskRef, uctx: &UserContext) {
    const MAX_USER_FRAMES: usize = 32;

    let mut fp = uctx.regs.s0;
    let sp = uctx.regs.sp;
    warn!(
        "user backtrace:\n  #00 pc={:#018x} ra={:#018x} sp={:#018x} fp={:#018x}",
        uctx.sepc, uctx.regs.ra, sp, fp
    );

    for depth in 1..MAX_USER_FRAMES {
        let Some(frame) = read_user_stack_frame(current, fp) else {
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
fn dump_user_backtrace(_current: &UserTaskRef, _uctx: &UserContext) {}

/// Dump user-mode register state once the signal disposition really terminates.
fn dump_user_crash_context(current: &UserTaskRef, uctx: &UserContext) {
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

    dump_user_backtrace(current, uctx);
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
    ptrace_stop_current_impl(thr, signo, uctx, None)
}

pub fn ptrace_syscall_stop_current(
    thr: &Thread,
    signo: Signo,
    uctx: &mut UserContext,
    syscall_no: usize,
) -> Option<Option<Signo>> {
    ptrace_stop_current_impl(thr, signo, uctx, Some(syscall_no))
}

pub fn wait_existing_ptrace_stop_current(thr: &Thread, uctx: &mut UserContext) {
    let tid = thr.tid();
    if let Some(signo) = thr.proc_data.ptrace_stop_signo_for(tid) {
        notify_ptrace_waiter(thr, signo);
    }
    wait_ptrace_resume(thr, tid, uctx);
}

fn wait_ptrace_resume(thr: &Thread, tid: TidNumber, uctx: &mut UserContext) {
    let task = current_user_task();
    let stale_interrupts = thr.interrupt_snapshot();
    thr.acknowledge_interrupt(stale_interrupts);
    let wait_result = block_on_user(
        &task,
        super::process_wait::wait_on_pollset(thr.proc_data.ptrace_stop_event(), || {
            thr.proc_data
                .ptrace_stop_signo_for(tid)
                .is_none()
                .then_some(())
        }),
    );

    if matches!(wait_result, UserWaitOutcome::Interrupted) {
        thr.proc_data.clear_ptrace_stop();
    } else if matches!(wait_result, UserWaitOutcome::Ready(()))
        && let Some(resume_uctx) = thr.proc_data.take_ptrace_stop_user_context_for(tid)
    {
        *uctx = resume_uctx;
        thr.proc_data.restore_current_fp_for_ptrace(tid, uctx);
    }
}

fn ptrace_stop_current_impl(
    thr: &Thread,
    signo: Signo,
    uctx: &mut UserContext,
    syscall_no: Option<usize>,
) -> Option<Option<Signo>> {
    if !thr.proc_data.is_ptrace_traceme() && !thr.proc_data.is_ptrace_attached() {
        return None;
    }

    let tid = thr.tid();
    while !thr.proc_data.claim_ptrace_stop(tid) {
        block_on(super::process_wait::wait_on_pollset(
            thr.proc_data.ptrace_stop_event(),
            || (!thr.proc_data.has_ptrace_stop(tid)).then_some(()),
        ));
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
    if let Some(syscall_no) = syscall_no {
        thr.proc_data
            .set_ptrace_syscall_stop(tid, signo, uctx, syscall_no);
    } else {
        thr.proc_data.set_ptrace_stop(tid, signo, uctx);
    }
    notify_ptrace_waiter(thr, signo);

    wait_ptrace_resume(thr, tid, uctx);
    Some(thr.proc_data.take_ptrace_resume_signo_for(tid))
}

fn notify_ptrace_waiter(thr: &Thread, signo: Signo) {
    let waiter = thr
        .proc_data
        .ptrace_tracer_identity()
        .or_else(|| thr.proc_data.proc.parent().map(|parent| parent.identity()));
    if let Some(parent_data) = waiter.and_then(|identity| identity.live_data()) {
        let sigchld = new_sigchld_for_receiver(
            &parent_data,
            thr.proc_data.proc.pid(),
            thr.cred().uid,
            CLD_TRAPPED as i32,
            signo as i32,
        );
        let _ = send_signal_to_process_data(&parent_data, Some(sigchld));
        // Ptrace stop report is published before waking waiters.
        unsafe { parent_data.child_exit_event().wake(axpoll::IoEvents::IN) };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SignalCheckOutcome {
    None,
    HandlerInstalled,
    HandledInKernel,
}

impl SignalCheckOutcome {
    fn delivered(self) -> bool {
        !matches!(self, Self::None)
    }
}

pub fn check_signals(
    current: &UserTaskRef,
    uctx: &mut UserContext,
    restore_blocked: Option<SignalSet>,
    restart_info: Option<&SyscallRestartInfo>,
) -> bool {
    check_signals_with_outcome(current, uctx, restore_blocked, restart_info).delivered()
}

pub(crate) fn check_signals_with_outcome(
    current: &UserTaskRef,
    uctx: &mut UserContext,
    restore_blocked: Option<SignalSet>,
    restart_info: Option<&SyscallRestartInfo>,
) -> SignalCheckOutcome {
    let thr = current.as_thread();
    if thr.take_deadline_overrun() {
        let _result = thr
            .signal()
            .send_signal(SignalInfo::new_kernel(Signo::SIGXCPU));
    }

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
        return SignalCheckOutcome::HandledInKernel;
    }

    let mut user_memory = UserMemoryProvider::new(current);
    let Some((sig, os_action)) = thr.signal().check_signals_with(
        &mut user_memory,
        uctx,
        restore_blocked,
        |uctx, _sig, restartable| {
            // Apply the SA_RESTART decision once per interrupted syscall.
            // Callers pass `Some(info)` only for the first delivered signal;
            // later iterations pass `None`, so the restart adjustment remains
            // single-shot.
            if let Some(info) = restart_info
                && (uctx.retval() as isize) == -(crate::Errno::EINTR.into_raw() as isize)
                && restartable
            {
                let new_ip = uctx.ip() - uctx.syscall_insn_len();
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
        },
        || {
            #[cfg(target_arch = "x86_64")]
            {
                let state = ax_runtime::task::capture_current_user_fp_state().expect(
                    "signal delivery must capture FPU state from ordinary current task context",
                );
                starry_signal::arch::SignalFpState::new(state)
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                starry_signal::arch::SignalFpState
            }
        },
    ) else {
        return SignalCheckOutcome::None;
    };

    let signo = sig.signo();

    if signo != Signo::SIGKILL
        && !thr
            .proc_data
            .take_ptrace_resume_signal_bypass_for(thr.tid(), signo)
        && let Some(resume_signo) = ptrace_stop_current(thr, signo, uctx)
    {
        match resume_signo {
            None => return SignalCheckOutcome::HandledInKernel,
            Some(new_signo) if new_signo != signo => {
                thr.proc_data
                    .set_ptrace_resume_signal_bypass_for(thr.tid(), new_signo);
                let _ = thr.signal().send_signal(SignalInfo::new_kernel(new_signo));
                return SignalCheckOutcome::HandledInKernel;
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
    let dump_on_terminate = thr.claim_fault_dump(signo as u8);

    let outcome = if os_action == SignalOSAction::NoFurtherAction {
        SignalCheckOutcome::HandlerInstalled
    } else {
        SignalCheckOutcome::HandledInKernel
    };
    match os_action {
        SignalOSAction::Terminate => {
            if dump_on_terminate {
                dump_user_crash_context(current, uctx);
            }
            do_exit(signo as i32, true);
        }
        SignalOSAction::CoreDump => {
            if dump_on_terminate {
                dump_user_crash_context(current, uctx);
            }
            do_exit(128 + signo as i32, true);
        }
        SignalOSAction::Stop => do_job_stop(thr, signo, uctx),
        SignalOSAction::Continue => {}
        SignalOSAction::NoFurtherAction => {}
    }
    outcome
}

pub(super) fn queue_rttime_limit_signal_from_scheduler_tick(thr: &Thread, observed_ns: u64) {
    let limit = thr.proc_data.rlimit(RLIMIT_RTTIME);
    let (soft_limit_us, hard_limit_us) = (limit.current, limit.max);
    if soft_limit_us == u64::MAX {
        return;
    }
    let action = thr
        .rttime()
        .lock()
        .check_limit_at(thr.cpu_time(), observed_ns, soft_limit_us, hard_limit_us);
    let signo = match action {
        RttimeLimitAction::None => return,
        RttimeLimitAction::Soft => Signo::SIGXCPU,
        RttimeLimitAction::Hard => Signo::SIGKILL,
    };
    let _queued = thr.signal().send_signal(SignalInfo::new_kernel(signo));
}

/// Notify a process's parent of a job-control state change by sending it
/// `SIGCHLD` (with `CLD_STOPPED`/`CLD_CONTINUED`) and waking its `waitpid`.
fn notify_parent_job_change(proc_data: &ProcessData, code: i32, status: i32) {
    let proc = &proc_data.proc;
    let Some(parent) = proc.parent() else {
        return;
    };
    let Ok(parent_data) = get_process_data_by_number(parent.pid()) else {
        return;
    };
    // si_uid carries the child's real UID; read it from any live thread.
    let child_uid = proc
        .threads()
        .into_iter()
        .next()
        .and_then(|tid| get_task_by_number(tid).ok())
        .map_or(0, |task| task.as_thread().cred().uid);
    let sig = new_sigchld_for_receiver(&parent_data, proc.pid(), child_uid, code, status);
    let _ = send_signal_to_process(parent.pid(), Some(sig));
    // Job-control report is published before waking waiters.
    unsafe { parent_data.child_exit_event().wake(axpoll::IoEvents::IN) };
}

/// Builds child status in the namespace of the process that will dequeue it.
pub(crate) fn new_sigchld_for_receiver(
    receiver: &ProcessData,
    child_pid: TgidNumber,
    child_uid: u32,
    code: i32,
    status: i32,
) -> SignalInfo {
    let child_pid = ROOT_PID_NS
        .lookup(child_pid.pid_number())
        .and_then(|identity| {
            PidView::new(receiver.identity().active_namespace()).visible_number(&identity)
        })
        .map_or(0, |pid| pid.get());
    SignalInfo::new_sigchld(child_pid, child_uid, code, status)
}

/// Enter a job-control stop: record the stop, notify the parent, then park the
/// current thread until `SIGCONT` clears the stop (or `SIGKILL` force-resumes it
/// so the kill can proceed). A seized tracer may wake this loop solely to
/// publish `PTRACE_EVENT_STOP`; that wake does not release the job stop.
///
/// Uses a plain block, not [`interruptible`],
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
fn do_job_stop(thr: &Thread, signo: Signo, uctx: &mut UserContext) {
    let proc_data = &thr.proc_data;
    // Snapshot before recording the stop so a racing SIGCONT (which advances the
    // generation) cancels this stop.
    let continue_gen = proc_data.continue_generation();
    let tid = thr.tid();
    if !proc_data.set_job_stopped(signo, continue_gen, tid) {
        return;
    }
    notify_parent_job_change(proc_data, CLD_STOPPED as i32, signo as i32);

    let tid = thr.tid();
    let cont_event = proc_data.cont_event();
    while proc_data.is_job_stopped() {
        if proc_data.has_ptrace_pending_event_for(tid) {
            let resume_signo = ptrace_stop_current(thr, signo, uctx).flatten();
            match resume_signo {
                // Linux re-reports a seized group stop after a tracer supplies
                // SIGCONT to resume its PTRACE_EVENT_STOP. The signal is not a
                // signal-delivery stop, so it must not release the job stop yet.
                Some(Signo::SIGCONT) => {
                    proc_data.set_ptrace_pending_event(
                        tid,
                        crate::syscall::ptrace::PTRACE_EVENT_STOP,
                        0,
                    );
                }
                // A subsequent zero-signal ptrace resume leaves the group stop
                // and resumes user execution. There is no user-visible SIGCONT
                // delivery to enqueue on this path.
                None if proc_data.set_job_continued() => {
                    notify_parent_job_change(
                        proc_data,
                        CLD_CONTINUED as i32,
                        Signo::SIGCONT as i32,
                    );
                }
                _ => {}
            }
            continue;
        }

        block_on(super::process_wait::wait_on_pollset(&cont_event, || {
            (!proc_data.is_job_stopped() || proc_data.has_ptrace_pending_event_for(tid))
                .then_some(())
        }));
    }
}

pub fn block_next_signal() {
    current_user_task().as_thread().block_next_signal_check();
}

pub fn with_blocked_signals<R>(
    blocked: Option<SignalSet>,
    f: impl FnOnce() -> crate::StarryResult<R>,
) -> crate::StarryResult<R> {
    let curr = current_user_task();
    let sig = curr.as_thread().signal();

    let Some(blocked) = blocked else {
        return f();
    };

    let old_blocked = sig.set_blocked(blocked);
    let has_deliverable_signal = || !(sig.pending() & !sig.blocked()).is_empty();
    // A signal may already be pending under the caller's mask. Once the
    // temporary pselect/ppoll mask makes it deliverable, publish the same
    // sticky interruption that a newly arriving signal would publish.
    if has_deliverable_signal() {
        curr.interrupt();
    }

    let result = f();
    if matches!(&result, Err(crate::StarryError::Interrupted)) {
        // Keep the temporary mask active through the return-to-user signal
        // scan. This also closes the window where a signal arrives after the
        // wait reports interruption but before the syscall restores its mask.
        // The signal frame records old_blocked, and rt_sigreturn restores it
        // after the handler, matching Linux's saved_sigmask contract. If no
        // signal remains deliverable, the safe-point scan restores it directly.
        curr.as_thread().defer_signal_mask_restore(old_blocked);
    } else {
        sig.set_blocked(old_blocked);
    }
    result
}

/// Sends a signal to a thread.
pub fn send_signal_to_thread(
    tgid: Option<TgidNumber>,
    tid: TidNumber,
    sig: Option<SignalInfo>,
) -> StarryResult<()> {
    let task = get_task_by_number(tid)?;
    let expected_process = tgid
        .map(get_process_data_by_number)
        .transpose()?
        .map(|process| process.identity());
    send_signal_to_task(&task, expected_process, sig)
}

/// Sends a signal to one already-resolved stable thread generation.
pub(crate) fn send_signal_to_task(
    task: &UserTaskRef,
    expected_process: Option<Arc<PidIdentity>>,
    sig: Option<SignalInfo>,
) -> StarryResult<()> {
    let thread = task.as_thread();
    if expected_process
        .is_some_and(|expected| !Arc::ptr_eq(&expected, &thread.proc_data.identity()))
    {
        return Err(StarryError::NoSuchProcess);
    }

    if let Some(sig) = sig {
        let signo = sig.signo();
        info!("Send signal {signo:?} to thread {}", thread.tid());
        // Only wake the target thread when the signal is deliverable
        // (not blocked/not ignored).  Sending a blocked signal via
        // tkill/tgkill must NOT interrupt the target per POSIX; the signal
        // is queued as pending and stays invisible until unblocked.
        if thread.signal().send_signal(sig) {
            task.interrupt();
        }
        // Always wake signalfd waiters — even blocked signals should be
        // visible via signalfd in an epoll event loop.
        thread.wake_signalfd();
    }

    Ok(())
}

/// Sends a signal to a process.
pub fn send_signal_to_process(pid: TgidNumber, sig: Option<SignalInfo>) -> StarryResult<()> {
    let proc_data = match get_process_data_by_number(pid) {
        Ok(proc_data) => proc_data,
        Err(_) => {
            // A zombie process has exited but not yet been reaped by waitpid().
            // Its ProcessData is gone, but the PID still exists: kill(pid, 0)
            // must return 0, and signals are silently dropped (no live threads).
            if ROOT_PID_NS
                .lookup(pid.pid_number())
                .is_some_and(|identity| identity.is_zombie())
            {
                return Ok(());
            }
            return Err(StarryError::NoSuchProcess);
        }
    };

    send_signal_to_process_data(&proc_data, sig)
}

/// Sends a signal to one already-resolved stable process generation.
pub(crate) fn send_signal_to_process_data(
    proc_data: &Arc<ProcessData>,
    sig: Option<SignalInfo>,
) -> StarryResult<()> {
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
                notify_parent_job_change(proc_data, CLD_CONTINUED as i32, Signo::SIGCONT as i32);
            }
            _ => {}
        }
    }

    if let Some(sig) = sig {
        let signo = sig.signo();
        info!("Send signal {signo:?} to process {}", proc_data.proc.pid());
        let ptrace_stop_tid = (signo == Signo::SIGKILL)
            .then(|| proc_data.selected_ptrace_stop_tid())
            .flatten();
        if signo == Signo::SIGKILL {
            let _wake_tid = publish_before_fatal_stop_release(
                || publish_process_signal(proc_data, sig, ptrace_stop_tid),
                ptrace_stop_tid.map(|_| || proc_data.clear_ptrace_stop()),
                || proc_data.clear_job_stop_for_kill(),
            );
        } else {
            let _wake_tid = publish_process_signal(proc_data, sig, ptrace_stop_tid);
        }
        // Wake signalfd waiters on every thread: even blocked process-level
        // signals must be visible from signalfd in an epoll event loop.
        for tid in proc_data.proc.threads() {
            if let Ok(task) = get_task_by_number(tid) {
                task.as_thread().wake_signalfd();
            }
        }
    }

    Ok(())
}

fn publish_process_signal(
    proc_data: &ProcessData,
    sig: SignalInfo,
    ptrace_stop_tid: Option<TidNumber>,
) -> Option<TidNumber> {
    let wake_tid = proc_data
        .signal
        .send_signal(sig)
        .and_then(|tid| TidNumber::try_from(tid).ok());
    if let Some(tid) = wake_tid
        && let Ok(task) = get_task_by_number(tid)
    {
        // The pending signal is visible before the direct scheduler wake.
        task.interrupt();
    }
    if let Some(tid) = ptrace_stop_tid
        && Some(tid) != wake_tid
        && let Ok(task) = get_task_by_number(tid)
    {
        // A fatal signal must abort the exact traced thread even when the
        // process signal manager selected an unblocked sibling.
        task.interrupt();
    }
    wake_tid
}

/// Sends a signal to a process group.
pub fn send_signal_to_process_group(pgid: PgidNumber, sig: Option<SignalInfo>) -> StarryResult<()> {
    let pg = get_process_group_by_number(pgid)?;

    send_signal_to_process_group_ref(&pg, sig)
}

/// Sends a signal to one already-resolved process-group generation.
pub(crate) fn send_signal_to_process_group_ref(
    pg: &Arc<ProcessGroup>,
    sig: Option<SignalInfo>,
) -> StarryResult<()> {
    if let Some(sig) = sig {
        info!(
            "Send signal {:?} to process group {}",
            sig.signo(),
            pg.pgid()
        );
        for proc in pg.processes() {
            // A zombie's ProcessData may already be freed; skip it so live
            // siblings still receive the signal.
            if let Err(e) = send_signal_to_process(proc.pid_number(), Some(sig)) {
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
pub fn raise_signal_fatal(sig: SignalInfo, uctx: &UserContext) -> crate::StarryResult<()> {
    let curr = current_user_task();
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
    let mut mask = thread.signal().blocked();
    if mask.has(signo) {
        mask.remove(signo);
        thread.signal().set_blocked(mask);
    }

    // Tag the dump request with the specific fault signo so a later
    // `check_signals` only consumes it when that signal is the one
    // being delivered. Group-exit SIGKILLs sent to peers via
    // `send_signal_to_process` skip this path and leave the slot at
    // zero, so peers terminate silently. Storing 0 elsewhere is the
    // "no dump" sentinel — signo values start at 1.
    thread.set_fault_dump(signo as u8);

    if thread.signal().send_signal(sig) {
        curr.interrupt();
    } else {
        // send_signal returning false means the signal was rejected
        // (already pending). Either way the faulting thread is the
        // right one to terminate, so dump and exit here directly so
        // userspace cannot lose the register state.
        thread.clear_fault_dump();
        dump_user_crash_context(&curr, uctx);
        do_exit(signo as i32, true);
    }

    Ok(())
}
