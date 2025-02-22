mod fs;
mod mm;
mod task;
mod utils;

use crate::task::{time_stat_from_kernel_to_user, time_stat_from_user_to_kernel};
use axerrno::LinuxError;
use axhal::{
    arch::TrapFrame,
    trap::{SYSCALL, register_trap_handler},
};
use syscalls::Sysno;

use self::fs::*;
use self::mm::*;
use self::task::*;
use self::utils::*;

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
    info!("Syscall {:?}", Sysno::from(syscall_num as u32));
    time_stat_from_user_to_kernel();
    let ans = match Sysno::from(syscall_num as u32) {
        Sysno::read => sys_read(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::write => sys_write(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::mmap => sys_mmap(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
            tf.arg5() as _,
        ) as _,
        Sysno::ioctl => sys_ioctl(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _) as _,
        Sysno::writev => sys_writev(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::sched_yield => sys_sched_yield() as isize,
        Sysno::nanosleep => sys_nanosleep(tf.arg0() as _, tf.arg1() as _) as _,
        Sysno::getpid => sys_getpid() as isize,
        Sysno::getppid => sys_getppid() as isize,
        Sysno::exit => sys_exit(tf.arg0() as _),
        Sysno::gettimeofday => sys_get_time_of_day(tf.arg0() as _) as _,
        Sysno::getcwd => sys_getcwd(tf.arg0() as _, tf.arg1() as _) as _,
        Sysno::dup => sys_dup(tf.arg0() as _) as _,
        Sysno::dup3 => sys_dup3(tf.arg0() as _, tf.arg1() as _) as _,
        Sysno::clone => sys_clone(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ) as _,
        Sysno::wait4 => sys_wait4(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _) as _,
        Sysno::pipe2 => sys_pipe2(tf.arg0() as _) as _,
        Sysno::close => sys_close(tf.arg0() as _) as _,
        Sysno::chdir => sys_chdir(tf.arg0() as _) as _,
        Sysno::mkdirat => sys_mkdirat(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _) as _,
        Sysno::execve => sys_execve(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _) as _,
        Sysno::openat => sys_openat(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ) as _,
        Sysno::getdents64 => sys_getdents64(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::linkat => sys_linkat(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ) as _,
        Sysno::unlinkat => sys_unlinkat(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::uname => sys_uname(tf.arg0() as _) as _,
        Sysno::fstat => sys_fstat(tf.arg0() as _, tf.arg1() as _) as _,
        Sysno::statx => sys_statx(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ) as _,
        Sysno::munmap => sys_munmap(tf.arg0() as _, tf.arg1() as _) as _,
        Sysno::times => sys_times(tf.arg0() as _) as _,
        Sysno::brk => sys_brk(tf.arg0() as _) as _,
        #[cfg(target_arch = "x86_64")]
        Sysno::arch_prctl => sys_arch_prctl(tf.arg0() as _, tf.arg1() as _),
        Sysno::set_tid_address => sys_set_tid_address(tf.arg0() as _),
        Sysno::clock_gettime => sys_clock_gettime(tf.arg0() as _, tf.arg1() as _) as _,
        Sysno::exit_group => sys_exit_group(tf.arg0() as _),
        _ => {
            warn!("Unimplemented syscall: {}", syscall_num);
            axtask::exit(LinuxError::ENOSYS as _)
        }
    };
    time_stat_from_kernel_to_user();
    info!("syscall return: {}", ans);
    ans
}
