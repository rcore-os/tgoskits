use ax_memory_addr::VirtAddr;
use ax_runtime::{
    hal::cpu::{
        trap::PageFaultFlags,
        uspace::{ExceptionKind, ReturnReason, UserContext},
    },
    thread::UserExecutionContext,
};
use starry_signal::{FPE_INTDIV, SEGV_ACCERR, SEGV_MAPERR, SignalInfo, Signo};
use syscalls::Sysno;

#[cfg(target_arch = "loongarch64")]
use super::unaligned::{UnalignedEmulationResult, emulate_user_unaligned};
use super::{
    SignalCheckOutcome, SyscallRestartInfo, SyscallTraceState, Thread, TidNumber, check_signals,
    check_signals_with_outcome, current_user_task, ptrace_stop_current,
    ptrace_syscall_stop_current, raise_signal_fatal, wait_existing_ptrace_stop_current,
};
use crate::{
    mm::{FaultResult, VmMutPtr, VmPtr},
    syscall::{SyscallRestart, handle_syscall, syscall_allows_signal_restart},
};

fn handle_user_page_fault(
    thread: &Thread,
    address: VirtAddr,
    flags: PageFaultFlags,
    context: &UserContext,
) {
    // Count every user-mode fault for /proc/vmstat pgfault (mm/vmstat.c
    // semantics: all faults, before resolution). Kernel-mode faults on user
    // addresses are counted separately in the mm page-fault handler.
    crate::mm::PAGE_FAULT_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    // Resolve through the pinned MM's fault transaction. File faults past EOF
    // are SIGBUS/BUS_ADRERR on Linux; collapsing them into SIGSEGV makes mmap'd
    // databases and runtimes mis-handle truncation.  A transient eviction
    // conflict is left retryable and does not publish a signal.
    let signal = {
        let Ok(aspace) = thread.proc_data.pin_aspace() else {
            return;
        };
        match aspace.handle_page_fault_result(address, flags) {
            FaultResult::Handled | FaultResult::Retry => None,
            // Preserve user-mode refault and the signal check below while
            // keeping allocation failure distinct from transaction contention
            // for the kernel's faultable-copy error path.
            FaultResult::NoMemory => None,
            FaultResult::PermissionDenied => Some((Signo::SIGSEGV, SEGV_ACCERR)),
            FaultResult::Unmapped => Some((Signo::SIGSEGV, SEGV_MAPERR)),
            FaultResult::Sigbus(code) => Some((Signo::SIGBUS, code as i32)),
        }
    };
    if let Some((signo, si_code)) = signal {
        warn!(
            "{:?}: synchronous {signo:?} memory fault at {:#x} {:?}",
            thread.proc_data.proc, address, flags
        );
        raise_signal_fatal(
            SignalInfo::new_fault(signo, si_code, address.as_usize()),
            context,
        )
        .expect("Failed to send synchronous memory-fault signal");
    }
}

/// Creates the entry closure for one scheduler-owned user thread.
pub fn new_user_task(
    uctx: UserContext,
    set_child_tid: usize,
    child_tid: TidNumber,
) -> impl FnOnce() + Send + 'static {
    move || {
        let curr = current_user_task();
        let mut uctx = UserExecutionContext::bind(uctx)
            .expect("user register image must bind to its current runtime context");

        if let Some(tid) = (set_child_tid as *mut u32).nullable() {
            tid.vm_write(&curr, child_tid.get()).ok();
        }

        info!("Enter user space: ip={:#x}, sp={:#x}", uctx.ip(), uctx.sp());

        let thr = curr.as_thread();
        let resumed_from_initial_ptrace_stop =
            if thr.proc_data.ptrace_stop_signo_for(thr.tid()).is_some() {
                wait_existing_ptrace_stop_current(thr, &mut uctx);
                true
            } else if thr.tid().pid_number() == thr.proc_data.proc.pid().pid_number()
                && thr.proc_data.ptrace_stop_signo().is_some()
            {
                let _ = ptrace_stop_current(thr, Signo::SIGSTOP, &mut uctx);
                true
            } else {
                false
            };
        if resumed_from_initial_ptrace_stop {
            // `block_on_user` consumes the interruption that aborted the stop.
            // Re-scan pending signals before the first user instruction so an
            // un-interceptable SIGKILL cannot be replaced by a racing `_exit(0)`.
            while check_signals(&curr, &mut uctx, None, None) {}
        }
        while !thr.pending_exit() {
            if thr.proc_data.has_ptrace_singlestep_work() {
                let tid = thr.tid();
                let is_ptraced =
                    thr.proc_data.is_ptrace_traceme() || thr.proc_data.is_ptrace_attached();
                if thr.proc_data.is_ptrace_singlestep_for(tid) && is_ptraced {
                    #[cfg(any(
                        target_arch = "riscv64",
                        target_arch = "aarch64",
                        target_arch = "loongarch64"
                    ))]
                    {
                        // An IRQ can return here after the stepped branch reached
                        // the planted breakpoint PC but before the breakpoint trap
                        // was delivered. Report that as the completed single-step
                        // instead of restoring the breakpoint and running past it.
                        if crate::syscall::ptrace_complete_singlestep_breakpoint_if_at_ip(
                            &thr.proc_data,
                            tid,
                            &mut uctx,
                        ) && ptrace_stop_current(thr, Signo::SIGTRAP, &mut uctx).is_some()
                        {
                            continue;
                        }
                    }

                    crate::syscall::ptrace_setup_singlestep(&thr.proc_data, tid, &mut uctx);
                }
            }

            let reason = uctx
                .enter()
                .expect("return-to-user validation must match the current task and address space");

            let saved_a0 = uctx.arg0();
            let saved_sysno = uctx.sysno();
            let continuation_ip = uctx.ip();
            let is_syscall = matches!(reason, ReturnReason::Syscall);
            let mut syscall_restart = SyscallRestart::Allowed;

            if stop_for_pending_ptrace_event(thr, &mut uctx) {
                continue;
            }

            match reason {
                ReturnReason::Syscall => {
                    let ptrace_trace = thr.proc_data.ptrace.syscall_trace_if_active(|| thr.tid());
                    if matches!(ptrace_trace, Some((_, SyscallTraceState::Entry)))
                        && let Some(resume_signo) =
                            ptrace_syscall_stop_current(thr, Signo::SIGTRAP, &mut uctx, saved_sysno)
                    {
                        enqueue_ptrace_syscall_resume_signal(thr, resume_signo);
                    }

                    // The tracer may replace the syscall number and arguments
                    // while the entry stop is active.
                    let syscall_no = uctx.sysno();
                    let syscall_arg0 = uctx.arg0();
                    let address_space_replaced = syscall_no == Sysno::execve as usize
                        || syscall_no == Sysno::execveat as usize;
                    if ptrace_trace.is_some()
                        && let Some(exit_code) = ptrace_exit_event_code(syscall_no, syscall_arg0)
                        && crate::syscall::ptrace_notify_exit(thr.tid(), exit_code)
                    {
                        let _ = ptrace_stop_current(thr, Signo::SIGTRAP, &mut uctx);
                    }

                    syscall_restart = handle_syscall(&curr, &mut uctx);
                    if address_space_replaced {
                        uctx.refresh_address_space()
                            .expect("execve must leave a valid current address space");
                    }
                    if let Some((tid, _)) = ptrace_trace {
                        if stop_for_pending_ptrace_event(thr, &mut uctx) {
                            continue;
                        }
                        if let Some(former_tid) = thr.proc_data.take_ptrace_exec_stop_pending() {
                            let _is_event =
                                crate::syscall::ptrace_notify_exec(thr.tid(), former_tid);
                            if let Some(_resume_sig) =
                                ptrace_stop_current(thr, Signo::SIGTRAP, &mut uctx)
                            {
                                continue;
                            }
                        }
                        if matches!(
                            thr.proc_data.ptrace_syscall_trace_state_for(tid),
                            SyscallTraceState::Exit
                        ) {
                            let resume_signo = ptrace_syscall_stop_current(
                                thr,
                                Signo::SIGTRAP,
                                &mut uctx,
                                syscall_no,
                            );
                            enqueue_ptrace_syscall_resume_signal(thr, resume_signo.flatten());
                        }
                    }
                }
                ReturnReason::PageFault(addr, flags) => {
                    handle_user_page_fault(thr, addr, flags, &uctx);
                }
                ReturnReason::Interrupt => {}
                ReturnReason::Exception(exc_info) => {
                    handle_user_exception(&curr, &mut uctx, exc_info);
                }
                r => {
                    warn!("Unexpected return reason: {r:?}");
                    raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSEGV), &uctx)
                        .expect("Failed to send SIGSEGV");
                }
            }

            if !thr.unblock_next_signal_check() && thr.has_user_return_work() {
                let eintr_code = -(crate::Errno::EINTR.into_raw() as isize);
                let restart = if is_syscall
                    && (uctx.retval() as isize) == eintr_code
                    && syscall_restart.is_allowed()
                    && syscall_allows_signal_restart(saved_sysno)
                {
                    Some(SyscallRestartInfo {
                        saved_a0,
                        saved_sysno,
                        continuation_ip,
                        without_handler: syscall_restart.can_restart_without_handler(),
                    })
                } else {
                    None
                };
                // The first user handler decides whether to restart a wait.
                // Kernel-only processing preserves that internal restart
                // class; other syscall classes retain their existing policy.
                let mut pending_restart = restart.as_ref();
                let mut deferred_mask_restore = thr.take_deferred_signal_mask_restore();
                loop {
                    // Match Linux's recalc_sigpending()/TIF_SIGPENDING
                    // handshake: only acknowledge wake publications that were
                    // visible before this safe-point scan. A signal published
                    // after the snapshot remains sticky and forces another
                    // pass, so returning to userspace cannot erase the sole
                    // wake reason for a subsequent interruptible syscall.
                    let interrupt_snapshot = thr.interrupt_snapshot();
                    loop {
                        let outcome = check_signals_with_outcome(
                            &curr,
                            &mut uctx,
                            deferred_mask_restore,
                            pending_restart,
                        );
                        match outcome {
                            SignalCheckOutcome::None => {
                                if let Some(old_blocked) = deferred_mask_restore.take() {
                                    // The interrupt was not backed by a signal
                                    // that remains deliverable. Restore the
                                    // syscall-entry mask instead of leaking the
                                    // temporary mask.
                                    thr.signal().set_blocked(old_blocked);
                                }
                                // Linux retains ERESTARTSYS when get_signal()
                                // finds no user handler, including after a
                                // stop/continue cycle or a consumed wakeup.
                                if let Some(info) = pending_restart
                                    && info.without_handler
                                {
                                    info.restore_context(&mut uctx);
                                    pending_restart = None;
                                }
                                break;
                            }
                            SignalCheckOutcome::HandlerInstalled => {
                                // The signal frame now owns restoration of the
                                // syscall-entry mask through rt_sigreturn.
                                deferred_mask_restore = None;
                                pending_restart = None;
                            }
                            SignalCheckOutcome::HandledInKernel => {
                                // Keep the temporary mask until a handler owns
                                // restoration or no deliverable signals remain.
                                // Only the wait-family internal restart class
                                // changes its kernel-only signal policy here.
                                if pending_restart.is_some_and(|info| !info.without_handler) {
                                    pending_restart = None;
                                }
                            }
                        }
                    }
                    thr.acknowledge_interrupt(interrupt_snapshot);
                    if !thr.interrupted() {
                        break;
                    }
                }
            }
        }
    }
}

#[cold]
fn handle_user_exception(
    current: &super::UserTaskRef,
    uctx: &mut UserContext,
    exc_info: ax_runtime::hal::cpu::uspace::ExceptionInfo,
) {
    let thr = current.as_thread();
    let kind = exc_info.kind();
    // A uprobe plants an `int3` in user text (delivered as a
    // #BP / Breakpoint exception) and completes its
    // out-of-line single-step via a #DB / Debug exception.
    // Route both to this process' uprobe manager before any
    // ptrace / signal handling: if a uprobe owns the
    // faulting address it fixes up `uctx` (sets the
    // out-of-line PC + single-step, or restores PC after the
    // step) and we resume directly. If not, fall through.
    match kind {
        ExceptionKind::Breakpoint
            if crate::uprobe::break_uprobe_handler(current, uctx).is_some() =>
        {
            return;
        }
        // x86_64 completes the out-of-line single-step via a
        // #DB; other arches handle stepping inside the
        // breakpoint path, so the debug hook is x86_64-only.
        #[cfg(target_arch = "x86_64")]
        ExceptionKind::Debug if crate::uprobe::debug_uprobe_handler(current, uctx).is_some() => {
            return;
        }
        _ => {}
    }
    if matches!(kind, ExceptionKind::Breakpoint)
        && (thr.proc_data.is_ptrace_traceme() || thr.proc_data.is_ptrace_attached())
    {
        #[cfg(any(
            target_arch = "riscv64",
            target_arch = "aarch64",
            target_arch = "loongarch64"
        ))]
        {
            let _ = crate::syscall::ptrace_complete_singlestep_breakpoint_if_at_ip(
                &thr.proc_data,
                thr.tid(),
                uctx,
            );
        }
        if let Some(_resume_sig) = ptrace_stop_current(thr, Signo::SIGTRAP, uctx) {
            return;
        }
    }
    // On x86_64, PTRACE_SINGLESTEP sets TF in RFLAGS;
    // the resulting #DB exception arrives here.
    // ExceptionKind::Debug and uctx.rflags only exist on
    // x86_64, so this whole block is arch-gated.
    #[cfg(target_arch = "x86_64")]
    if matches!(kind, ExceptionKind::Debug)
        && (thr.proc_data.is_ptrace_traceme() || thr.proc_data.is_ptrace_attached())
    {
        // Clear TF (bit 8) in the saved RFLAGS.  The Intel
        // SDM (Vol 3A §17.3.2) states the CPU clears TF
        // when delivering a TF-induced #DB, but QEMU may
        // not always honour this.  Clearing explicitly
        // prevents an unwanted extra single-step on resume.
        let _ = uctx.clear_single_step_after_debug();
        thr.proc_data.set_ptrace_singlestep_for(thr.tid(), false);
        if let Some(_resume_sig) = ptrace_stop_current(thr, Signo::SIGTRAP, uctx) {
            return;
        }
    }
    if matches!(kind, ExceptionKind::Misaligned) {
        #[cfg(target_arch = "loongarch64")]
        match emulate_user_unaligned(thr, uctx, exc_info.badv) {
            Ok(UnalignedEmulationResult::Complete) => return,
            Ok(UnalignedEmulationResult::PageFault { address, flags }) => {
                handle_user_page_fault(thr, address, flags, uctx);
                // A resolved fault leaves ERA unchanged; exiting this
                // exception block returns to the outer user loop and retries
                // the same instruction. A fatal fault is delivered below.
                return;
            }
            Err(err) => {
                let exe_path = thr.proc_data.exe_path().clone();
                warn!(
                    "loongarch64 unaligned emulation failed: task={}, pid={}, exe='{}', ip={:#x}, \
                     fault_addr={:#x}, err={}, info={:?}",
                    current.id_name(),
                    thr.proc_data.proc.pid(),
                    exe_path,
                    uctx.ip(),
                    exc_info.fault_addr().unwrap_or(0),
                    err,
                    exc_info,
                );
            }
        }
    }
    let syndrome = exc_info.syndrome();
    warn!(
        "user exception: ip={:#x}, fault_addr={:#x}, kind={:?}, esr={:#x}, ec={:#x}, iss={:#x}, \
         info={:?}",
        uctx.ip(),
        exc_info.fault_addr().unwrap_or(0),
        kind,
        syndrome.raw,
        syndrome.class,
        syndrome.iss,
        exc_info
    );
    let sig_info = match kind {
        ExceptionKind::Misaligned => SignalInfo::new_kernel(Signo::SIGBUS),
        ExceptionKind::Breakpoint => SignalInfo::new_kernel(Signo::SIGTRAP),
        ExceptionKind::IllegalInstruction => {
            // AArch64 EL0 reads of ID_AA64*_EL1 (CPU feature
            // detection, e.g. the Go runtime) trap as EC=0 /
            // IllegalInstruction. Emulate them like Linux
            // instead of killing the program with SIGILL.
            #[cfg(target_arch = "aarch64")]
            if unsafe { uctx.emulate_mrs_id_reg() } {
                return;
            }
            SignalInfo::new_kernel(Signo::SIGILL)
        }
        // x86 `#DE`: integer divide-by-zero or the
        // `INT_MIN / -1` overflow. POSIX/Linux deliver SIGFPE
        // with si_code FPE_INTDIV and si_addr = faulting PC.
        // The HotSpot JVM's x86 interpreter/JIT emit a bare
        // `idiv` and rely on exactly this signal to raise a
        // Java ArithmeticException; routing it through the old
        // `_ => SIGTRAP` fall-through made the JVM abort mid
        // javac compilation. (Other arches do not trap on
        // integer divide-by-zero, so they never reach here.)
        ExceptionKind::ArithmeticError => {
            SignalInfo::new_fault(Signo::SIGFPE, FPE_INTDIV, uctx.ip())
        }
        _ => SignalInfo::new_kernel(Signo::SIGTRAP),
    };
    raise_signal_fatal(sig_info, uctx).expect("Failed to send fatal exception signal");
}

fn ptrace_exit_event_code(sysno: usize, arg0: usize) -> Option<i32> {
    match Sysno::new(sysno) {
        Some(Sysno::exit | Sysno::exit_group) => Some((arg0 as i32) << 8),
        _ => None,
    }
}

#[inline]
fn stop_for_pending_ptrace_event(thr: &super::Thread, uctx: &mut UserContext) -> bool {
    if !thr.proc_data.has_ptrace_pending_event() {
        return false;
    }
    let tid = thr.tid();
    thr.proc_data.has_ptrace_pending_event_for(tid)
        && ptrace_stop_current(thr, Signo::SIGTRAP, uctx).is_some()
}

fn enqueue_ptrace_syscall_resume_signal(thr: &super::Thread, resume_signo: Option<Signo>) {
    let Some(signo) = resume_signo else {
        return;
    };

    // A PTRACE_SYSCALL resume signal is delivered after the matching syscall
    // exit stop. Do not arm the ptrace bypass: the tracer must observe its
    // subsequent signal-delivery stop.
    let _ = thr.signal().send_signal(SignalInfo::new_kernel(signo));
}
