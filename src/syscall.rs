use axerrno::LinuxError;
use axhal::{
    arch::TrapFrame,
    trap::{SYSCALL, register_trap_handler},
};
use starry_api::*;
use starry_core::task::{time_stat_from_kernel_to_user, time_stat_from_user_to_kernel};
use syscalls::Sysno;

#[register_trap_handler(SYSCALL)]
fn handle_syscall(tf: &mut TrapFrame, syscall_num: usize) -> isize {
    let sysno = Sysno::from(syscall_num as u32);
    info!("Syscall {}", sysno);
    time_stat_from_user_to_kernel();
    let result = match sysno {
        Sysno::read => sys_read(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::write => sys_write(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::mmap => sys_mmap(
            tf.arg0().into(),
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
            tf.arg5() as _,
        ),
        Sysno::ioctl => sys_ioctl(tf.arg0() as _, tf.arg1() as _, tf.arg2().into()),
        Sysno::writev => sys_writev(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::sched_yield => sys_sched_yield(),
        Sysno::nanosleep => sys_nanosleep(tf.arg0().into(), tf.arg1().into()),
        Sysno::getpid => sys_getpid(),
        Sysno::getppid => sys_getppid(),
        Sysno::gettid => sys_gettid(),
        Sysno::exit => sys_exit(tf.arg0() as _),
        Sysno::exit_group => sys_exit_group(tf.arg0() as _),
        Sysno::gettimeofday => sys_get_time_of_day(tf.arg0().into()),
        Sysno::getcwd => sys_getcwd(tf.arg0().into(), tf.arg1() as _),
        Sysno::dup => sys_dup(tf.arg0() as _),
        Sysno::dup3 => sys_dup3(tf.arg0() as _, tf.arg1() as _),
        Sysno::fcntl => sys_fcntl(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::clone => sys_clone(
            tf,
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2(),
            tf.arg3(),
            tf.arg4(),
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::fork => sys_fork(tf),
        Sysno::wait4 => sys_waitpid(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::pipe2 => sys_pipe2(tf.arg0().into()),
        Sysno::close => sys_close(tf.arg0() as _),
        Sysno::chdir => sys_chdir(tf.arg0().into()),
        Sysno::mkdirat => sys_mkdirat(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::execve => sys_execve(tf.arg0().into(), tf.arg1().into(), tf.arg2().into()),
        Sysno::openat => sys_openat(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::open => sys_open(tf.arg0().into(), tf.arg1() as _, tf.arg2() as _),
        Sysno::getdents64 => sys_getdents64(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::linkat => sys_linkat(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3().into(),
            tf.arg4() as _,
        ),
        Sysno::unlinkat => sys_unlinkat(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::uname => sys_uname(tf.arg0().into()),
        Sysno::fstat => sys_fstat(tf.arg0() as _, tf.arg1().into()),
        Sysno::mount => sys_mount(
            tf.arg0().into(),
            tf.arg1().into(),
            tf.arg2().into(),
            tf.arg3() as _,
            tf.arg4().into(),
        ) as _,
        Sysno::umount2 => sys_umount2(tf.arg0().into(), tf.arg1() as _) as _,
        #[cfg(target_arch = "x86_64")]
        Sysno::newfstatat => sys_fstatat(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2().into(),
            tf.arg3() as _,
        ),
        #[cfg(not(target_arch = "x86_64"))]
        Sysno::fstatat => sys_fstatat(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2().into(),
            tf.arg3() as _,
        ),
        Sysno::statx => sys_statx(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4().into(),
        ),
        Sysno::munmap => sys_munmap(tf.arg0().into(), tf.arg1() as _),
        Sysno::mprotect => sys_mprotect(tf.arg0().into(), tf.arg1() as _, tf.arg2() as _),
        Sysno::times => sys_times(tf.arg0().into()),
        Sysno::brk => sys_brk(tf.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::arch_prctl => sys_arch_prctl(tf, tf.arg0() as _, tf.arg1() as _),
        Sysno::set_tid_address => sys_set_tid_address(tf.arg0()),
        Sysno::clock_gettime => sys_clock_gettime(tf.arg0() as _, tf.arg1().into()),
        Sysno::getuid => sys_getuid(),
        Sysno::rt_sigprocmask => sys_rt_sigprocmask(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2().into(),
            tf.arg3() as _,
        ),
        Sysno::rt_sigaction => sys_rt_sigaction(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2().into(),
            tf.arg3() as _,
        ),
        _ => {
            warn!("Unimplemented syscall: {}", sysno);
            Err(LinuxError::ENOSYS)
        }
    };
    let ans = result.unwrap_or_else(|err| -err.code() as _);
    time_stat_from_kernel_to_user();
    info!("Syscall {:?} return {}", sysno, ans);
    ans
}
