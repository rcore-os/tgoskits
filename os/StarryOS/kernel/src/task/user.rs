use ax_hal::uspace::{ExceptionInfo, ExceptionKind, ReturnReason, UserContext};
use ax_task::TaskInner;
use starry_process::Pid;
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};

use super::{
    AsThread, SyscallRestartInfo, TimerState, check_signals, raise_signal_fatal, set_timer_state,
    unblock_next_signal,
};
use crate::syscall::handle_syscall;

/// Dump user-mode register state at a fatal exception.
///
/// Logged at `info!` so it shows up in QEMU serial output without needing
/// `RUST_LOG=debug`. Arch-aware; falls back to a placeholder on unexpected
/// targets.
fn dump_user_crash_context(uctx: &UserContext) {
    #[cfg(target_arch = "riscv64")]
    {
        let r = &uctx.regs;
        info!(
            "  pc(sepc)={:#018x} ra={:#018x} sp={:#018x}",
            uctx.sepc, r.ra, r.sp,
        );
        info!("  gp={:#018x}  tp={:#018x}  s0={:#018x}", r.gp, r.tp, r.s0,);
        info!(
            "  a0={:#018x} a1={:#018x} a2={:#018x} a3={:#018x}",
            r.a0, r.a1, r.a2, r.a3,
        );
        info!(
            "  a4={:#018x} a5={:#018x} a6={:#018x} a7={:#018x}",
            r.a4, r.a5, r.a6, r.a7,
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        info!("  pc(elr)={:#018x} spsr={:#018x}", uctx.elr, uctx.spsr,);
        info!(
            "  x0={:#018x} x1={:#018x} x2={:#018x} x3={:#018x}",
            uctx.x[0], uctx.x[1], uctx.x[2], uctx.x[3],
        );
        info!(
            "  x29(fp)={:#018x} x30(lr)={:#018x}",
            uctx.x[29], uctx.x[30],
        );
    }
    #[cfg(target_arch = "x86_64")]
    {
        info!(
            "  rip={:#018x} rsp={:#018x} rflags={:#018x}",
            uctx.rip, uctx.rsp, uctx.rflags,
        );
        info!(
            "  rax={:#018x} rdi={:#018x} rsi={:#018x} rdx={:#018x}",
            uctx.rax, uctx.rdi, uctx.rsi, uctx.rdx,
        );
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let r = &uctx.regs;
        info!(
            "  era={:#018x} ra={:#018x} sp={:#018x} tp={:#018x}",
            uctx.era, r.ra, r.sp, r.tp,
        );
        info!(
            "  a0={:#018x} a1={:#018x} a2={:#018x} a3={:#018x}",
            r.a0, r.a1, r.a2, r.a3,
        );
    }
    #[cfg(not(any(
        target_arch = "riscv64",
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64",
    )))]
    {
        info!("  (register dump not implemented for this arch)");
    }
}

/// Create a new user task.
pub fn new_user_task(name: &str, mut uctx: UserContext, set_child_tid: usize) -> TaskInner {
    TaskInner::new(
        move || {
            let curr = ax_task::current();

            if let Some(tid) = (set_child_tid as *mut Pid).nullable() {
                tid.vm_write(curr.id().as_u64() as Pid).ok();
            }

            info!("Enter user space: ip={:#x}, sp={:#x}", uctx.ip(), uctx.sp());

            let thr = curr.as_thread();
            while !thr.pending_exit() {
                let reason = uctx.run();

                set_timer_state(&curr, TimerState::Kernel);

                let saved_a0 = uctx.arg0();
                let saved_sysno = uctx.sysno();
                let is_syscall = matches!(reason, ReturnReason::Syscall);

                match reason {
                    ReturnReason::Syscall => handle_syscall(&mut uctx),
                    ReturnReason::PageFault(addr, flags) => {
                        if !thr.proc_data.aspace().lock().handle_page_fault(addr, flags) {
                            info!(
                                "{:?}: segmentation fault at {:#x} {:?}",
                                thr.proc_data.proc, addr, flags
                            );
                            dump_user_crash_context(&uctx);
                            raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSEGV))
                                .expect("Failed to send SIGSEGV");
                        }
                    }
                    ReturnReason::Interrupt => {}
                    #[allow(unused_labels)]
                    ReturnReason::Exception(exc_info) => 'exc: {
                        // TODO: detailed handling
                        let kind = exc_info.kind();
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
                            ExceptionKind::IllegalInstruction => Signo::SIGILL,
                            _ => Signo::SIGTRAP,
                        };
                        dump_user_crash_context(&uctx);
                        raise_signal_fatal(SignalInfo::new_kernel(signo))
                            .expect("Failed to send SIGTRAP");
                    }
                    r => {
                        warn!("Unexpected return reason: {r:?}");
                        raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSEGV))
                            .expect("Failed to send SIGSEGV");
                    }
                }

                if !unblock_next_signal() {
                    let eintr_code = -(ax_errno::LinuxError::EINTR.code() as isize);
                    let restart = if is_syscall && (uctx.retval() as isize) == eintr_code {
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
    0
}

#[cfg(target_arch = "loongarch64")]
fn exception_iss_value(_exc_info: &ExceptionInfo) -> u64 {
    0
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
