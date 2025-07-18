use axprocess::Pid;
use axsignal::{SignalInfo, Signo};
use axtask::current;
use linux_raw_sys::general::{SI_KERNEL, robust_list_head};
use starry_core::{
    futex::FutexKey,
    task::{AsThread, get_process_data},
};

use crate::{
    clear_proc_shm, exit_robust_list,
    ptr::{UserPtr, nullable},
    signal::{send_signal_to_process, send_signal_to_thread},
};

pub fn do_exit(exit_code: i32, group_exit: bool) -> ! {
    let curr = current();
    let thr = curr.as_thread();

    info!("{:?} exit with code: {}", curr.id_name(), exit_code);

    let clear_child_tid = UserPtr::<Pid>::from(thr.clear_child_tid());
    if let Ok(clear_tid) = clear_child_tid.get_as_mut() {
        *clear_tid = 0;

        let key = FutexKey::new_current(clear_tid as *const _ as usize);
        let guard = thr.proc_data.futex_table_for(&key).get(&key);
        if let Some(futex) = guard {
            futex.wq.notify_one(false);
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
                let _ = send_signal_to_process(
                    parent.pid(),
                    Some(SignalInfo::new(signo, SI_KERNEL as _)),
                );
            }
            if let Ok(data) = get_process_data(parent.pid()) {
                data.child_exit_event.notify(usize::MAX);
            }
        }

        clear_proc_shm(process.pid());
    }
    if group_exit && !process.is_group_exited() {
        process.group_exit();
        let sig = SignalInfo::new(Signo::SIGKILL, SI_KERNEL as _);
        for tid in process.threads() {
            let _ = send_signal_to_thread(None, tid, Some(sig.clone()));
        }
    }
    axtask::exit(exit_code)
}

pub fn sys_exit(exit_code: i32) -> ! {
    do_exit(exit_code << 8, false)
}

pub fn sys_exit_group(exit_code: i32) -> ! {
    do_exit(exit_code << 8, true)
}
