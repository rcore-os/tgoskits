use core::sync::atomic::Ordering;

use axprocess::Pid;
use axsignal::{SignalInfo, Signo};
use axtask::{TaskExtRef, current};
use linux_raw_sys::general::{SI_KERNEL, robust_list_head};
use starry_core::task::ProcessData;

use crate::{
    clear_proc_shm, exit_robust_list,
    file::FD_TABLE,
    ptr::{UserPtr, nullable},
    signal::{send_signal_process, send_signal_thread},
};

pub fn do_exit(exit_code: i32, group_exit: bool) -> ! {
    let curr = current();
    let curr_ext = curr.task_ext();

    let thread = &curr_ext.thread;
    info!("{:?} exit with code: {}", thread, exit_code);

    let clear_child_tid = UserPtr::<Pid>::from(curr_ext.thread_data().clear_child_tid());
    if let Ok(clear_tid) = clear_child_tid.get_as_mut() {
        *clear_tid = 0;

        let guard = curr_ext
            .process_data()
            .futex_table
            .get(clear_tid as *const _ as usize);
        if let Some(futex) = guard {
            futex.wq.notify_one(false);
        }
        axtask::yield_now();
    }
    let head: UserPtr<robust_list_head> = curr_ext
        .thread_data()
        .robust_list_head
        .load(Ordering::SeqCst)
        .into();
    if let Ok(Some(head)) = nullable!(head.get_as_mut()) {
        if let Err(err) = exit_robust_list(head) {
            warn!("exit robust list failed: {:?}", err);
        }
    }

    let process = thread.process();
    if thread.exit(exit_code) {
        process.exit();
        if let Some(parent) = process.parent() {
            if let Some(signo) = process.data::<ProcessData>().and_then(|it| it.exit_signal) {
                let _ = send_signal_process(&parent, SignalInfo::new(signo, SI_KERNEL as _));
            }
            if let Some(data) = parent.data::<ProcessData>() {
                data.child_exit_wq.notify_all(false)
            }
        }

        clear_proc_shm(process.pid());
        process.exit();
        // TODO: clear namespace resources
        // FIXME: axns should drop all the resources
        FD_TABLE.clear();
    }
    if group_exit && !process.is_group_exited() {
        process.group_exit();
        let sig = SignalInfo::new(Signo::SIGKILL, SI_KERNEL as _);
        for thr in process.threads() {
            let _ = send_signal_thread(&thr, sig.clone());
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
