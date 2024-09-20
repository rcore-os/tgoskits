mod fs;
mod mm;
mod task;
mod time;
use axerrno::LinuxError;
use axhal::{
    arch::TrapFrame,
    trap::{register_trap_handler, SYSCALL},
};
use fs::*;
use mm::*;
use task::*;
use time::*;
const SYS_READ: usize = 0;
const SYS_WRITE: usize = 1;
const SYS_MMAP: usize = 9;
const SYS_IOCTL: usize = 16;
const SYS_WRITEV: usize = 20;
const SYS_SCHED_YIELD: usize = 24;
const SYS_NANOSLEEP: usize = 35;
const SYS_GETPID: usize = 39;
const SYS_EXIT: usize = 60;
#[cfg(target_arch = "x86_64")]
const SYS_ARCH_PRCTL: usize = 158;
const SYS_SET_TID_ADDRESS: usize = 218;
const SYS_CLOCK_GETTIME: usize = 228;
const SYS_CLOCK_NANOSLEEP: usize = 230;
const SYS_EXITGROUP: usize = 231;

/// Macro to generate syscall body
///
/// It will receive a function which return Result<_, LinuxError> and convert it to
/// the type which is specified by the caller.
#[macro_export]
macro_rules! syscall_body {
    ($fn: ident, $($stmt: tt)*) => {{
        #[allow(clippy::redundant_closure_call)]
        let res = (|| -> axerrno::LinuxResult<_> { $($stmt)* })();
        match res {
            Ok(_) | Err(axerrno::LinuxError::EAGAIN) => debug!(concat!(stringify!($fn), " => {:?}"),  res),
            Err(_) => info!(concat!(stringify!($fn), " => {:?}"), res),
        }
        match res {
            Ok(v) => v as _,
            Err(e) => {
                -e.code() as _
            }
        }
    }};
}

#[register_trap_handler(SYSCALL)]
fn handle_syscall(tf: &TrapFrame, syscall_num: usize) -> isize {
    let ret = match syscall_num {
        SYS_READ => sys_read(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_WRITE => sys_write(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_MMAP => sys_mmap(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
            tf.arg5() as _,
        ) as _,
        SYS_IOCTL => sys_ioctl(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _) as _,
        SYS_WRITEV => sys_writev(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_SCHED_YIELD => sys_sched_yield() as isize,
        SYS_NANOSLEEP => sys_nanosleep(tf.arg0() as _, tf.arg1() as _) as _,
        SYS_GETPID => sys_getpid() as isize,
        SYS_EXIT => sys_exit(tf.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        SYS_ARCH_PRCTL => sys_arch_prctl(tf.arg0() as _, tf.arg1() as _),
        SYS_SET_TID_ADDRESS => sys_set_tid_address(tf.arg0() as _),
        SYS_CLOCK_GETTIME => sys_clock_gettime(tf.arg0() as _, tf.arg1() as _) as _,
        SYS_CLOCK_NANOSLEEP => sys_nanosleep(tf.arg0() as _, tf.arg1() as _) as _,
        SYS_EXITGROUP => sys_exit_group(tf.arg0() as _),
        _ => {
            warn!("Unimplemented syscall: {}", syscall_num);
            axtask::exit(LinuxError::ENOSYS as _)
        }
    };
    ret
}
