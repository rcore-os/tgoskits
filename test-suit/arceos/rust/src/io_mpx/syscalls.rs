//! Standard file ownership and I/O with libc's Linux eventfd/epoll ABI.
//!
//! std has no eventfd or epoll interface. Those operations use libc types and
//! symbols; pipe creation, reads, writes, errno and descriptor lifetime use std.

use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr,
};

use libc::c_int;
pub use libc::epoll_event as EpollEvent;

fn errno(error: io::Error) -> c_int {
    error
        .raw_os_error()
        .expect("libc I/O errors must preserve errno")
}

fn fd_result(result: c_int) -> Result<c_int, c_int> {
    if result < 0 {
        Err(errno(io::Error::last_os_error()))
    } else {
        Ok(result)
    }
}

fn owned_file(result: c_int) -> Result<File, c_int> {
    let fd = fd_result(result)?;
    // SAFETY: a successful descriptor-creating call returns a fresh fd. File
    // takes its sole ownership and closes it when the test releases it.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub fn eventfd(initval: u32, flags: c_int) -> Result<File, c_int> {
    // SAFETY: eventfd takes scalar values and returns a new descriptor.
    owned_file(unsafe { libc::eventfd(initval, flags) })
}

pub fn pipe() -> Result<(File, File), c_int> {
    let (reader, writer) = io::pipe().map_err(errno)?;
    Ok((
        File::from(OwnedFd::from(reader)),
        File::from(OwnedFd::from(writer)),
    ))
}

pub fn epoll_create1(flags: c_int) -> Result<File, c_int> {
    // SAFETY: epoll_create1 takes a scalar flag value and returns a new fd.
    owned_file(unsafe { libc::epoll_create1(flags) })
}

pub fn epoll_ctl(
    epfd: &File,
    op: c_int,
    fd: &File,
    event: Option<&mut EpollEvent>,
) -> Result<c_int, c_int> {
    let event = event.map_or(ptr::null_mut(), ptr::from_mut);
    // SAFETY: both files stay alive for the call; event is either null for DEL
    // or a uniquely borrowed initialized libc epoll_event of the target ABI.
    fd_result(unsafe { libc::epoll_ctl(epfd.as_raw_fd(), op, fd.as_raw_fd(), event) })
}

pub fn epoll_wait(epfd: &File, events: &mut [EpollEvent], timeout: c_int) -> Result<c_int, c_int> {
    let maxevents = c_int::try_from(events.len()).expect("epoll test buffer exceeds c_int");
    // SAFETY: the mutable slice provides maxevents writable, ABI-correct events
    // and the epoll descriptor remains owned throughout the synchronous call.
    fd_result(unsafe {
        libc::epoll_wait(epfd.as_raw_fd(), events.as_mut_ptr(), maxevents, timeout)
    })
}

pub fn read(mut fd: &File, buf: &mut [u8]) -> Result<usize, c_int> {
    fd.read(buf).map_err(errno)
}

pub fn write(mut fd: &File, buf: &[u8]) -> Result<usize, c_int> {
    fd.write(buf).map_err(errno)
}

pub fn read_u64(fd: &File) -> Result<u64, c_int> {
    let mut buf = [0u8; 8];
    assert_eq!(
        read(fd, &mut buf)?,
        buf.len(),
        "eventfd read must return a full counter"
    );
    Ok(u64::from_ne_bytes(buf))
}

pub fn write_u64(fd: &File, value: u64) -> Result<usize, c_int> {
    write(fd, &value.to_ne_bytes())
}

pub fn assert_errno<T>(result: Result<T, c_int>, expected: c_int, what: &str) {
    match result {
        Err(errno) => assert_eq!(
            errno, expected,
            "{what} must fail with errno {expected}, got {errno}"
        ),
        Ok(_) => panic!("{what} must fail with errno {expected}, but it succeeded"),
    }
}
