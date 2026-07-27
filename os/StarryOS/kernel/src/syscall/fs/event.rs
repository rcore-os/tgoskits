use ax_errno::{AxError, AxResult};
use bitflags::bitflags;
use linux_raw_sys::general::{EFD_CLOEXEC, EFD_NONBLOCK, EFD_SEMAPHORE};

use crate::file::{FileLike, add_file_like, event::EventFd};

bitflags! {
    /// Flags for the `eventfd2` syscall.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct EventFdFlags: u32 {
        /// Create a file descriptor that is closed on `exec`.
        const CLOEXEC = EFD_CLOEXEC;
        /// Create a non-blocking eventfd.
        const NONBLOCK = EFD_NONBLOCK;
        /// Create a semaphore eventfd.
        const SEMAPHORE = EFD_SEMAPHORE;
    }
}

#[cfg(target_arch = "x86_64")]
pub fn sys_eventfd(initval: u32) -> AxResult<isize> {
    sys_eventfd2(initval, 0)
}

// Create an eventfd and install it into the current file descriptor table.
pub fn sys_eventfd2(initval: u32, flags: u32) -> AxResult<isize> {
    debug!(
        "sys_eventfd2 called: initval={}, flags={:#x}",
        initval, flags
    );

    let flags = EventFdFlags::from_bits(flags).ok_or(AxError::InvalidInput)?;

    let event_fd = EventFd::new(initval as _, flags.contains(EventFdFlags::SEMAPHORE));
    event_fd.set_nonblocking(flags.contains(EventFdFlags::NONBLOCK))?;
    let fd =
        add_file_like(event_fd as _, flags.contains(EventFdFlags::CLOEXEC)).map(|fd| fd as _)?;
    debug!("sys_eventfd2: success, fd={}", fd);
    Ok(fd)
}

#[cfg(axtest)]
pub(crate) fn eventfd_flags_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::{EFD_CLOEXEC, EFD_NONBLOCK, EFD_SEMAPHORE};
    // Test EventFdFlags validation
    let valid_flags = 0u32;
    assert!(EventFdFlags::from_bits(valid_flags).is_some());

    let cloexec_only = EFD_CLOEXEC as u32;
    assert!(EventFdFlags::from_bits(cloexec_only).is_some());

    let nonblock_only = EFD_NONBLOCK as u32;
    assert!(EventFdFlags::from_bits(nonblock_only).is_some());

    let semaphore_only = EFD_SEMAPHORE as u32;
    assert!(EventFdFlags::from_bits(semaphore_only).is_some());

    let all_valid = EFD_CLOEXEC as u32 | EFD_NONBLOCK as u32 | EFD_SEMAPHORE as u32;
    assert!(EventFdFlags::from_bits(all_valid).is_some());

    // Invalid flag should return None
    let invalid_flags = 0xFFFF;
    assert!(EventFdFlags::from_bits(invalid_flags).is_none());

    true
}
