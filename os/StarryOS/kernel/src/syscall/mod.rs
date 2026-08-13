mod fs;
mod io_mpx;
mod ipc;
mod kmod;
mod mm;
mod net;
mod ns;
mod resources;
mod signal;
mod sync;
mod sys;
mod task;
mod time;

use ax_errno::{AxError, LinuxError};
use ax_runtime::hal::cpu::uspace::UserContext;
use starry_signal::Signo;
use syscalls::Sysno;

pub use self::{
    fs::*, io_mpx::*, ipc::*, mm::*, net::*, resources::*, signal::*, sync::*, sys::*, task::*,
    time::*,
};
use crate::task::{SeccompDecision, UserTaskRef, do_exit, seccomp_errno};

pub fn syscall_allows_signal_restart(sysno: usize) -> bool {
    // Linux never restarts fd-multiplexing waits or System V message-queue
    // blocking calls, even when the delivered handler uses SA_RESTART. Keep
    // the classification here because signal delivery only sees the syscall
    // number and the interrupted -EINTR result.
    let Some(sysno) = Sysno::new(sysno) else {
        return true;
    };

    if matches!(
        sysno,
        Sysno::ppoll
            | Sysno::pselect6
            | Sysno::epoll_pwait
            | Sysno::epoll_pwait2
            | Sysno::msgsnd
            | Sysno::msgrcv
    ) {
        return false;
    }

    // The legacy multiplexing entry points exist in the x86_64 syscall table
    // but not in the generic tables used by riscv64, aarch64, and loongarch64.
    #[cfg(target_arch = "x86_64")]
    if matches!(sysno, Sysno::poll | Sysno::select | Sysno::epoll_wait) {
        return false;
    }

    true
}

// `#[inline(never)]` keeps `sysno` reachable as a real call target so a kprobe
// planted at its symbol actually fires; its first-argument register also holds
// the raw syscall id, letting a `profile`-style eBPF demo read the syscall
// number directly off the probed register. In release builds LLVM would
// otherwise inline it into `handle_syscall` and the planted `int3` would land
// on a copy that never executes, so the probe would never trigger.
#[inline(never)]
pub fn sysno(id: usize) -> Option<Sysno> {
    let Some(sysno) = Sysno::new(id) else {
        warn!("Invalid syscall number: {}", id);
        return None;
    };
    Some(sysno)
}

/// Dispatches one syscall with the scheduler-owned current task capability.
///
/// The user-thread entry keeps this strong reference alive for its entire
/// execution loop. Syscall helpers borrow the same capability across blocking
/// operations instead of reacquiring `current` through the mutable runqueue
/// owner. This mirrors Linux's syscall-local use of `current` while preserving
/// the Rust lifetime that pins the Starry extension and scheduler record.
pub fn handle_syscall(current: &UserTaskRef, uctx: &mut UserContext) {
    let thread = current.as_thread();
    let Some(sysno) = sysno(uctx.sysno()) else {
        uctx.set_retval(-LinuxError::ENOSYS.code() as _);
        return;
    };
    trace!("Syscall {sysno:?}");
    match thread.evaluate_seccomp(uctx) {
        SeccompDecision::Allow => {}
        SeccompDecision::Errno(errno) => {
            uctx.set_retval(seccomp_errno(errno));
            return;
        }
        SeccompDecision::KillProcess => {
            do_exit(Signo::SIGSYS as i32, true);
            return;
        }
        SeccompDecision::KillThread => {
            do_exit(Signo::SIGSYS as i32, false);
            return;
        }
        SeccompDecision::UnsupportedAction => {
            uctx.set_retval(-LinuxError::ENOSYS.code() as usize);
            return;
        }
    }

    // Snapshot sepc before dispatching: if a signal handler is installed
    // during the syscall, the handler redirects uctx.ip() elsewhere.
    // We must not overwrite retval when that happens, because on
    // non-x86_64 arches retval and arg0 (signo) share a register.
    let prev_ip = uctx.ip();

    let result = match sysno {
        // fs ctl
        Sysno::ioctl => sys_ioctl(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::chdir => sys_chdir(current, uctx.arg0() as _),
        Sysno::fchdir => sys_fchdir(uctx.arg0() as _),
        Sysno::chroot => sys_chroot(current, uctx.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::mkdir => sys_mkdir(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::mkdirat => sys_mkdirat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::mknod => sys_mknod(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::mknodat => sys_mknodat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::getdents64 => sys_getdents64(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::link => sys_link(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::linkat => sys_linkat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::rmdir => sys_rmdir(current, uctx.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::unlink => sys_unlink(current, uctx.arg0() as _),
        Sysno::unlinkat => sys_unlinkat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::getcwd => sys_getcwd(current, uctx.arg0() as _, uctx.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::symlink => sys_symlink(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::symlinkat => sys_symlinkat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::rename => sys_rename(current, uctx.arg0() as _, uctx.arg1() as _),
        #[cfg(not(target_arch = "riscv64"))]
        Sysno::renameat => sys_renameat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::renameat2 => sys_renameat2(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::sync => sys_sync(),
        Sysno::syncfs => sys_syncfs(uctx.arg0() as _),

        // xattr stubs — rsext4 has no extended attributes, return empty/ENODATA/EOPNOTSUPP
        Sysno::listxattr => sys_listxattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::llistxattr => sys_llistxattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::flistxattr => sys_flistxattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::getxattr => sys_getxattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::lgetxattr => sys_lgetxattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::fgetxattr => sys_fgetxattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::setxattr => sys_setxattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::lsetxattr => sys_lsetxattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::fsetxattr => sys_fsetxattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::removexattr => sys_removexattr(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::lremovexattr => sys_lremovexattr(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::fremovexattr => sys_fremovexattr(current, uctx.arg0() as _, uctx.arg1() as _),

        // file ops
        #[cfg(target_arch = "x86_64")]
        Sysno::chown => sys_chown(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::lchown => sys_lchown(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::fchown => sys_fchown(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::fchownat => sys_fchownat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::chmod => sys_chmod(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::fchmod => sys_fchmod(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::fchmodat => sys_fchmodat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            0,
        ),
        Sysno::fchmodat2 => sys_fchmodat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::readlink => sys_readlink(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::readlinkat => sys_readlinkat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::utime => sys_utime(current, uctx.arg0() as _, uctx.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::utimes => sys_utimes(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::utimensat => sys_utimensat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),

        // fd ops
        #[cfg(target_arch = "x86_64")]
        Sysno::open => sys_open(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::creat => sys_creat(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::openat => sys_openat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::openat2 => sys_openat2(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::close => sys_close(uctx.arg0() as _),
        Sysno::close_range => sys_close_range(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::dup => sys_dup(uctx.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::dup2 => sys_dup2(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::dup3 => sys_dup3(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::fcntl => sys_fcntl(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::flock => sys_flock(current, uctx.arg0() as _, uctx.arg1() as _),

        // io
        Sysno::read => sys_read(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::readv => sys_readv(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::write => sys_write(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::writev => sys_writev(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::lseek => sys_lseek(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _),
        Sysno::truncate => sys_truncate(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::ftruncate => sys_ftruncate(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::fallocate => sys_fallocate(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::fsync => sys_fsync(uctx.arg0() as _),
        Sysno::fdatasync => sys_fdatasync(uctx.arg0() as _),
        Sysno::sync_file_range => sys_sync_file_range(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::fadvise64 => sys_fadvise64(
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::pread64 => sys_pread64(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::pwrite64 => sys_pwrite64(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::preadv => sys_preadv(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4(),
        ),
        Sysno::pwritev => sys_pwritev(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4(),
        ),
        // Kernel ABI: SYSCALL_DEFINE6(preadv2, fd, vec, vlen, pos_l, pos_h, flags)
        // arg4 is pos_h (high 32 bits of offset); flags is arg5.
        Sysno::preadv2 => sys_preadv2(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),
        Sysno::pwritev2 => sys_pwritev2(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),
        Sysno::process_vm_readv => sys_process_vm_readv(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),
        Sysno::process_vm_writev => sys_process_vm_writev(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),
        Sysno::io_setup => sys_io_setup(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::io_destroy => sys_io_destroy(current, uctx.arg0() as _),
        Sysno::io_submit => sys_io_submit(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::io_getevents => sys_io_getevents(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::io_pgetevents => sys_io_pgetevents(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5(),
        ),
        Sysno::io_cancel => sys_io_cancel(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::io_uring_setup => sys_io_uring_setup(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::io_uring_enter => sys_io_uring_enter(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4(),
            uctx.arg5(),
        ),
        Sysno::io_uring_register => sys_io_uring_register(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2(),
            uctx.arg3() as _,
        ),
        Sysno::sendfile => sys_sendfile(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::copy_file_range => sys_copy_file_range(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),
        Sysno::splice => sys_splice(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),

        // io mpx
        #[cfg(target_arch = "x86_64")]
        Sysno::pause => sys_ppoll(current, 0usize.into(), 0, 0usize.into(), 0usize.into(), 0),
        #[cfg(target_arch = "x86_64")]
        Sysno::poll => sys_poll(
            current,
            uctx.arg0().into(),
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::ppoll => sys_ppoll(
            current,
            uctx.arg0().into(),
            uctx.arg1() as _,
            uctx.arg2().into(),
            uctx.arg3().into(),
            uctx.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::select => sys_select(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2().into(),
            uctx.arg3().into(),
            uctx.arg4().into(),
        ),
        Sysno::pselect6 => sys_pselect6(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2().into(),
            uctx.arg3().into(),
            uctx.arg4().into(),
            uctx.arg5().into(),
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::epoll_create => sys_epoll_create(uctx.arg0() as _),
        Sysno::epoll_create1 => sys_epoll_create1(uctx.arg0() as _),
        Sysno::epoll_ctl => sys_epoll_ctl(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3().into(),
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::epoll_wait => sys_epoll_wait(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::epoll_pwait => sys_epoll_pwait(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4().into(),
            uctx.arg5() as _,
        ),
        Sysno::epoll_pwait2 => sys_epoll_pwait2(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2() as _,
            uctx.arg3().into(),
            uctx.arg4().into(),
            uctx.arg5() as _,
        ),

        // fs mount
        Sysno::mount => sys_mount(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ) as _,
        Sysno::umount2 => sys_umount2(current, uctx.arg0() as _, uctx.arg1() as _) as _,
        Sysno::pivot_root => sys_pivot_root(current, uctx.arg0() as _, uctx.arg1() as _) as _,
        Sysno::fsopen => sys_fsopen(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::fsconfig => sys_fsconfig(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::fsmount => sys_fsmount(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::move_mount => sys_move_mount(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::mount_setattr => sys_mount_setattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),

        // pipe
        Sysno::pipe2 => sys_pipe2(current, uctx.arg0() as _, uctx.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::pipe => sys_pipe2(current, uctx.arg0() as _, 0),

        // event
        Sysno::eventfd2 => sys_eventfd2(uctx.arg0() as _, uctx.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::eventfd => sys_eventfd(uctx.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::inotify_init => sys_inotify_init1(0),
        Sysno::inotify_init1 => sys_inotify_init1(uctx.arg0() as _),
        Sysno::inotify_add_watch => sys_inotify_add_watch(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::inotify_rm_watch => sys_inotify_rm_watch(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::timerfd_create => sys_timerfd_create(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::timerfd_settime => sys_timerfd_settime(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::timerfd_gettime => sys_timerfd_gettime(current, uctx.arg0() as _, uctx.arg1() as _),

        // pidfd
        Sysno::pidfd_open => sys_pidfd_open(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::pidfd_getfd => sys_pidfd_getfd(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::pidfd_send_signal => sys_pidfd_send_signal(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),

        // memfd
        Sysno::memfd_create => sys_memfd_create(current, uctx.arg0() as _, uctx.arg1() as _),

        // fs stat
        #[cfg(target_arch = "x86_64")]
        Sysno::stat => sys_stat(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::fstat => sys_fstat(current, uctx.arg0() as _, uctx.arg1() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::lstat => sys_lstat(current, uctx.arg0() as _, uctx.arg1() as _),
        #[cfg(any(target_arch = "x86_64", target_arch = "riscv64"))]
        Sysno::newfstatat => sys_fstatat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        #[cfg(not(any(target_arch = "x86_64", target_arch = "riscv64")))]
        Sysno::fstatat => sys_fstatat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::statx => sys_statx(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::access => sys_access(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::faccessat => sys_faccessat2(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            0,
        ),
        Sysno::faccessat2 => sys_faccessat2(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::statfs => sys_statfs(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::fstatfs => sys_fstatfs(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::name_to_handle_at => sys_name_to_handle_at(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::open_by_handle_at => Err(AxError::OperationNotSupported),

        // mm
        Sysno::brk => sys_brk(current, uctx.arg0() as _),
        Sysno::mmap => sys_mmap(
            current,
            uctx.arg0(),
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),
        Sysno::munmap => sys_munmap(current, uctx.arg0(), uctx.arg1() as _),
        Sysno::mprotect => sys_mprotect(current, uctx.arg0(), uctx.arg1() as _, uctx.arg2() as _),
        Sysno::mincore => sys_mincore(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::mremap => sys_mremap(
            current,
            uctx.arg0(),
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4(),
        ),
        Sysno::madvise => sys_madvise(current, uctx.arg0(), uctx.arg1() as _, uctx.arg2() as _),
        Sysno::msync => sys_msync(current, uctx.arg0(), uctx.arg1() as _, uctx.arg2() as _),
        Sysno::mlock => sys_mlock(current, uctx.arg0(), uctx.arg1() as _),
        Sysno::mlock2 => sys_mlock2(current, uctx.arg0(), uctx.arg1() as _, uctx.arg2() as _),

        // task info
        Sysno::getpid => sys_getpid(current),
        Sysno::getppid => sys_getppid(current),
        Sysno::gettid => sys_gettid(current),
        Sysno::getrusage => sys_getrusage(current, uctx.arg0() as _, uctx.arg1() as _),

        // task sched
        Sysno::sched_yield => sys_sched_yield(),
        Sysno::nanosleep => sys_nanosleep(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::clock_nanosleep => sys_clock_nanosleep(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::sched_getaffinity => sys_sched_getaffinity(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::sched_setaffinity => sys_sched_setaffinity(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::sched_getscheduler => sys_sched_getscheduler(current, uctx.arg0() as _),
        Sysno::sched_setscheduler => sys_sched_setscheduler(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::sched_getparam => sys_sched_getparam(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::sched_setparam => sys_sched_setparam(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::sched_get_priority_min => sys_sched_get_priority_min(uctx.arg0() as _),
        Sysno::sched_get_priority_max => sys_sched_get_priority_max(uctx.arg0() as _),
        Sysno::sched_rr_get_interval => {
            sys_sched_rr_get_interval(current, uctx.arg0() as _, uctx.arg1() as _)
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
        Sysno::sched_rr_get_interval_time64 => {
            sys_sched_rr_get_interval_time64(current, uctx.arg0() as _, uctx.arg1() as _)
        }
        Sysno::sched_setattr => sys_sched_setattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::sched_getattr => sys_sched_getattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::getpriority => sys_getpriority(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::setpriority => sys_setpriority(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),

        // task ops
        Sysno::execve => sys_execve(
            current,
            uctx,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::execveat => sys_execveat(
            current,
            uctx,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::set_tid_address => sys_set_tid_address(current, uctx.arg0()),
        Sysno::getcpu => sys_getcpu(current, uctx.arg0() as _, uctx.arg1() as _, uctx.arg2()),
        #[cfg(target_arch = "x86_64")]
        Sysno::arch_prctl => sys_arch_prctl(current, uctx, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::prctl => sys_prctl(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::prlimit64 => sys_prlimit64(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        // Legacy getrlimit/setrlimit -> prlimit64. The syscalls crate defines these
        // numbers on all four arches (x86_64 #97/#160; riscv64/aarch64/loongarch64
        // #163/#164). They are load-bearing on riscv64/x86_64 (which keep
        // __ARCH_WANT_SET_GET_RLIMIT, so glibc/Go issue the legacy call); on
        // aarch64/loongarch64 stock Linux is asm-generic and returns ENOSYS there
        // (libc uses prlimit64 only), so this arm is a harmless, more-permissive
        // superset. `struct rlimit` is two `unsigned long` == two u64 on every
        // 64-bit arch, layout-identical to `rlimit64`, so route through prlimit64
        // with pid=0 (== current process). Go's syscall package invokes the legacy
        // getrlimit directly (consul on riscv64 aborts with ENOSYS otherwise).
        Sysno::getrlimit => sys_prlimit64(
            current,
            0,
            uctx.arg0() as _,
            core::ptr::null(),
            uctx.arg1() as _,
        ),
        Sysno::setrlimit => sys_prlimit64(
            current,
            0,
            uctx.arg0() as _,
            uctx.arg1() as _,
            core::ptr::null_mut(),
        ),
        Sysno::capget => sys_capget(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::capset => sys_capset(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::umask => sys_umask(current, uctx.arg0() as _),
        Sysno::personality => sys_personality(current, uctx.arg0()),
        Sysno::setreuid => sys_setreuid(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::setregid => sys_setregid(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::setresuid => sys_setresuid(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::setresgid => sys_setresgid(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::get_mempolicy => sys_get_mempolicy(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::set_mempolicy => sys_set_mempolicy(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::mbind => sys_mbind(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),

        // task management
        Sysno::clone => sys_clone(
            current,
            uctx,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2(),
            uctx.arg3(),
            uctx.arg4(),
        ),
        Sysno::clone3 => sys_clone3(
            current,
            uctx,
            uctx.arg0() as _, // args_ptr
            uctx.arg1() as _, // args_size
        ),
        #[cfg(target_arch = "x86_64")]
        Sysno::fork => sys_fork(current, uctx),
        #[cfg(target_arch = "x86_64")]
        Sysno::vfork => sys_vfork(current, uctx),
        Sysno::unshare => sys_unshare(current, uctx.arg0() as _),
        Sysno::setns => sys_setns(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::exit => sys_exit(uctx.arg0() as _),
        Sysno::exit_group => sys_exit_group(uctx.arg0() as _),
        Sysno::wait4 => sys_waitpid(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::waitid => sys_waitid(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::ptrace => sys_ptrace(
            current,
            uctx.arg0() as _,
            uctx.arg1(),
            uctx.arg2(),
            uctx.arg3(),
        ),
        Sysno::getsid => sys_getsid(current, uctx.arg0() as _),
        Sysno::setsid => sys_setsid(current),
        Sysno::getpgid => sys_getpgid(current, uctx.arg0() as _),
        #[cfg(target_arch = "x86_64")]
        Sysno::getpgrp => sys_getpgrp(current),
        Sysno::setpgid => sys_setpgid(current, uctx.arg0() as _, uctx.arg1() as _),

        // signal
        Sysno::rt_sigprocmask => sys_rt_sigprocmask(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::rt_sigaction => sys_rt_sigaction(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::rt_sigpending => sys_rt_sigpending(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::rt_sigreturn => sys_rt_sigreturn(current, uctx),
        Sysno::rt_sigtimedwait => sys_rt_sigtimedwait(
            current,
            uctx,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::rt_sigsuspend => {
            sys_rt_sigsuspend(current, uctx, uctx.arg0() as _, uctx.arg1() as _)
        }
        Sysno::kill => sys_kill(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::tkill => sys_tkill(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::tgkill => sys_tgkill(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::rt_sigqueueinfo => sys_rt_sigqueueinfo(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::rt_tgsigqueueinfo => sys_rt_tgsigqueueinfo(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::sigaltstack => sys_sigaltstack(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::futex => sys_futex(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
            uctx.arg5() as _,
        ),
        Sysno::get_robust_list => sys_get_robust_list(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::set_robust_list => sys_set_robust_list(current, uctx.arg0() as _, uctx.arg1() as _),

        // sys
        Sysno::getuid => sys_getuid(current),
        Sysno::geteuid => sys_geteuid(current),
        Sysno::getgid => sys_getgid(current),
        Sysno::getegid => sys_getegid(current),
        Sysno::setuid => sys_setuid(current, uctx.arg0() as _),
        Sysno::setgid => sys_setgid(current, uctx.arg0() as _),
        Sysno::getresuid => sys_getresuid(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::getresgid => sys_getresgid(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::getgroups => sys_getgroups(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::setgroups => sys_setgroups(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::setfsuid => sys_setfsuid(current, uctx.arg0() as _),
        Sysno::setfsgid => sys_setfsgid(current, uctx.arg0() as _),
        Sysno::uname => sys_uname(current, uctx.arg0() as _),
        Sysno::sethostname => sys_sethostname(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::setdomainname => sys_setdomainname(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::sysinfo => sys_sysinfo(current, uctx.arg0() as _),
        Sysno::syslog => sys_syslog(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::reboot => sys_reboot(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3(),
        ),
        Sysno::getrandom => sys_getrandom(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::seccomp => sys_seccomp(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        #[cfg(target_arch = "riscv64")]
        Sysno::riscv_flush_icache => sys_riscv_flush_icache(uctx.arg0(), uctx.arg1(), uctx.arg2()),
        #[cfg(target_arch = "riscv64")]
        Sysno::riscv_hwprobe => sys_riscv_hwprobe(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),

        // sync
        Sysno::membarrier => sys_membarrier(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::rseq => sys_rseq(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),

        // time
        #[cfg(target_arch = "x86_64")]
        Sysno::time => sys_time(current, uctx.arg0() as _),
        Sysno::gettimeofday => sys_gettimeofday(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::times => sys_times(current, uctx.arg0() as _),
        Sysno::clock_gettime => sys_clock_gettime(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::clock_getres => sys_clock_getres(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::getitimer => sys_getitimer(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::setitimer => sys_setitimer(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),

        // msg
        Sysno::msgget => sys_msgget(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::msgsnd => sys_msgsnd(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::msgrcv => sys_msgrcv(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::msgctl => sys_msgctl(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),

        // POSIX message queues
        Sysno::mq_open => sys_mq_open(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::mq_unlink => sys_mq_unlink(current, uctx.arg0() as _),
        Sysno::mq_timedsend => sys_mq_timedsend(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::mq_timedreceive => sys_mq_timedreceive(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::mq_notify => sys_mq_notify(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::mq_getsetattr => sys_mq_getsetattr(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),

        // shm
        Sysno::shmget => sys_shmget(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::shmat => sys_shmat(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::shmctl => sys_shmctl(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2().into(),
        ),
        Sysno::shmdt => sys_shmdt(current, uctx.arg0() as _),

        // net
        Sysno::socket => sys_socket(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::socketpair => sys_socketpair(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3().into(),
        ),
        Sysno::bind => sys_bind(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2() as _,
        ),
        Sysno::connect => sys_connect(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2() as _,
        ),
        Sysno::getsockname => sys_getsockname(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2().into(),
        ),
        Sysno::getpeername => sys_getpeername(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2().into(),
        ),
        Sysno::listen => sys_listen(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::accept => sys_accept(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2().into(),
        ),
        Sysno::accept4 => sys_accept4(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2().into(),
            uctx.arg3() as _,
        ),
        Sysno::shutdown => sys_shutdown(uctx.arg0() as _, uctx.arg1() as _),
        Sysno::sendto => sys_sendto(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4().into(),
            uctx.arg5() as _,
        ),
        Sysno::recvfrom => sys_recvfrom(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4().into(),
            uctx.arg5().into(),
        ),
        Sysno::sendmsg => sys_sendmsg(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2() as _,
        ),
        Sysno::recvmsg => sys_recvmsg(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2() as _,
        ),
        Sysno::sendmmsg => sys_sendmmsg(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::recvmmsg => sys_recvmmsg(
            current,
            uctx.arg0() as _,
            uctx.arg1().into(),
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4().into(),
        ),
        Sysno::getsockopt => sys_getsockopt(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3().into(),
            uctx.arg4().into(),
        ),
        Sysno::setsockopt => sys_setsockopt(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3().into(),
            uctx.arg4() as _,
        ),

        // signal file descriptors
        Sysno::signalfd4 => sys_signalfd4(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2(),
            uctx.arg3() as _,
        ),

        // fspick/open_tree remain unsupported. Report ENOSYS instead of a
        // dummy fd so callers can select their classic-mount fallback.
        Sysno::fspick | Sysno::open_tree => Err(AxError::Unsupported),

        // dummy fds
        Sysno::userfaultfd | Sysno::memfd_secret => sys_dummy_fd(current, sysno),

        Sysno::bpf => {
            crate::ebpf::sys_bpf(current, uctx.arg0() as _, uctx.arg1(), uctx.arg2() as _)
        }
        Sysno::perf_event_open => crate::perf::sys_perf_event_open(
            current,
            uctx.arg0(),
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
            uctx.arg4() as _,
        ),
        Sysno::init_module => kmod::sys_init_module(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::finit_module => kmod::sys_finit_module(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::delete_module => {
            kmod::sys_delete_module(current, uctx.arg0() as _, uctx.arg1() as _)
        }

        Sysno::fanotify_init => Err(AxError::Unsupported),

        Sysno::timer_create => sys_timer_create(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
        ),
        Sysno::timer_settime => sys_timer_settime(
            current,
            uctx.arg0() as _,
            uctx.arg1() as _,
            uctx.arg2() as _,
            uctx.arg3() as _,
        ),
        Sysno::timer_gettime => sys_timer_gettime(current, uctx.arg0() as _, uctx.arg1() as _),
        Sysno::timer_delete => sys_timer_delete(current, uctx.arg0() as _),

        _ => {
            let tid = current.as_thread().tid();
            warn!("Unimplemented syscall: {sysno} (tid={tid})");
            Err(AxError::Unsupported)
        }
    };
    debug!("Syscall {sysno} return {result:?}");
    let new_retval = result.unwrap_or_else(|err| -LinuxError::from(err).code() as _) as _;

    if uctx.ip() == prev_ip {
        uctx.set_retval(new_retval);
    }
}

#[cfg(axtest)]
pub(crate) fn task_clone_validation_rules_hold_for_test() -> bool {
    task::clone_validation_rules_hold_for_test()
}

#[cfg(feature = "axtest")]
const _: fn(&crate::task::UserTaskRef, &mut UserContext) = handle_syscall;

#[cfg(feature = "axtest")]
const _: fn(
    &crate::task::UserTaskRef,
    *const u32,
    u32,
    u32,
    *const linux_raw_sys::general::timespec,
    *mut u32,
    u32,
) -> ax_errno::AxResult<isize> = sys_futex;

#[cfg(axtest)]
pub(crate) fn capability_data_conversion_rules_hold_for_test() -> bool {
    task::capability_data_conversion_rules_hold_for_test()
}

#[cfg(axtest)]
pub(crate) fn pipe_size_rounding_and_rejection_rules_hold_for_test() -> bool {
    // fd_ops is re-exported via `pub use self::fs::*`, so the helper is
    // accessible directly through the fs module.
    fs::pipe_size_rounding_and_rejection_rules_hold_for_test()
}

#[cfg(axtest)]
pub(crate) fn membarrier_validation_rules_hold_for_test() -> bool {
    sync::membarrier_validation_rules_hold_for_test()
}

#[cfg(axtest)]
pub(crate) fn syscall_signal_restart_rules_hold_for_test() -> bool {
    use syscalls::Sysno;

    assert!(syscall_allows_signal_restart(Sysno::read as usize));
    assert!(syscall_allows_signal_restart(Sysno::write as usize));
    for sysno in [
        Sysno::ppoll,
        Sysno::pselect6,
        Sysno::epoll_pwait,
        Sysno::epoll_pwait2,
        Sysno::msgsnd,
        Sysno::msgrcv,
    ] {
        assert!(!syscall_allows_signal_restart(sysno as usize));
    }
    #[cfg(target_arch = "x86_64")]
    for sysno in [Sysno::poll, Sysno::select, Sysno::epoll_wait] {
        assert!(!syscall_allows_signal_restart(sysno as usize));
    }
    true
}

#[cfg(axtest)]
pub(crate) use self::ipc::ipc_permission_and_constants_rules_hold_for_test;
#[cfg(axtest)]
pub(crate) use self::kmod::kmod_flags_validation_rules_hold_for_test;
#[cfg(axtest)]
pub(crate) use self::resources::resources_rlimit_validation_rules_hold_for_test;
#[cfg(axtest)]
pub(crate) use self::signal::signal_sigset_and_signo_validation_rules_hold_for_test;
#[cfg(axtest)]
pub(crate) use self::sys::sys_constants_and_validation_rules_hold_for_test;
#[cfg(axtest)]
pub(crate) use self::time::time_clock_id_validation_rules_hold_for_test;
