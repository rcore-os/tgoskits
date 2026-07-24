use core::ffi::c_int;

use ax_errno::{AxError, AxResult};
use bitflags::bitflags;
use linux_raw_sys::general::{O_CLOEXEC, O_NONBLOCK};
use starry_vm::VmMutPtr;

use crate::file::{FileLike, Pipe, close_file_like};

bitflags! {
    /// Flags for the `pipe2` syscall.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct PipeFlags: u32 {
        /// Create a pipe with close-on-exec flag.
        const CLOEXEC = O_CLOEXEC;
        /// Create a non-blocking pipe.
        const NONBLOCK = O_NONBLOCK;
    }
}

pub fn sys_pipe2(fds: *mut [c_int; 2], flags: u32) -> AxResult<isize> {
    let flags = PipeFlags::from_bits(flags).ok_or_else(|| {
        warn!("sys_pipe2 <= unrecognized flags: {flags}");
        AxError::InvalidInput
    })?;

    let cloexec = flags.contains(PipeFlags::CLOEXEC);
    let (read_end, write_end) = Pipe::new();
    if flags.contains(PipeFlags::NONBLOCK) {
        read_end.set_nonblocking(true)?;
        write_end.set_nonblocking(true)?;
    }
    let read_fd = read_end.add_to_fd_table(cloexec)?;
    let write_fd = write_end
        .add_to_fd_table(cloexec)
        .inspect_err(|_| close_file_like(read_fd).unwrap())?;

    if let Err(err) = fds.vm_write([read_fd, write_fd]) {
        close_file_like(read_fd).ok();
        close_file_like(write_fd).ok();
        return Err(err.into());
    }

    debug!(
        "sys_pipe2 <= fds: {:?}, flags: {:?}",
        [read_fd, write_fd],
        flags
    );
    Ok(0)
}

#[cfg(axtest)]
pub(crate) fn pipe_flags_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::{O_CLOEXEC, O_NONBLOCK};
    // Test PipeFlags validation
    let valid_flags = 0u32;
    assert!(PipeFlags::from_bits(valid_flags).is_some());

    let cloexec_only = O_CLOEXEC as u32;
    assert!(PipeFlags::from_bits(cloexec_only).is_some());

    let nonblock_only = O_NONBLOCK as u32;
    assert!(PipeFlags::from_bits(nonblock_only).is_some());

    let all_valid = O_CLOEXEC as u32 | O_NONBLOCK as u32;
    assert!(PipeFlags::from_bits(all_valid).is_some());

    // Invalid flag should return None
    let invalid_flags = 0xFFFF;
    assert!(PipeFlags::from_bits(invalid_flags).is_none());

    true
}
