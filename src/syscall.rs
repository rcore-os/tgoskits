use alloc::string::ToString;

use axerrno::{LinuxError, LinuxResult};
use axhal::{
    arch::TrapFrame,
    trap::{SYSCALL, register_trap_handler},
};
use starry_api::*;
use starry_core::task::{time_stat_from_kernel_to_user, time_stat_from_user_to_kernel};
use syscalls::Sysno;

fn handle_syscall_impl(tf: &mut TrapFrame, sysno: Sysno) -> LinuxResult<isize> {
    match sysno {
        // fs ctl
        Sysno::ioctl => sys_ioctl(tf.arg0() as _, tf.arg1() as _, tf.arg2().into()),
        Sysno::chdir => sys_chdir(tf.arg0().into()),
        Sysno::fchdir => sys_fchdir(tf.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::mkdir => sys_mkdir(tf.arg0().into(), tf.arg1() as _),
        Sysno::mkdirat => sys_mkdirat(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::getdents64 => sys_getdents64(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::link => sys_link(tf.arg0().into(), tf.arg1().into()),
        Sysno::linkat => sys_linkat(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3().into(),
            tf.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::rmdir => sys_rmdir(tf.arg0().into()),
        #[cfg(target_arch = "x86_64")]
        Sysno::unlink => sys_unlink(tf.arg0().into()),
        Sysno::unlinkat => sys_unlinkat(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::getcwd => sys_getcwd(tf.arg0().into(), tf.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::symlink => sys_symlink(tf.arg0().into(), tf.arg1().into()),
        Sysno::symlinkat => sys_symlinkat(tf.arg0().into(), tf.arg1() as _, tf.arg2().into()),
        #[cfg(target_arch = "x86_64")]
        Sysno::rename => sys_rename(tf.arg0().into(), tf.arg1().into()),
        Sysno::renameat => sys_renameat(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3().into(),
        ),
        Sysno::renameat2 => sys_renameat2(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3().into(),
            tf.arg4() as _,
        ),
        Sysno::sync => sys_sync(),
        Sysno::syncfs => sys_syncfs(tf.arg0() as _),

        // file ops
        Sysno::fchown => sys_fchown(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::fchownat => sys_fchownat(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::chmod => sys_chmod(tf.arg0().into(), tf.arg1() as _),
        Sysno::fchmod => sys_fchmod(tf.arg0() as _, tf.arg1() as _),
        Sysno::fchmodat | Sysno::fchmodat2 => sys_fchmodat(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::readlink => sys_readlink(tf.arg0().into(), tf.arg1().into(), tf.arg2() as _),
        Sysno::readlinkat => sys_readlinkat(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2().into(),
            tf.arg3() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::utime => sys_utime(tf.arg0().into(), tf.arg1().into()),
        #[cfg(target_arch = "x86_64")]
        Sysno::utimes => sys_utimes(tf.arg0().into(), tf.arg1().into()),
        Sysno::utimensat => sys_utimensat(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2().into(),
            tf.arg3() as _,
        ),

        // fd ops
        #[cfg(target_arch = "x86_64")]
        Sysno::open => sys_open(tf.arg0().into(), tf.arg1() as _, tf.arg2() as _),
        Sysno::openat => sys_openat(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::close => sys_close(tf.arg0() as _),
        Sysno::dup => sys_dup(tf.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::dup2 => sys_dup2(tf.arg0() as _, tf.arg1() as _),
        Sysno::dup3 => sys_dup3(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::fcntl => sys_fcntl(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::flock => sys_flock(tf.arg0() as _, tf.arg1() as _),

        // io
        Sysno::read => sys_read(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::readv => sys_readv(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::write => sys_write(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::writev => sys_writev(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::lseek => sys_lseek(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::truncate => sys_truncate(tf.arg0().into(), tf.arg1() as _),
        Sysno::ftruncate => sys_ftruncate(tf.arg0() as _, tf.arg1() as _),
        Sysno::fallocate => sys_fallocate(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::fsync => sys_fsync(tf.arg0() as _),
        Sysno::fdatasync => sys_fdatasync(tf.arg0() as _),
        Sysno::pread64 => sys_pread64(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::pwrite64 => sys_pwrite64(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::preadv => sys_preadv(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::pwritev => sys_pwritev(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::preadv2 => sys_preadv2(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ),
        Sysno::pwritev2 => sys_pwritev2(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ),
        Sysno::sendfile => sys_sendfile(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2().into(),
            tf.arg3() as _,
        ),
        Sysno::splice => sys_splice(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3().into(),
            tf.arg4() as _,
            tf.arg5() as _,
        ),

        // io mpx
        #[cfg(target_arch = "x86_64")]
        Sysno::poll => sys_poll(tf.arg0().into(), tf.arg1() as _, tf.arg2() as _),
        Sysno::ppoll => sys_ppoll(
            tf.arg0().into(),
            tf.arg1() as _,
            tf.arg2().into(),
            tf.arg3().into(),
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::select => sys_select(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2().into(),
            tf.arg3().into(),
            tf.arg4().into(),
        ),
        Sysno::pselect6 => sys_pselect6(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2().into(),
            tf.arg3().into(),
            tf.arg4().into(),
            tf.arg5().into(),
        ),

        // fs mount
        Sysno::mount => sys_mount(
            tf.arg0().into(),
            tf.arg1().into(),
            tf.arg2().into(),
            tf.arg3() as _,
            tf.arg4().into(),
        ) as _,
        Sysno::umount2 => sys_umount2(tf.arg0().into(), tf.arg1() as _) as _,

        // pipe
        Sysno::pipe2 => sys_pipe2(tf.arg0().into(), tf.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::pipe => sys_pipe2(tf.arg0().into(), 0),

        // fs stat
        #[cfg(target_arch = "x86_64")]
        Sysno::stat => sys_stat(tf.arg0().into(), tf.arg1().into()),
        Sysno::fstat => sys_fstat(tf.arg0() as _, tf.arg1().into()),
        #[cfg(target_arch = "x86_64")]
        Sysno::lstat => sys_lstat(tf.arg0().into(), tf.arg1().into()),
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
        #[cfg(target_arch = "x86_64")]
        Sysno::access => sys_access(tf.arg0().into(), tf.arg1() as _),
        Sysno::faccessat | Sysno::faccessat2 => sys_faccessat2(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::statfs => sys_statfs(tf.arg0().into(), tf.arg1().into()),
        Sysno::fstatfs => sys_fstatfs(tf.arg0() as _, tf.arg1().into()),

        // mm
        Sysno::brk => sys_brk(tf.arg0() as _),
        Sysno::mmap => sys_mmap(
            tf.arg0(),
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
            tf.arg5() as _,
        ),
        Sysno::munmap => sys_munmap(tf.arg0(), tf.arg1() as _),
        Sysno::mprotect => sys_mprotect(tf.arg0(), tf.arg1() as _, tf.arg2() as _),
        Sysno::madvise => sys_madvise(tf.arg0(), tf.arg1() as _, tf.arg2() as _),
        Sysno::msync => sys_msync(tf.arg0(), tf.arg1() as _, tf.arg2() as _),

        // task info
        Sysno::getpid => sys_getpid(),
        Sysno::getppid => sys_getppid(),
        Sysno::gettid => sys_gettid(),
        Sysno::getrusage => sys_getrusage(tf.arg0() as _, tf.arg1().into()),

        // task sched
        Sysno::sched_yield => sys_sched_yield(),
        Sysno::nanosleep => sys_nanosleep(tf.arg0().into(), tf.arg1().into()),
        Sysno::clock_nanosleep => sys_clock_nanosleep(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2().into(),
            tf.arg3().into(),
        ),
        Sysno::sched_getaffinity => {
            sys_sched_getaffinity(tf.arg0() as _, tf.arg1() as _, tf.arg2().into())
        }
        Sysno::sched_setaffinity => {
            sys_sched_setaffinity(tf.arg0() as _, tf.arg1() as _, tf.arg2().into())
        }
        Sysno::getpriority => sys_getpriority(tf.arg0() as _, tf.arg1() as _),

        // task ops
        Sysno::execve => sys_execve(tf, tf.arg0().into(), tf.arg1().into(), tf.arg2().into()),
        Sysno::set_tid_address => sys_set_tid_address(tf.arg0()),
        #[cfg(target_arch = "x86_64")]
        Sysno::arch_prctl => sys_arch_prctl(tf, tf.arg0() as _, tf.arg1() as _),
        Sysno::prlimit64 => sys_prlimit64(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2().into(),
            tf.arg3().into(),
        ),
        Sysno::capget => sys_capget(tf.arg0().into(), tf.arg1().into()),
        Sysno::capset => sys_capset(tf.arg0().into(), tf.arg1().into()),
        Sysno::umask => sys_umask(tf.arg0() as _),
        Sysno::setreuid => sys_setreuid(tf.arg0() as _, tf.arg1() as _),
        Sysno::setresuid => sys_setresuid(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::setresgid => sys_setresgid(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),

        // task management
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
        Sysno::exit => sys_exit(tf.arg0() as _),
        Sysno::exit_group => sys_exit_group(tf.arg0() as _),
        Sysno::wait4 => sys_waitpid(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::getsid => sys_getsid(tf.arg0() as _),
        Sysno::setsid => sys_setsid(),
        Sysno::getpgid => sys_getpgid(tf.arg0() as _),
        Sysno::setpgid => sys_setpgid(tf.arg0() as _, tf.arg1() as _),

        // signal
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
        Sysno::rt_sigpending => sys_rt_sigpending(tf.arg0().into(), tf.arg1() as _),
        Sysno::rt_sigreturn => sys_rt_sigreturn(tf),
        Sysno::rt_sigtimedwait => sys_rt_sigtimedwait(
            tf.arg0().into(),
            tf.arg1().into(),
            tf.arg2().into(),
            tf.arg3() as _,
        ),
        Sysno::rt_sigsuspend => sys_rt_sigsuspend(tf, tf.arg0().into(), tf.arg1() as _),
        Sysno::kill => sys_kill(tf.arg0() as _, tf.arg1() as _),
        Sysno::tkill => sys_tkill(tf.arg0() as _, tf.arg1() as _),
        Sysno::tgkill => sys_tgkill(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::rt_sigqueueinfo => sys_rt_sigqueueinfo(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2().into(),
            tf.arg3() as _,
        ),
        Sysno::rt_tgsigqueueinfo => sys_rt_tgsigqueueinfo(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3().into(),
            tf.arg4() as _,
        ),
        Sysno::sigaltstack => sys_sigaltstack(tf.arg0().into(), tf.arg1().into()),
        Sysno::futex => sys_futex(
            tf.arg0().into(),
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3().into(),
            tf.arg4().into(),
            tf.arg5() as _,
        ),
        Sysno::get_robust_list => {
            sys_get_robust_list(tf.arg0() as _, tf.arg1().into(), tf.arg2().into())
        }
        Sysno::set_robust_list => sys_set_robust_list(tf.arg0().into(), tf.arg1() as _),

        // sys
        Sysno::getuid => sys_getuid(),
        Sysno::geteuid => sys_geteuid(),
        Sysno::getgid => sys_getgid(),
        Sysno::getegid => sys_getegid(),
        Sysno::setuid => sys_setuid(tf.arg0() as _),
        Sysno::uname => sys_uname(tf.arg0().into()),
        Sysno::sysinfo => sys_sysinfo(tf.arg0().into()),
        Sysno::syslog => sys_syslog(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::getrandom => sys_getrandom(tf.arg0().into(), tf.arg1() as _, tf.arg2() as _),

        // time
        Sysno::gettimeofday => sys_gettimeofday(tf.arg0().into()),
        Sysno::times => sys_times(tf.arg0().into()),
        Sysno::clock_gettime => sys_clock_gettime(tf.arg0() as _, tf.arg1().into()),
        Sysno::clock_getres => sys_clock_getres(tf.arg0() as _, tf.arg1().into()),
        Sysno::getitimer => sys_getitimer(tf.arg0() as _, tf.arg1().into()),
        Sysno::setitimer => sys_setitimer(tf.arg0() as _, tf.arg1().into(), tf.arg2().into()),

        // shm
        Sysno::shmget => sys_shmget(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::shmat => sys_shmat(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::shmctl => sys_shmctl(tf.arg0() as _, tf.arg1() as _, tf.arg2().into()),
        Sysno::shmdt => sys_shmdt(tf.arg0() as _),

        // net
        Sysno::socket => sys_socket(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::bind => sys_bind(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::connect => sys_connect(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::getsockname => sys_getsockname(tf.arg0() as _, tf.arg1().into(), tf.arg2().into()),
        Sysno::getpeername => sys_getpeername(tf.arg0() as _, tf.arg1().into(), tf.arg2().into()),
        Sysno::listen => sys_listen(tf.arg0() as _, tf.arg1() as _),
        Sysno::accept => sys_accept(tf.arg0() as _, tf.arg1().into(), tf.arg2().into()),
        Sysno::accept4 => sys_accept4(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2().into(),
            tf.arg3() as _,
        ),
        Sysno::shutdown => sys_shutdown(tf.arg0() as _, tf.arg1() as _),
        Sysno::sendto => sys_sendto(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4().into(),
            tf.arg5() as _,
        ),
        Sysno::recvfrom => sys_recvfrom(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4().into(),
            tf.arg5().into(),
        ),
        Sysno::getsockopt => sys_getsockopt(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3().into(),
            tf.arg4().into(),
        ),
        Sysno::setsockopt => sys_setsockopt(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3().into(),
            tf.arg4() as _,
        ),

        _ => {
            warn!("Unimplemented syscall: {}", sysno);
            Err(LinuxError::ENOSYS)
        }
    }
}

#[register_trap_handler(SYSCALL)]
fn handle_syscall(tf: &mut TrapFrame, syscall_num: usize) -> isize {
    let sysno = Sysno::new(syscall_num);
    trace!("Syscall {:?}", sysno);

    time_stat_from_user_to_kernel();

    let result = sysno
        .ok_or(LinuxError::ENOSYS)
        .and_then(|sysno| handle_syscall_impl(tf, sysno));
    debug!(
        "Syscall {} return {:?}",
        sysno.map_or("(invalid)".to_string(), |s| s.to_string()),
        result
    );

    time_stat_from_kernel_to_user();
    result.unwrap_or_else(|err| -err.code() as _)
}
