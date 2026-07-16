use ax_runtime::hal::cpu::uspace::{ExceptionKind, ReturnReason, UserContext};
use starry_process::Pid;
use starry_signal::{FPE_INTDIV, SEGV_ACCERR, SEGV_MAPERR, SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};
use syscalls::Sysno;

use super::{
    SyscallRestartInfo, SyscallTraceState, TimerState, check_signals, current_user_task,
    poll_process_timer, ptrace_stop_current, ptrace_syscall_stop_current, raise_signal_fatal,
    set_timer_state, unblock_next_signal, wait_existing_ptrace_stop_current,
};
use crate::syscall::{handle_syscall, syscall_allows_signal_restart};

/// Creates the entry closure for one scheduler-owned user thread.
pub fn new_user_task(
    mut uctx: UserContext,
    set_child_tid: usize,
) -> impl FnOnce() + Send + 'static {
    move || {
        let curr = current_user_task();

        if let Some(tid) = (set_child_tid as *mut Pid).nullable() {
            tid.vm_write(curr.as_thread().tid() as Pid).ok();
        }

        info!("Enter user space: ip={:#x}, sp={:#x}", uctx.ip(), uctx.sp());

        let thr = curr.as_thread();
        if thr.proc_data.ptrace_stop_signo_for(thr.tid()).is_some() {
            wait_existing_ptrace_stop_current(thr, &mut uctx);
        } else if thr.tid() == thr.proc_data.proc.pid()
            && thr.proc_data.ptrace_stop_signo().is_some()
        {
            let _ = ptrace_stop_current(thr, Signo::SIGSTOP, &mut uctx);
        }
        while !thr.pending_exit() {
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

            let reason = uctx.run();

            set_timer_state(&curr, TimerState::Kernel);

            let saved_a0 = uctx.arg0();
            let saved_sysno = uctx.sysno();
            let is_syscall = matches!(reason, ReturnReason::Syscall);

            match reason {
                ReturnReason::Syscall => {
                    let tid = thr.tid();
                    let trace_state = thr.proc_data.take_ptrace_syscall_trace_for(tid);
                    if matches!(trace_state, SyscallTraceState::Entry)
                        && ptrace_syscall_stop_current(thr, Signo::SIGTRAP, &mut uctx).is_some()
                    {
                        match thr.proc_data.take_ptrace_syscall_trace_for(tid) {
                            SyscallTraceState::Entry | SyscallTraceState::Exit => thr
                                .proc_data
                                .set_ptrace_syscall_trace_state_for(tid, SyscallTraceState::Exit),
                            SyscallTraceState::None => {}
                        }
                    }

                    if let Some(exit_code) = ptrace_exit_event_code(saved_sysno, saved_a0)
                        && crate::syscall::ptrace_notify_exit(thr.proc_data.proc.pid(), exit_code)
                    {
                        let _ = ptrace_stop_current(thr, Signo::SIGTRAP, &mut uctx);
                    }

                    handle_syscall(&mut uctx);
                    if thr.proc_data.has_ptrace_pending_event_for(tid)
                        && let Some(_resume_sig) =
                            ptrace_stop_current(thr, Signo::SIGTRAP, &mut uctx)
                    {
                        continue;
                    }
                    if matches!(
                        thr.proc_data.take_ptrace_syscall_trace_for(tid),
                        SyscallTraceState::Exit
                    ) {
                        let _ = ptrace_syscall_stop_current(thr, Signo::SIGTRAP, &mut uctx);
                    }
                    if thr.proc_data.take_ptrace_exec_stop_pending() {
                        let _is_event =
                            crate::syscall::ptrace_notify_exec(thr.proc_data.proc.pid());
                        if let Some(_resume_sig) =
                            ptrace_stop_current(thr, Signo::SIGTRAP, &mut uctx)
                        {
                            continue;
                        }
                    }
                }
                ReturnReason::PageFault(addr, flags) => {
                    // Count every user-mode fault for /proc/vmstat pgfault (mm/vmstat.c
                    // semantics: all faults, before resolution). Kernel-mode faults on user
                    // addresses are counted separately in the mm page-fault handler.
                    crate::mm::PAGE_FAULT_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    // Classify si_code while holding the aspace lock: an
                    // existing mapping that rejected the access is a
                    // permission violation (SEGV_ACCERR), otherwise the
                    // address is unmapped (SEGV_MAPERR) — matching Linux's
                    // do_user_addr_fault().
                    let si_code = {
                        let aspace = thr.proc_data.aspace();
                        let mut aspace = aspace.lock();
                        if aspace.handle_page_fault(addr, flags) {
                            None
                        } else if aspace.find_area(addr).is_some() {
                            Some(SEGV_ACCERR)
                        } else {
                            Some(SEGV_MAPERR)
                        }
                    };
                    if let Some(si_code) = si_code {
                        warn!(
                            "{:?}: segmentation fault at {:#x} {:?}",
                            thr.proc_data.proc, addr, flags
                        );
                        // POSIX: a synchronous SIGSEGV must carry the
                        // faulting address in si_addr so handlers can
                        // classify and recover from guard-page / implicit-
                        // null-check faults.
                        raise_signal_fatal(
                            SignalInfo::new_fault(Signo::SIGSEGV, si_code, addr.as_usize()),
                            &uctx,
                        )
                        .expect("Failed to send SIGSEGV");
                    }
                }
                ReturnReason::Interrupt => {}
                #[allow(unused_labels)]
                ReturnReason::Exception(exc_info) => 'exc: {
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
                            if crate::uprobe::break_uprobe_handler(&curr, &mut uctx).is_some() =>
                        {
                            break 'exc;
                        }
                        // x86_64 completes the out-of-line single-step via a
                        // #DB; other arches handle stepping inside the
                        // breakpoint path, so the debug hook is x86_64-only.
                        #[cfg(target_arch = "x86_64")]
                        ExceptionKind::Debug
                            if crate::uprobe::debug_uprobe_handler(&curr, &mut uctx).is_some() =>
                        {
                            break 'exc;
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
                                &mut uctx,
                            );
                        }
                        if let Some(_resume_sig) =
                            ptrace_stop_current(thr, Signo::SIGTRAP, &mut uctx)
                        {
                            break 'exc;
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
                        if let Some(_resume_sig) =
                            ptrace_stop_current(thr, Signo::SIGTRAP, &mut uctx)
                        {
                            break 'exc;
                        }
                    }
                    if matches!(kind, ExceptionKind::Misaligned) {
                        #[cfg(target_arch = "loongarch64")]
                        match unsafe { uctx.emulate_unaligned_at(exc_info.badv as u64) } {
                            Ok(()) => break 'exc,
                            Err(err) => {
                                let exe_path = thr.proc_data.exe_path.read().clone();
                                warn!(
                                    "loongarch64 unaligned emulation failed: task={}, pid={}, \
                                     exe='{}', ip={:#x}, fault_addr={:#x}, err={}, info={:?}",
                                    curr.id_name(),
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
                        "user exception: ip={:#x}, fault_addr={:#x}, kind={:?}, esr={:#x}, \
                         ec={:#x}, iss={:#x}, info={:?}",
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
                                break 'exc;
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
                    raise_signal_fatal(sig_info, &uctx)
                        .expect("Failed to send fatal exception signal");
                }
                r => {
                    warn!("Unexpected return reason: {r:?}");
                    raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSEGV), &uctx)
                        .expect("Failed to send SIGSEGV");
                }
            }

            if !unblock_next_signal() {
                // POSIX timers are also driven by the alarm task, but polling
                // here closes the window where an expired timer is only noticed
                // after the current syscall returns to userspace.
                poll_process_timer(thr.proc_data.proc.pid());

                let eintr_code = -(ax_errno::LinuxError::EINTR.code() as isize);
                let restart = if is_syscall
                    && (uctx.retval() as isize) == eintr_code
                    && syscall_allows_signal_restart(saved_sysno)
                {
                    Some(SyscallRestartInfo {
                        saved_a0,
                        saved_sysno,
                    })
                } else {
                    None
                };
                // Single-shot: the first delivered signal decides
                // whether to restart. Subsequent signals in the same
                // loop must not re-apply the decision.
                let mut pending_restart = restart.as_ref();
                while check_signals(thr, &mut uctx, None, pending_restart) {
                    pending_restart = None;
                }
            }

            set_timer_state(&curr, TimerState::User);
            curr.clear_interrupt();
        }
    }
}

fn ptrace_exit_event_code(sysno: usize, arg0: usize) -> Option<i32> {
    match Sysno::new(sysno) {
        Some(Sysno::exit | Sysno::exit_group) => Some((arg0 as i32) << 8),
        _ => None,
    }
}
