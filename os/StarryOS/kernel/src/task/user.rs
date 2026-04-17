use ax_hal::uspace::{ExceptionKind, ReturnReason, UserContext};
use ax_task::TaskInner;
use starry_process::Pid;
use starry_signal::{SignalInfo, Signo};
use starry_vm::{VmMutPtr, VmPtr};

use super::{
    AsThread, SYSCALL_INSN_LEN, SyscallRestartInfo, TimerState, check_signals, raise_signal_fatal,
    set_timer_state, unblock_next_signal,
};
use crate::syscall::handle_syscall;

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
                let is_syscall = matches!(reason, ReturnReason::Syscall);

                match reason {
                    ReturnReason::Syscall => handle_syscall(&mut uctx),
                    ReturnReason::PageFault(addr, flags) => {
                        if !thr.proc_data.aspace.lock().handle_page_fault(addr, flags) {
                            info!(
                                "{:?}: segmentation fault at {:#x} {:?}",
                                thr.proc_data.proc, addr, flags
                            );
                            raise_signal_fatal(SignalInfo::new_kernel(Signo::SIGSEGV))
                                .expect("Failed to send SIGSEGV");
                        }
                    }
                    ReturnReason::Interrupt => {}
                    #[allow(unused_labels)]
                    ReturnReason::Exception(exc_info) => 'exc: {
                        // TODO: detailed handling
                        let signo = match exc_info.kind() {
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
                        Some(SyscallRestartInfo { saved_a0 })
                    } else {
                        None
                    };
                    while check_signals(thr, &mut uctx, None, restart.as_ref()) {}

                    if let Some(info) = &restart {
                        if (uctx.retval() as isize) == eintr_code {
                            let new_ip = uctx.ip() - SYSCALL_INSN_LEN;
                            uctx.set_ip(new_ip);
                            uctx.set_arg0(info.saved_a0);
                        }
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
