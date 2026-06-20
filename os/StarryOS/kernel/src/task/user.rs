use ax_runtime::hal::cpu::uspace::{ExceptionInfo, ExceptionKind, ReturnReason, UserContext};
use ax_task::TaskInner;
use starry_process::Pid;
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};
use syscalls::Sysno;

use super::{
    AsThread, SyscallRestartInfo, SyscallTraceState, TimerState, check_signals, poll_process_timer,
    ptrace_stop_current, ptrace_syscall_stop_current, raise_signal_fatal, set_timer_state,
    unblock_next_signal, wait_existing_ptrace_stop_current,
};
use crate::syscall::{handle_syscall, syscall_allows_signal_restart};

/// Create a new user task.
pub fn new_user_task(name: &str, mut uctx: UserContext, set_child_tid: usize) -> TaskInner {
    TaskInner::new(
        move || {
            let curr = ax_task::current();

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
                if thr.proc_data.is_ptrace_singlestep_for(thr.tid())
                    && (thr.proc_data.is_ptrace_traceme() || thr.proc_data.is_ptrace_attached())
                {
                    #[cfg(any(
                        target_arch = "riscv64",
                        target_arch = "aarch64",
                        target_arch = "loongarch64"
                    ))]
                    crate::syscall::ptrace_setup_singlestep(&thr.proc_data, thr.tid(), &mut uctx);
                    #[cfg(target_arch = "x86_64")]
                    crate::syscall::ptrace_setup_singlestep(&thr.proc_data, &mut uctx);
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
                                SyscallTraceState::Entry | SyscallTraceState::Exit => {
                                    thr.proc_data.set_ptrace_syscall_trace_state_for(
                                        tid,
                                        SyscallTraceState::Exit,
                                    )
                                }
                                SyscallTraceState::None => {}
                            }
                        }

                        if let Some(exit_code) = ptrace_exit_event_code(saved_sysno, saved_a0)
                            && crate::syscall::ptrace_notify_exit(
                                thr.proc_data.proc.pid(),
                                exit_code,
                            )
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
                        if !thr.proc_data.aspace().lock().handle_page_fault(addr, flags) {
                            info!(
                                "{:?}: segmentation fault at {:#x} {:?}",
                                thr.proc_data.proc, addr, flags
                            );
                            raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSEGV), &uctx)
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
                            #[cfg(feature = "ebpf-kmod")]
                            ExceptionKind::Breakpoint
                                if crate::uprobe::break_uprobe_handler(&mut uctx).is_some() =>
                            {
                                break 'exc;
                            }
                            // x86_64 completes the out-of-line single-step via a
                            // #DB; other arches handle stepping inside the
                            // breakpoint path, so the debug hook is x86_64-only.
                            #[cfg(all(feature = "ebpf-kmod", target_arch = "x86_64"))]
                            ExceptionKind::Debug
                                if crate::uprobe::debug_uprobe_handler(&mut uctx).is_some() =>
                            {
                                break 'exc;
                            }
                            _ => {}
                        }
                        if matches!(kind, ExceptionKind::Breakpoint)
                            && (thr.proc_data.is_ptrace_traceme()
                                || thr.proc_data.is_ptrace_attached())
                        {
                            let saved_insn = thr.proc_data.take_ptrace_ss_saved_insn_for(thr.tid());
                            if let Some((addr, insn)) = saved_insn {
                                if addr == uctx.ip() {
                                    #[cfg(any(
                                        target_arch = "riscv64",
                                        target_arch = "aarch64",
                                        target_arch = "loongarch64"
                                    ))]
                                    let _ = crate::syscall::ptrace_restore_singlestep_insn(
                                        &thr.proc_data,
                                        thr.tid(),
                                        addr,
                                        insn,
                                    );
                                    #[cfg(not(any(
                                        target_arch = "riscv64",
                                        target_arch = "aarch64",
                                        target_arch = "loongarch64"
                                    )))]
                                    thr.proc_data.set_ptrace_ss_saved_insn_for(
                                        thr.tid(),
                                        Some((addr, insn)),
                                    );
                                } else {
                                    thr.proc_data.set_ptrace_ss_saved_insn_for(
                                        thr.tid(),
                                        Some((addr, insn)),
                                    );
                                }
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
                            && (thr.proc_data.is_ptrace_traceme()
                                || thr.proc_data.is_ptrace_attached())
                        {
                            // Clear TF (bit 8) in the saved RFLAGS.  The Intel
                            // SDM (Vol 3A §17.3.2) states the CPU clears TF
                            // when delivering a TF-induced #DB, but QEMU may
                            // not always honour this.  Clearing explicitly
                            // prevents an unwanted extra single-step on resume.
                            uctx.rflags &= !(1u64 << 8);
                            if let Some(_resume_sig) =
                                ptrace_stop_current(thr, Signo::SIGTRAP, &mut uctx)
                            {
                                break 'exc;
                            }
                        }
                        warn!(
                            "user exception: ip={:#x}, fault_addr={:#x}, kind={:?}, esr={:#x}, \
                             ec={:#x}, iss={:#x}, info={:?}",
                            uctx.ip(),
                            exception_fault_addr(&exc_info),
                            kind,
                            exception_esr_value(&exc_info),
                            exception_ec_value(&exc_info),
                            exception_iss_value(&exc_info),
                            exc_info
                        );
                        let signo = match kind {
                            ExceptionKind::Misaligned => {
                                #[cfg(target_arch = "loongarch64")]
                                if unsafe { uctx.emulate_unaligned() }.is_ok() {
                                    break 'exc;
                                }
                                Signo::SIGBUS
                            }
                            ExceptionKind::Breakpoint => Signo::SIGTRAP,
                            ExceptionKind::IllegalInstruction => {
                                // AArch64 EL0 reads of ID_AA64*_EL1 (CPU feature
                                // detection, e.g. the Go runtime) trap as EC=0 /
                                // IllegalInstruction. Emulate them like Linux
                                // instead of killing the program with SIGILL.
                                #[cfg(target_arch = "aarch64")]
                                if unsafe { uctx.emulate_mrs_id_reg() } {
                                    break 'exc;
                                }
                                Signo::SIGILL
                            }
                            _ => Signo::SIGTRAP,
                        };
                        raise_signal_fatal(SignalInfo::new_kernel(signo), &uctx)
                            .expect("Failed to send SIGTRAP");
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
        },
        name.into(),
        crate::config::KERNEL_STACK_SIZE,
    )
}

fn ptrace_exit_event_code(sysno: usize, arg0: usize) -> Option<i32> {
    match Sysno::new(sysno) {
        Some(Sysno::exit | Sysno::exit_group) => Some((arg0 as i32) << 8),
        _ => None,
    }
}

#[cfg(target_arch = "aarch64")]
fn exception_fault_addr(exc_info: &ExceptionInfo) -> usize {
    exc_info.far
}

#[cfg(target_arch = "aarch64")]
fn exception_esr_value(exc_info: &ExceptionInfo) -> u64 {
    exc_info.esr_value()
}

#[cfg(target_arch = "aarch64")]
fn exception_ec_value(exc_info: &ExceptionInfo) -> u64 {
    exc_info.ec_value()
}

#[cfg(target_arch = "aarch64")]
fn exception_iss_value(exc_info: &ExceptionInfo) -> u64 {
    exc_info.iss_value()
}

#[cfg(target_arch = "riscv64")]
fn exception_fault_addr(exc_info: &ExceptionInfo) -> usize {
    exc_info.stval
}

#[cfg(target_arch = "riscv64")]
fn exception_esr_value(_exc_info: &ExceptionInfo) -> u64 {
    0
}

#[cfg(target_arch = "riscv64")]
fn exception_ec_value(_exc_info: &ExceptionInfo) -> u64 {
    0
}

#[cfg(target_arch = "riscv64")]
fn exception_iss_value(_exc_info: &ExceptionInfo) -> u64 {
    0
}

#[cfg(target_arch = "loongarch64")]
fn exception_fault_addr(exc_info: &ExceptionInfo) -> usize {
    exc_info.badv
}

#[cfg(target_arch = "loongarch64")]
fn exception_esr_value(_exc_info: &ExceptionInfo) -> u64 {
    0
}

#[cfg(target_arch = "loongarch64")]
fn exception_ec_value(_exc_info: &ExceptionInfo) -> u64 {
    _exc_info.ecode as u64
}

#[cfg(target_arch = "loongarch64")]
fn exception_iss_value(_exc_info: &ExceptionInfo) -> u64 {
    _exc_info.esubcode as u64
}

#[cfg(target_arch = "x86_64")]
fn exception_fault_addr(exc_info: &ExceptionInfo) -> usize {
    exc_info.cr2
}

#[cfg(target_arch = "x86_64")]
fn exception_esr_value(_exc_info: &ExceptionInfo) -> u64 {
    0
}

#[cfg(target_arch = "x86_64")]
fn exception_ec_value(_exc_info: &ExceptionInfo) -> u64 {
    0
}

#[cfg(target_arch = "x86_64")]
fn exception_iss_value(_exc_info: &ExceptionInfo) -> u64 {
    0
}
