#![allow(dead_code)]

use arceos_posix_api as api;
use axerrno::LinuxError;
use axhal::arch::{TrapFrame, UspaceContext};
use axhal::trap::{register_trap_handler, SYSCALL};
use axtask::TaskExtRef;

const SYS_READ: usize = 0;
const SYS_WRITE: usize = 1;
const SYS_SCHED_YIELD: usize = 24;
const SYS_GETPID: usize = 39;
const SYS_CLONE: usize = 56;
const SYS_FORK: usize = 57;
const SYS_EXECVE: usize = 59;
const SYS_EXIT: usize = 60;
const SYS_CLOCK_GETTIME: usize = 228;
const SYS_NANOSLEEP: usize = 230;

fn sys_clone(tf: &TrapFrame, newsp: usize) -> usize {
    let aspace = axtask::current().task_ext().aspace.clone();
    let mut uctx = UspaceContext::from(tf);
    uctx.set_sp(newsp);
    uctx.set_retval(0);
    let new_task = crate::task::spawn_user_task(aspace, uctx);
    new_task.id().as_u64() as usize
}

#[register_trap_handler(SYSCALL)]
fn handle_syscall(tf: &TrapFrame, syscall_num: usize) -> isize {
    let ret = match syscall_num {
        SYS_READ => api::sys_read(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_WRITE => api::sys_write(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_SCHED_YIELD => api::sys_sched_yield() as isize,
        SYS_GETPID => api::sys_getpid() as isize,
        SYS_EXIT => axtask::exit(tf.arg0() as _),
        SYS_CLOCK_GETTIME => unsafe { api::sys_clock_gettime(tf.arg0() as _, tf.arg1() as _) as _ },
        SYS_NANOSLEEP => unsafe { api::sys_nanosleep(tf.arg0() as _, tf.arg1() as _) as _ },
        _ => {
            warn!("Unimplemented syscall: {}", syscall_num);
            -LinuxError::ENOSYS.code() as _
        }
    };
    ret
}
