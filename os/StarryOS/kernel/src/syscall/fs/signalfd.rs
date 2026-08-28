use bitflags::bitflags;
use linux_raw_sys::general::{O_CLOEXEC, O_NONBLOCK};
use starry_signal::SignalSet;
use starry_vm::VmPtr;

use crate::{
    StarryError, StarryResult,
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
/// * `flags` - Flags used when creating a new descriptor. Linux validates but
///   otherwise ignores these flags when updating an existing signalfd.
pub fn sys_signalfd4(
    fd: i32,
    mask: *const SignalSet,
    sigsetsize: usize,
    flags: u32,
) -> StarryResult<isize> {
    check_sigset_size(sigsetsize)?;

    let flags = SignalfdFlags::from_bits(flags).ok_or(StarryError::InvalidInput)?;

    // Read the signal mask from user space before handling the request mode.
    let mask = unsafe { mask.vm_read_uninit()?.assume_init() };

    // Linux only updates the mask for an existing signalfd. Valid creation
    // flags do not alter its descriptor or file status flags.
    if fd != -1 {
        let signalfd = Signalfd::from_fd(fd)?;
        signalfd.update_mask(mask);
        return Ok(fd as _);
    }

    // Create a new Signalfd
    let signalfd = Signalfd::new(mask);
    signalfd.set_nonblocking(flags.contains(SignalfdFlags::NONBLOCK))?;

    // Add to file descriptor table
    add_file_like(signalfd as _, flags.contains(SignalfdFlags::CLOEXEC)).map(|fd| fd as _)
}

#[cfg(all(test, not(axtest)))]
fn signalfd_flags_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::{O_CLOEXEC, O_NONBLOCK};
    // Test SignalfdFlags validation
    let valid_flags = 0u32;
    assert!(SignalfdFlags::from_bits(valid_flags).is_some());

    let cloexec_only = O_CLOEXEC;
    assert!(SignalfdFlags::from_bits(cloexec_only).is_some());

    let nonblock_only = O_NONBLOCK;
    assert!(SignalfdFlags::from_bits(nonblock_only).is_some());

    let all_valid = O_CLOEXEC | O_NONBLOCK;
    assert!(SignalfdFlags::from_bits(all_valid).is_some());

    // Invalid flag should return None
    let invalid_flags = 0xFFFF;
    assert!(SignalfdFlags::from_bits(invalid_flags).is_none());

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn signalfd_flags_validation_rules_hold() {
        assert!(super::signalfd_flags_validation_rules_hold_for_test());
    }
}
