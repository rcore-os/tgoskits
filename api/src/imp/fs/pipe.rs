use core::ffi::c_int;

use axerrno::LinuxResult;
use bitflags::bitflags;
use linux_raw_sys::general::{O_CLOEXEC, O_NONBLOCK};

use crate::{
    file::{FileLike, Pipe, close_file_like},
    ptr::UserPtr,
};

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

pub fn sys_pipe2(fds: UserPtr<[c_int; 2]>, flags: u32) -> LinuxResult<isize> {
    let flags = {
        let new_flags = PipeFlags::from_bits_truncate(flags);
        if new_flags.bits() != flags {
            warn!("sys_pipe2 <= unrecognized flags: {flags}");
        }
        new_flags
    };

    let fds = fds.get_as_mut()?;

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

    fds[0] = read_fd;
    fds[1] = write_fd;

    info!("sys_pipe2 <= fds: {:?}", fds);
    Ok(0)
}
