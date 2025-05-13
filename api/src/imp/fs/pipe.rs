use core::ffi::c_int;

use axerrno::LinuxResult;

use crate::{
    file::{FileLike, Pipe, close_file_like},
    ptr::UserPtr,
};

pub fn sys_pipe2(fds: UserPtr<[c_int; 2]>, flags: i32) -> LinuxResult<isize> {
    if flags != 0 {
        warn!("sys_pipe2: unsupported flags: {}", flags);
    }

    let fds = fds.get_as_mut()?;

    let (read_end, write_end) = Pipe::new();
    let read_fd = read_end.add_to_fd_table()?;
    let write_fd = write_end
        .add_to_fd_table()
        .inspect_err(|_| close_file_like(read_fd).unwrap())?;

    fds[0] = read_fd;
    fds[1] = write_fd;

    info!("sys_pipe2 <= fds: {:?}", fds);
    Ok(0)
}
