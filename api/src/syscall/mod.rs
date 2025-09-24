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
use axhal::context::TrapFrame;
use syscalls::Sysno;

use self::{
    fs::*, io_mpx::*, ipc::*, mm::*, net::*, resources::*, signal::*, sync::*, sys::*, task::*,
    time::*,
};

pub fn handle_syscall(tf: &mut TrapFrame) {
    let Some(sysno) = Sysno::new(tf.sysno()) else {
        warn!("Invalid syscall number: {}", tf.sysno());
        tf.set_retval(-LinuxError::ENOSYS.code() as _);
        return;
    };

    trace!("Syscall {:?}", sysno);

    let result = match sysno {
        // fs ctl
        Sysno::ioctl => sys_ioctl(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::chdir => sys_chdir(tf.arg0() as _),
        Sysno::fchdir => sys_fchdir(tf.arg0() as _),
        Sysno::chroot => sys_chroot(tf.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::mkdir => sys_mkdir(tf.arg0() as _, tf.arg1() as _),
        Sysno::mkdirat => sys_mkdirat(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::getdents64 => sys_getdents64(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::link => sys_link(tf.arg0() as _, tf.arg1() as _),
        Sysno::linkat => sys_linkat(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::rmdir => sys_rmdir(tf.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::unlink => sys_unlink(tf.arg0() as _),
        Sysno::unlinkat => sys_unlinkat(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::getcwd => sys_getcwd(tf.arg0() as _, tf.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::symlink => sys_symlink(tf.arg0() as _, tf.arg1() as _),
        Sysno::symlinkat => sys_symlinkat(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::rename => sys_rename(tf.arg0() as _, tf.arg1() as _),
        Sysno::renameat => sys_renameat(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::renameat2 => sys_renameat2(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ),
        Sysno::sync => sys_sync(),
        Sysno::syncfs => sys_syncfs(tf.arg0() as _),

        // file ops
        Sysno::fchown => sys_fchown(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::fchownat => sys_fchownat(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::chmod => sys_chmod(tf.arg0() as _, tf.arg1() as _),
        Sysno::fchmod => sys_fchmod(tf.arg0() as _, tf.arg1() as _),
        Sysno::fchmodat | Sysno::fchmodat2 => sys_fchmodat(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::readlink => sys_readlink(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::readlinkat => sys_readlinkat(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::utime => sys_utime(tf.arg0() as _, tf.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::utimes => sys_utimes(tf.arg0() as _, tf.arg1() as _),
        Sysno::utimensat => sys_utimensat(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),

        // fd ops
        #[cfg(target_arch = "x86_64")]
        Sysno::open => sys_open(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::openat => sys_openat(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::close => sys_close(tf.arg0() as _),
        Sysno::close_range => sys_close_range(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::dup => sys_dup(tf.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::dup2 => sys_dup2(tf.arg0() as _, tf.arg1() as _),
        Sysno::dup3 => sys_dup3(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::fcntl => sys_fcntl(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::flock => sys_flock(tf.arg0() as _, tf.arg1() as _),

        // io
        Sysno::read => sys_read(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::readv => sys_readv(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::write => sys_write(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::writev => sys_writev(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
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
        Sysno::fadvise64 => sys_fadvise64(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::pread64 => sys_pread64(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::pwrite64 => sys_pwrite64(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::preadv => sys_preadv(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::pwritev => sys_pwritev(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::preadv2 => sys_preadv2(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ),
        Sysno::pwritev2 => sys_pwritev2(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ),
        Sysno::sendfile => sys_sendfile(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::copy_file_range => sys_copy_file_range(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
            tf.arg5() as _,
        ),
        Sysno::splice => sys_splice(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
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
            tf.arg4() as _,
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
        Sysno::epoll_create1 => sys_epoll_create1(tf.arg0() as _),
        Sysno::epoll_ctl => sys_epoll_ctl(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3().into(),
        ),
        Sysno::epoll_pwait => sys_epoll_pwait(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4().into(),
            tf.arg5() as _,
        ),
        Sysno::epoll_pwait2 => sys_epoll_pwait2(
            tf.arg0() as _,
            tf.arg1().into(),
            tf.arg2() as _,
            tf.arg3().into(),
            tf.arg4().into(),
            tf.arg5() as _,
        ),

        // fs mount
        Sysno::mount => sys_mount(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ) as _,
        Sysno::umount2 => sys_umount2(tf.arg0() as _, tf.arg1() as _) as _,

        // pipe
        Sysno::pipe2 => sys_pipe2(tf.arg0() as _, tf.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::pipe => sys_pipe2(tf.arg0() as _, 0),

        // event
        Sysno::eventfd2 => sys_eventfd2(tf.arg0() as _, tf.arg1() as _),

        // pidfd
        Sysno::pidfd_open => sys_pidfd_open(tf.arg0() as _, tf.arg1() as _),
        Sysno::pidfd_getfd => sys_pidfd_getfd(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::pidfd_send_signal => sys_pidfd_send_signal(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),

        // memfd
        Sysno::memfd_create => sys_memfd_create(tf.arg0().into(), tf.arg1() as _),

        // fs stat
        #[cfg(target_arch = "x86_64")]
        Sysno::stat => sys_stat(tf.arg0() as _, tf.arg1() as _),
        Sysno::fstat => sys_fstat(tf.arg0() as _, tf.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::lstat => sys_lstat(tf.arg0() as _, tf.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::newfstatat => sys_fstatat(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        #[cfg(not(target_arch = "x86_64"))]
        Sysno::fstatat => sys_fstatat(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::statx => sys_statx(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::access => sys_access(tf.arg0() as _, tf.arg1() as _),
        Sysno::faccessat | Sysno::faccessat2 => sys_faccessat2(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::statfs => sys_statfs(tf.arg0() as _, tf.arg1() as _),
        Sysno::fstatfs => sys_fstatfs(tf.arg0() as _, tf.arg1() as _),

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
        Sysno::mremap => sys_mremap(tf.arg0(), tf.arg1() as _, tf.arg2() as _, tf.arg3() as _),
        Sysno::madvise => sys_madvise(tf.arg0(), tf.arg1() as _, tf.arg2() as _),
        Sysno::msync => sys_msync(tf.arg0(), tf.arg1() as _, tf.arg2() as _),
        Sysno::mlock => sys_mlock(tf.arg0(), tf.arg1() as _),
        Sysno::mlock2 => sys_mlock2(tf.arg0(), tf.arg1() as _, tf.arg2() as _),

        // task info
        Sysno::getpid => sys_getpid(),
        Sysno::getppid => sys_getppid(),
        Sysno::gettid => sys_gettid(),
        Sysno::getrusage => sys_getrusage(tf.arg0() as _, tf.arg1() as _),

        // task sched
        Sysno::sched_yield => sys_sched_yield(),
        Sysno::nanosleep => sys_nanosleep(tf.arg0() as _, tf.arg1() as _),
        Sysno::clock_nanosleep => sys_clock_nanosleep(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::sched_getaffinity => {
            sys_sched_getaffinity(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _)
        }
        Sysno::sched_setaffinity => {
            sys_sched_setaffinity(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _)
        }
        Sysno::sched_getscheduler => sys_sched_getscheduler(tf.arg0() as _),
        Sysno::sched_setscheduler => {
            sys_sched_setscheduler(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _)
        }
        Sysno::sched_getparam => sys_sched_getparam(tf.arg0() as _, tf.arg1() as _),
        Sysno::getpriority => sys_getpriority(tf.arg0() as _, tf.arg1() as _),

        // task ops
        Sysno::execve => sys_execve(tf, tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::set_tid_address => sys_set_tid_address(tf.arg0()),
        #[cfg(target_arch = "x86_64")]
        Sysno::arch_prctl => sys_arch_prctl(tf, tf.arg0() as _, tf.arg1() as _),
        Sysno::prctl => sys_prctl(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ),
        Sysno::prlimit64 => sys_prlimit64(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::capget => sys_capget(tf.arg0() as _, tf.arg1() as _),
        Sysno::capset => sys_capset(tf.arg0() as _, tf.arg1() as _),
        Sysno::umask => sys_umask(tf.arg0() as _),
        Sysno::setreuid => sys_setreuid(tf.arg0() as _, tf.arg1() as _),
        Sysno::setresuid => sys_setresuid(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::setresgid => sys_setresgid(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::get_mempolicy => sys_get_mempolicy(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ),

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
        Sysno::wait4 => sys_waitpid(tf, tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::getsid => sys_getsid(tf.arg0() as _),
        Sysno::setsid => sys_setsid(),
        Sysno::getpgid => sys_getpgid(tf.arg0() as _),
        Sysno::setpgid => sys_setpgid(tf.arg0() as _, tf.arg1() as _),

        // signal
        Sysno::rt_sigprocmask => sys_rt_sigprocmask(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::rt_sigaction => sys_rt_sigaction(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::rt_sigpending => sys_rt_sigpending(tf.arg0() as _, tf.arg1() as _),
        Sysno::rt_sigreturn => sys_rt_sigreturn(tf),
        Sysno::rt_sigtimedwait => sys_rt_sigtimedwait(
            tf,
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::rt_sigsuspend => sys_rt_sigsuspend(tf, tf.arg0() as _, tf.arg1() as _),
        Sysno::kill => sys_kill(tf.arg0() as _, tf.arg1() as _),
        Sysno::tkill => sys_tkill(tf.arg0() as _, tf.arg1() as _),
        Sysno::tgkill => sys_tgkill(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::rt_sigqueueinfo => sys_rt_sigqueueinfo(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        Sysno::rt_tgsigqueueinfo => sys_rt_tgsigqueueinfo(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
        ),
        Sysno::sigaltstack => sys_sigaltstack(tf.arg0() as _, tf.arg1() as _),
        Sysno::futex => sys_futex(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
            tf.arg5() as _,
        ),
        Sysno::get_robust_list => {
            sys_get_robust_list(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _)
        }
        Sysno::set_robust_list => sys_set_robust_list(tf.arg0() as _, tf.arg1() as _),

        // sys
        Sysno::getuid => sys_getuid(),
        Sysno::geteuid => sys_geteuid(),
        Sysno::getgid => sys_getgid(),
        Sysno::getegid => sys_getegid(),
        Sysno::setuid => sys_setuid(tf.arg0() as _),
        Sysno::setgid => sys_setgid(tf.arg0() as _),
        Sysno::getgroups => sys_getgroups(tf.arg0() as _, tf.arg1() as _),
        Sysno::setgroups => sys_setgroups(tf.arg0() as _, tf.arg1() as _),
        Sysno::uname => sys_uname(tf.arg0() as _),
        Sysno::sysinfo => sys_sysinfo(tf.arg0() as _),
        Sysno::syslog => sys_syslog(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::getrandom => sys_getrandom(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::seccomp => sys_seccomp(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        #[cfg(target_arch = "riscv64")]
        Sysno::riscv_flush_icache => sys_riscv_flush_icache(),

        // sync
        Sysno::membarrier => sys_membarrier(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),

        // time
        Sysno::gettimeofday => sys_gettimeofday(tf.arg0() as _),
        Sysno::times => sys_times(tf.arg0() as _),
        Sysno::clock_gettime => sys_clock_gettime(tf.arg0() as _, tf.arg1() as _),
        Sysno::clock_getres => sys_clock_getres(tf.arg0() as _, tf.arg1() as _),
        Sysno::getitimer => sys_getitimer(tf.arg0() as _, tf.arg1() as _),
        Sysno::setitimer => sys_setitimer(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),

        // shm
        Sysno::shmget => sys_shmget(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::shmat => sys_shmat(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::shmctl => sys_shmctl(tf.arg0() as _, tf.arg1() as _, tf.arg2().into()),
        Sysno::shmdt => sys_shmdt(tf.arg0() as _),

        // net
        Sysno::socket => sys_socket(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        Sysno::socketpair => sys_socketpair(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3().into(),
        ),
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
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4().into(),
            tf.arg5() as _,
        ),
        Sysno::recvfrom => sys_recvfrom(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4().into(),
            tf.arg5().into(),
        ),
        Sysno::sendmsg => sys_sendmsg(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
        Sysno::recvmsg => sys_recvmsg(tf.arg0() as _, tf.arg1().into(), tf.arg2() as _),
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
            warn!("Unimplemented syscall: {}", sysno);
            Err(AxError::Unsupported)
        }
    };
    debug!("Syscall {} return {:?}", sysno, result);

    tf.set_retval(result.unwrap_or_else(|err| -LinuxError::from(err).code() as _) as _);
}
