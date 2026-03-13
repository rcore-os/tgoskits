use axerrno::{AxError, AxResult};
use bitflags::bitflags;
use linux_raw_sys::general::{O_CLOEXEC, O_NONBLOCK};
use starry_signal::SignalSet;
use starry_vm::VmPtr;

use crate::{
    file::{FileLike, add_file_like, signalfd::Signalfd},
    syscall::signal::check_sigset_size,
};

// SFD flag definitions (if not available in linux_raw_sys)
const SFD_CLOEXEC: u32 = O_CLOEXEC;
const SFD_NONBLOCK: u32 = O_NONBLOCK;

bitflags! {
    /// Flags for the `signalfd4` syscall.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct SignalfdFlags: u32 {
        /// Create a file descriptor that is closed on `exec`.
        const CLOEXEC = SFD_CLOEXEC;
        /// Create a non-blocking signalfd.
        const NONBLOCK = SFD_NONBLOCK;
    }
}

/// signalfd4 system call
///
/// Creates a file descriptor that can be used to accept signals targeted at
/// the caller. This provides an alternative to the use of a signal handler or
/// sigwaitinfo(2), and has the advantage that the file descriptor may be
/// monitored by select(2), poll(2), and epoll(7).
///
/// # Arguments
/// * `fd` - If `fd` is -1, then a new file descriptor is created. Otherwise,
///   `fd` must specify a valid existing signalfd file descriptor.
/// * `mask` - Pointer to a signal set (sigset_t).
/// * `sigsetsize` - The size (in bytes) of the mask pointed to by `mask`.
/// * `flags` - Flags to control the operation.
pub fn sys_signalfd4(
    fd: i32,
    mask: *const SignalSet,
    sigsetsize: usize,
    flags: u32,
) -> AxResult<isize> {
    check_sigset_size(sigsetsize)?;

    let flags = SignalfdFlags::from_bits(flags).ok_or(AxError::InvalidInput)?;

    if fd != -1 && flags.contains(SignalfdFlags::CLOEXEC) {
        return Err(AxError::InvalidInput);
    }

    // Read the signal mask from user space before handling the request mode.
    let mask = unsafe { mask.vm_read_uninit()?.assume_init() };

    // If fd is not -1, we should modify the existing signalfd
    if fd != -1 {
        let signalfd = Signalfd::from_fd(fd)?;
        signalfd.update_mask(mask);
        signalfd.set_nonblocking(flags.contains(SignalfdFlags::NONBLOCK))?;
        return Ok(fd as _);
    }

    // Create a new Signalfd
    let signalfd = Signalfd::new(mask);
    signalfd.set_nonblocking(flags.contains(SignalfdFlags::NONBLOCK))?;

    // Add to file descriptor table
    add_file_like(signalfd as _, flags.contains(SignalfdFlags::CLOEXEC)).map(|fd| fd as _)
}
