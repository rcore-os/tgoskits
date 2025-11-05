mod fs;
mod io_mpx;
mod ipc;
mod mm;
mod net;
mod resources;
mod signal;
mod sync;
mod sys;
mod task;
mod time;

use axerrno::{AxError, LinuxError};
use axhal::uspace::UserContext;
use syscalls::Sysno;

use self::{
    fs::*, io_mpx::*, ipc::*, mm::*, net::*, resources::*, signal::*, sync::*, sys::*, task::*,
    time::*,
};

pub fn handle_syscall(uctx: &mut UserContext) {
    let Some(sysno) = Sysno::new(uctx.sysno()) else {
        warn!("Invalid syscall number: {}", uctx.sysno());
        uctx.set_retval(-LinuxError::ENOSYS.code() as _);
        return;
    };

    trace!("Syscall {sysno:?}");

    let result = match sysno {
        // fs ctl
        Sysno::ioctl => sys_ioctl(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::chdir => sys_chdir(uctx.arg0() as _),
        Sysno::fchdir => sys_fchdir(uctx.arg0() as _),
        Sysno::chroot => sys_chroot(uctx.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::mkdir => sys_mkdir(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::mkdirat => sys_mkdirat(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::getdents64 => sys_getdents64(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::link => sys_link(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::linkat => sys_linkat(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::rmdir => sys_rmdir(uctx.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::unlink => sys_unlink(uctx.arg0() as _),
        Sysno::unlinkat => sys_unlinkat(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::getcwd => sys_getcwd(uctx.arg0() as _, uctx.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::symlink => sys_symlink(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::symlinkat => sys_symlinkat(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::rename => sys_rename(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::renameat => sys_renameat(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::renameat2 => sys_renameat2(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::sync => sys_sync(),
        Sysno::syncfs => sys_syncfs(uctx.arg0() as _),

        // file ops
        #[cfg(target_arch = "x86_64")]
        Sysno::chown => sys_chown(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::lchown => sys_lchown(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::fchown => sys_fchown(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::fchownat => sys_fchownat(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::chmod => sys_chmod(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::fchmod => sys_fchmod(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::fchmodat | Sysno::fchmodat2 => sys_fchmodat(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::readlink => sys_readlink(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::readlinkat => sys_readlinkat(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::utime => sys_utime(uctx.arg0() as _, uctx.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::utimes => sys_utimes(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::utimensat => sys_utimensat(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),

        // fd ops
        #[cfg(target_arch = "x86_64")]
        Sysno::open => sys_open(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::openat => sys_openat(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::close => sys_close(uctx.arg0() as _),
        Sysno::close_range => sys_close_range(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::dup => sys_dup(uctx.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::dup2 => sys_dup2(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::dup3 => sys_dup3(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::fcntl => sys_fcntl(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::flock => sys_flock(uctx.arg0() as _, uctx.arg1() as _),

        // io
        Sysno::read => sys_read(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::readv => sys_readv(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::write => sys_write(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::writev => sys_writev(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::lseek => sys_lseek(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::truncate => sys_truncate(uctx.arg0().into(), uctx.arg1() as _),
        Sysno::ftruncate => sys_ftruncate(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::fallocate => sys_fallocate(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::fsync => sys_fsync(uctx.arg0() as _),
        Sysno::fdatasync => sys_fdatasync(uctx.arg0() as _),
        Sysno::fadvise64 => sys_fadvise64(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::pread64 => sys_pread64(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::pwrite64 => sys_pwrite64(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::preadv => sys_preadv(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::pwritev => sys_pwritev(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::preadv2 => sys_preadv2(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::pwritev2 => sys_pwritev2(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::sendfile => sys_sendfile(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::copy_file_range => sys_copy_file_range(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),
        Sysno::splice => sys_splice(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),

        // io mpx
        #[cfg(target_arch = "x86_64")]
        Sysno::poll => sys_poll(uctx.arg0().into(), uctx.arg1() as _, uctx.arg2() as _),
        Sysno::ppoll => sys_ppoll(
            uctx.arg0().into(),
            uctx.arg1() as _,
            uctx.arg2().into(),
            uctx.arg3().into(),
            uctx.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::select => sys_select(
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2().into(),
            uctx.arg3().into(),
            uctx.arg4().into(),
        ),
        Sysno::pselect6 => sys_pselect6(
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2().into(),
            uctx.arg3().into(),
            uctx.arg4().into(),
            uctx.arg5().into(),
        ),
        Sysno::epoll_create1 => sys_epoll_create1(uctx.arg0() as _),
        Sysno::epoll_ctl => sys_epoll_ctl(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3().into(),
        ),
        Sysno::epoll_pwait => sys_epoll_pwait(
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4().into(),
            uctx.arg5() as _,
        ),
        Sysno::epoll_pwait2 => sys_epoll_pwait2(
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2() as _,
            uctx.arg3().into(),
            uctx.arg4().into(),
            uctx.arg5() as _,
        ),

        // fs mount
        Sysno::mount => sys_mount(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ) as _,
        Sysno::umount2 => sys_umount2(uctx.arg0() as _, uctx.arg1() as _) as _,

        // pipe
        Sysno::pipe2 => sys_pipe2(uctx.arg0() as _, uctx.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::pipe => sys_pipe2(uctx.arg0() as _, 0),

        // event
        Sysno::eventfd2 => sys_eventfd2(uctx.arg0() as _, uctx.arg1() as _),

        // pidfd
        Sysno::pidfd_open => sys_pidfd_open(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::pidfd_getfd => sys_pidfd_getfd(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::pidfd_send_signal => sys_pidfd_send_signal(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),

        // memfd
        Sysno::memfd_create => sys_memfd_create(uctx.arg0().into(), uctx.arg1() as _),

        // fs stat
        #[cfg(target_arch = "x86_64")]
        Sysno::stat => sys_stat(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::fstat => sys_fstat(uctx.arg0() as _, uctx.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::lstat => sys_lstat(uctx.arg0() as _, uctx.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::newfstatat => sys_fstatat(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        #[cfg(not(target_arch = "x86_64"))]
        Sysno::fstatat => sys_fstatat(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::statx => sys_statx(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::access => sys_access(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::faccessat | Sysno::faccessat2 => sys_faccessat2(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::statfs => sys_statfs(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::fstatfs => sys_fstatfs(uctx.arg0() as _, uctx.arg1() as _),

        // mm
        Sysno::brk => sys_brk(uctx.arg0() as _),
        Sysno::mmap => sys_mmap(
            uctx.arg0(),
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),
        Sysno::munmap => sys_munmap(uctx.arg0(), uctx.arg1() as _),
        Sysno::mprotect => sys_mprotect(uctx.arg0(), uctx.arg1() as _, uctx.arg2() as _),
        Sysno::mremap => sys_mremap(
            uctx.arg0(),
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::madvise => sys_madvise(uctx.arg0(), uctx.arg1() as _, uctx.arg2() as _),
        Sysno::msync => sys_msync(uctx.arg0(), uctx.arg1() as _, uctx.arg2() as _),
        Sysno::mlock => sys_mlock(uctx.arg0(), uctx.arg1() as _),
        Sysno::mlock2 => sys_mlock2(uctx.arg0(), uctx.arg1() as _, uctx.arg2() as _),

        // task info
        Sysno::getpid => sys_getpid(),
        Sysno::getppid => sys_getppid(),
        Sysno::gettid => sys_gettid(),
        Sysno::getrusage => sys_getrusage(uctx.arg0() as _, uctx.arg1() as _),

        // task sched
        Sysno::sched_yield => sys_sched_yield(),
        Sysno::nanosleep => sys_nanosleep(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::clock_nanosleep => sys_clock_nanosleep(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::sched_getaffinity => {
            sys_sched_getaffinity(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _)
        }
        Sysno::sched_setaffinity => {
            sys_sched_setaffinity(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _)
        }
        Sysno::sched_getscheduler => sys_sched_getscheduler(uctx.arg0() as _),
        Sysno::sched_setscheduler => {
            sys_sched_setscheduler(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _)
        }
        Sysno::sched_getparam => sys_sched_getparam(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::getpriority => sys_getpriority(uctx.arg0() as _, uctx.arg1() as _),

        // task ops
        Sysno::execve => sys_execve(uctx, uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::set_tid_address => sys_set_tid_address(uctx.arg0()),
        #[cfg(target_arch = "x86_64")]
        Sysno::arch_prctl => sys_arch_prctl(uctx, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::prctl => sys_prctl(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::prlimit64 => sys_prlimit64(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::capget => sys_capget(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::capset => sys_capset(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::umask => sys_umask(uctx.arg0() as _),
        Sysno::setreuid => sys_setreuid(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::setresuid => sys_setresuid(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::setresgid => sys_setresgid(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::get_mempolicy => sys_get_mempolicy(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),

        // task management
        Sysno::clone => sys_clone(
            uctx,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2(),
            uctx.arg3(),
            uctx.arg4(),
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::fork => sys_fork(uctx),
        Sysno::exit => sys_exit(uctx.arg0() as _),
        Sysno::exit_group => sys_exit_group(uctx.arg0() as _),
        Sysno::wait4 => sys_waitpid(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::getsid => sys_getsid(uctx.arg0() as _),
        Sysno::setsid => sys_setsid(),
        Sysno::getpgid => sys_getpgid(uctx.arg0() as _),
        Sysno::setpgid => sys_setpgid(uctx.arg0() as _, uctx.arg1() as _),

        // signal
        Sysno::rt_sigprocmask => sys_rt_sigprocmask(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::rt_sigaction => sys_rt_sigaction(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::rt_sigpending => sys_rt_sigpending(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::rt_sigreturn => sys_rt_sigreturn(uctx),
        Sysno::rt_sigtimedwait => sys_rt_sigtimedwait(
            uctx,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::rt_sigsuspend => sys_rt_sigsuspend(uctx, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::kill => sys_kill(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::tkill => sys_tkill(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::tgkill => sys_tgkill(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::rt_sigqueueinfo => sys_rt_sigqueueinfo(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::rt_tgsigqueueinfo => sys_rt_tgsigqueueinfo(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::sigaltstack => sys_sigaltstack(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::futex => sys_futex(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),
        Sysno::get_robust_list => {
            sys_get_robust_list(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _)
        }
        Sysno::set_robust_list => sys_set_robust_list(uctx.arg0() as _, uctx.arg1() as _),

        // sys
        Sysno::getuid => sys_getuid(),
        Sysno::geteuid => sys_geteuid(),
        Sysno::getgid => sys_getgid(),
        Sysno::getegid => sys_getegid(),
        Sysno::setuid => sys_setuid(uctx.arg0() as _),
        Sysno::setgid => sys_setgid(uctx.arg0() as _),
        Sysno::getgroups => sys_getgroups(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::setgroups => sys_setgroups(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::uname => sys_uname(uctx.arg0() as _),
        Sysno::sysinfo => sys_sysinfo(uctx.arg0() as _),
        Sysno::syslog => sys_syslog(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::getrandom => sys_getrandom(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::seccomp => sys_seccomp(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        #[cfg(target_arch = "riscv64")]
        Sysno::riscv_flush_icache => sys_riscv_flush_icache(),

        // sync
        Sysno::membarrier => sys_membarrier(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),

        // time
        Sysno::gettimeofday => sys_gettimeofday(uctx.arg0() as _),
        Sysno::times => sys_times(uctx.arg0() as _),
        Sysno::clock_gettime => sys_clock_gettime(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::clock_getres => sys_clock_getres(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::getitimer => sys_getitimer(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::setitimer => sys_setitimer(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),

        // shm
        Sysno::shmget => sys_shmget(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::shmat => sys_shmat(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::shmctl => sys_shmctl(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2().into()),
        Sysno::shmdt => sys_shmdt(uctx.arg0() as _),

        // net
        Sysno::socket => sys_socket(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::socketpair => sys_socketpair(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3().into(),
        ),
        Sysno::bind => sys_bind(uctx.arg0() as _, uctx.arg1().into(), uctx.arg2() as _),
        Sysno::connect => sys_connect(uctx.arg0() as _, uctx.arg1().into(), uctx.arg2() as _),
        Sysno::getsockname => {
            sys_getsockname(uctx.arg0() as _, uctx.arg1().into(), uctx.arg2().into())
        }
        Sysno::getpeername => {
            sys_getpeername(uctx.arg0() as _, uctx.arg1().into(), uctx.arg2().into())
        }
        Sysno::listen => sys_listen(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::accept => sys_accept(uctx.arg0() as _, uctx.arg1().into(), uctx.arg2().into()),
        Sysno::accept4 => sys_accept4(
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2().into(),
            uctx.arg3() as _,
        ),
        Sysno::shutdown => sys_shutdown(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::sendto => sys_sendto(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4().into(),
            uctx.arg5() as _,
        ),
        Sysno::recvfrom => sys_recvfrom(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4().into(),
            uctx.arg5().into(),
        ),
        Sysno::sendmsg => sys_sendmsg(uctx.arg0() as _, uctx.arg1().into(), uctx.arg2() as _),
        Sysno::recvmsg => sys_recvmsg(uctx.arg0() as _, uctx.arg1().into(), uctx.arg2() as _),
        Sysno::getsockopt => sys_getsockopt(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3().into(),
            uctx.arg4().into(),
        ),
        Sysno::setsockopt => sys_setsockopt(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3().into(),
            uctx.arg4() as _,
        ),

        // dummy fds
        Sysno::signalfd4
        | Sysno::timerfd_create
        | Sysno::fanotify_init
        | Sysno::inotify_init1
        | Sysno::userfaultfd
        | Sysno::perf_event_open
        | Sysno::io_uring_setup
        | Sysno::bpf
        | Sysno::fsopen
        | Sysno::fspick
        | Sysno::open_tree
        | Sysno::memfd_secret => sys_dummy_fd(sysno),

        Sysno::timer_create | Sysno::timer_gettime | Sysno::timer_settime => Ok(0),

        _ => {
            warn!("Unimplemented syscall: {sysno}");
            Err(AxError::Unsupported)
        }
    };
    debug!("Syscall {sysno} return {result:?}");

    uctx.set_retval(result.unwrap_or_else(|err| -LinuxError::from(err).code() as _) as _);
}
