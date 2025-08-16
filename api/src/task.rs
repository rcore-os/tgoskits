use core::sync::atomic::Ordering;

use axcpu::trap::{ExceptionInfoExt, ExceptionKind};
use axerrno::{LinuxError, LinuxResult};
use axhal::uspace::{ReturnReason, UserContext};
use axtask::{TaskInner, current};
use linux_raw_sys::general::{ROBUST_LIST_LIMIT, robust_list, robust_list_head};
use starry_core::{
    futex::FutexKey,
    shm::SHM_MANAGER,
    task::{
        AsThread, get_process_data, send_signal_to_process, send_signal_to_thread, set_timer_state,
    },
    time::TimerState,
};
use starry_process::Pid;
use starry_signal::{SignalInfo, Signo};

use crate::{
    mm::{UserPtr, access_user_memory, handle_user_page_fault, nullable},
    signal::{check_signals, unblock_next_signal},
    syscall::handle_syscall,
};

/// Create a new user task.
pub fn new_user_task(
    name: &str,
    mut uctx: UserContext,
    set_child_tid: Option<&'static mut Pid>,
) -> TaskInner {
    TaskInner::new(
        move || {
            let curr = axtask::current();
            access_user_memory(|| {
                if let Some(tid) = set_child_tid {
                    *tid = curr.id().as_u64() as Pid;
                }
            });

            info!("Enter user space: ip={:#x}, sp={:#x}", uctx.ip(), uctx.sp());

            let thr = curr.as_thread();
            while !thr.pending_exit() {
                let reason = uctx.run();

                set_timer_state(&curr, TimerState::Kernel);

                match reason {
                    ReturnReason::Syscall => handle_syscall(&mut uctx),
                    ReturnReason::PageFault(addr, flags) => {
                        handle_user_page_fault(&thr.proc_data, addr, flags)
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
                        send_signal_to_process(
                            thr.proc_data.proc.pid(),
                            Some(SignalInfo::new_kernel(signo)),
                        )
                        .expect("Failed to send SIGTRAP");
                    }
                    r => {
                        warn!("Unexpected return reason: {:?}", r);
                        send_signal_to_process(
                            thr.proc_data.proc.pid(),
                            Some(SignalInfo::new_kernel(Signo::SIGSEGV)),
                        )
                        .expect("Failed to send SIGSEGV");
                    }
                }

                if !unblock_next_signal() {
                    while check_signals(thr, &mut uctx, None) {}
                }

                set_timer_state(&curr, TimerState::User);
                // Clear interrupt state
                let _ = curr.interrupt_state();
            }
        },
        name.into(),
        starry_core::config::KERNEL_STACK_SIZE,
    )
}

fn handle_futex_death(entry: *mut robust_list, offset: i64) -> LinuxResult<()> {
    let address = (entry as u64)
        .checked_add_signed(offset)
        .ok_or(LinuxError::EINVAL)?;
    let address: usize = address.try_into().map_err(|_| LinuxError::EINVAL)?;
    let key = FutexKey::new_current(address);

    let curr = current();
    let futex_table = curr.as_thread().proc_data.futex_table_for(&key);

    let Some(futex) = futex_table.get(&key) else {
        return Ok(());
    };
    futex.owner_dead.store(true, Ordering::SeqCst);
    futex.wq.wake(1, u32::MAX);
    Ok(())
}

pub fn exit_robust_list(head: &mut robust_list_head) -> LinuxResult<()> {
    // Reference: https://elixir.bootlin.com/linux/v6.13.6/source/kernel/futex/core.c#L777

    let mut limit = ROBUST_LIST_LIMIT;

    let mut entry = head.list.next;
    let offset = head.futex_offset;
    let pending = head.list_op_pending;

    while !core::ptr::eq(entry, &head.list) {
        let next_entry = UserPtr::from(entry).get_as_mut()?.next;
        if entry != pending {
            handle_futex_death(entry, offset)?;
        }
        entry = next_entry;

        limit -= 1;
        if limit == 0 {
            return Err(LinuxError::ELOOP);
        }
        axtask::yield_now();
    }

    Ok(())
}

pub fn do_exit(exit_code: i32, group_exit: bool) {
    let curr = current();
    let thr = curr.as_thread();

    info!("{} exit with code: {}", curr.id_name(), exit_code);

    let clear_child_tid = UserPtr::<Pid>::from(thr.clear_child_tid());
    if let Ok(clear_tid) = clear_child_tid.get_as_mut() {
        *clear_tid = 0;

        let key = FutexKey::new_current(clear_tid as *const _ as usize);
        let table = thr.proc_data.futex_table_for(&key);
        let guard = table.get(&key);
        if let Some(futex) = guard {
            futex.wq.wake(1, u32::MAX);
        }
        axtask::yield_now();
    }
    let head: UserPtr<robust_list_head> = thr.robust_list_head().into();
    if let Ok(Some(head)) = nullable!(head.get_as_mut())
        && let Err(err) = exit_robust_list(head)
    {
        warn!("exit robust list failed: {:?}", err);
    }

    let process = &thr.proc_data.proc;
    if process.exit_thread(curr.id().as_u64() as Pid, exit_code) {
        process.exit();
        if let Some(parent) = process.parent() {
            if let Some(signo) = thr.proc_data.exit_signal {
                let _ = send_signal_to_process(parent.pid(), Some(SignalInfo::new_kernel(signo)));
            }
            if let Ok(data) = get_process_data(parent.pid()) {
                data.child_exit_event.notify(usize::MAX);
            }
        }
        thr.proc_data.exit_event.wake();

        SHM_MANAGER.lock().clear_proc_shm(process.pid());
    }
    if group_exit && !process.is_group_exited() {
        process.group_exit();
        let sig = SignalInfo::new_kernel(Signo::SIGKILL);
        for tid in process.threads() {
            let _ = send_signal_to_thread(None, tid, Some(sig.clone()));
        }
    }
    thr.set_exit();
}
