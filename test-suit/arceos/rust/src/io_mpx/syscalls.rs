//! Raw syscall shims for the eventfd/epoll tests.
//!
//! `ax_std` exposes `eventfd`, `epoll_create1`, `epoll_ctl`, `epoll_wait`,
//! `read`, and `write` as `#[no_mangle]` `extern "C"` symbols in
//! `ax_std::os::libc_compat`. The test crate declares them here with plain C
//! ABI types (the layout is erased at link time), so the tests can call them
//! without depending on the `libc` crate. Error results follow the Linux
//! convention: `-1` and the global `errno` set to the error code.

use core::{ffi::c_int, ptr};

/// Linux `struct epoll_event` ABI. On x86_64 the `data` field is packed to
/// offset 4 (12-byte struct); on other architectures it is naturally aligned
/// (16-byte struct). Must match `ax-posix-api`'s bindgen `epoll_event`.
#[cfg(target_arch = "x86_64")]
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct EpollEvent {
    pub events: u32,
    pub data: u64,
}

#[cfg(not(target_arch = "x86_64"))]
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct EpollEvent {
    pub events: u32,
    pub data: u64,
}

impl EpollEvent {
    /// Reads `data`, which may be unaligned on x86_64 (packed layout).
    pub fn data(&self) -> u64 {
        unsafe { ptr::read_unaligned(ptr::addr_of!(self.data)) }
    }
}

/// The `#[no_mangle]` symbols provided by `ax_std::os::libc_compat`.
mod raw {
    use core::ffi::{c_int, c_void};

    use super::EpollEvent;

    unsafe extern "C" {
        pub(super) fn eventfd(initval: u32, flags: c_int) -> c_int;
        pub(super) fn pipe(pipefd: *mut c_int) -> c_int;
        pub(super) fn epoll_create1(flags: c_int) -> c_int;
        pub(super) fn epoll_ctl(epfd: c_int, op: c_int, fd: c_int, event: *mut EpollEvent)
        -> c_int;
        pub(super) fn epoll_wait(
            epfd: c_int,
            events: *mut EpollEvent,
            maxevents: c_int,
            timeout: c_int,
        ) -> c_int;
        pub(super) fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
        pub(super) fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
        pub(super) fn __errno_location() -> *mut c_int;
    }
}

/// The `errno` set by the libc-compat layer on failure.
fn last_errno() -> c_int {
    unsafe { *raw::__errno_location() }
}

fn fd_syscall(result: c_int) -> Result<c_int, c_int> {
    if result < 0 {
        Err(last_errno())
    } else {
        Ok(result)
    }
}

fn io_syscall(result: isize) -> Result<usize, c_int> {
    if result < 0 {
        Err(last_errno())
    } else {
        Ok(result as usize)
    }
}

pub fn eventfd(initval: u32, flags: c_int) -> Result<c_int, c_int> {
    fd_syscall(unsafe { raw::eventfd(initval, flags) })
}

/// Creates a pipe and returns `(read_fd, write_fd)`.
pub fn pipe() -> Result<(c_int, c_int), c_int> {
    let mut fds = [0 as c_int; 2];
    fd_syscall(unsafe { raw::pipe(fds.as_mut_ptr()) })?;
    Ok((fds[0], fds[1]))
}

pub fn epoll_create1(flags: c_int) -> Result<c_int, c_int> {
    fd_syscall(unsafe { raw::epoll_create1(flags) })
}

pub fn epoll_ctl(
    epfd: c_int,
    op: c_int,
    fd: c_int,
    event: Option<&mut EpollEvent>,
) -> Result<c_int, c_int> {
    let ptr = event.map_or(ptr::null_mut(), |event| event as *mut EpollEvent);
    fd_syscall(unsafe { raw::epoll_ctl(epfd, op, fd, ptr) })
}

pub fn epoll_wait(epfd: c_int, events: &mut [EpollEvent], timeout: c_int) -> Result<c_int, c_int> {
    fd_syscall(unsafe {
        raw::epoll_wait(epfd, events.as_mut_ptr(), events.len() as c_int, timeout)
    })
}

pub fn read(fd: c_int, buf: &mut [u8]) -> Result<usize, c_int> {
    io_syscall(unsafe { raw::read(fd, buf.as_mut_ptr().cast(), buf.len()) })
}

pub fn write(fd: c_int, buf: &[u8]) -> Result<usize, c_int> {
    io_syscall(unsafe { raw::write(fd, buf.as_ptr().cast(), buf.len()) })
}

pub fn read_u64(fd: c_int) -> Result<u64, c_int> {
    let mut buf = [0u8; 8];
    read(fd, &mut buf)?;
    Ok(u64::from_ne_bytes(buf))
}

pub fn write_u64(fd: c_int, value: u64) -> Result<usize, c_int> {
    write(fd, &value.to_ne_bytes())
}

/// Asserts that a syscall failed with the given errno.
pub fn assert_errno<T>(result: Result<T, c_int>, expected: c_int, what: &str) {
    match result {
        Err(errno) => assert_eq!(
            errno, expected,
            "{what} must fail with errno {expected}, got {errno}"
        ),
        Ok(_) => panic!("{what} must fail with errno {expected}, but it succeeded"),
    }
}
