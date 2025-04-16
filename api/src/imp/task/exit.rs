use axprocess::Pid;
use axtask::{TaskExtRef, current};

use crate::ptr::{PtrWrapper, UserPtr};

pub fn do_exit(exit_code: i32, group_exit: bool) -> ! {
    let curr = current();
    let curr_ext = curr.task_ext();

    let thread = &curr_ext.thread;
    info!("{:?} exit with code: {}", thread, exit_code);

    let clear_child_tid = UserPtr::<Pid>::from(curr_ext.thread_data().clear_child_tid());
    if let Ok(clear_tid) = clear_child_tid.get() {
        unsafe { clear_tid.write(0) };
        // TODO: wake up threads, which are blocked by futex, and waiting for the address pointed by clear_child_tid
    }

    let process = thread.process();
    if thread.exit(exit_code) {
        // TODO: send exit signal to parent
        process.exit();
        // TODO: clear namespace resources
    }
    if group_exit && !process.is_group_exited() {
        process.group_exit();
        // TODO: send SIGKILL to other threads
    }
    axtask::exit(exit_code)
}

pub fn sys_exit(exit_code: i32) -> ! {
    do_exit(exit_code << 8, false)
}

pub fn sys_exit_group(exit_code: i32) -> ! {
    do_exit(exit_code << 8, true)
}
