use core::ffi::c_int;

#[cfg(feature = "poll")]
use crate::backend::sys_poll;
#[cfg(feature = "select")]
use crate::backend::sys_select;
#[cfg(feature = "epoll")]
use crate::backend::{sys_epoll_create, sys_epoll_create1, sys_epoll_ctl, sys_epoll_wait};
use crate::{ctypes, utils::e};

/// Creates a new epoll instance.
///
/// It returns a file descriptor referring to the new epoll instance.
#[cfg(feature = "epoll")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn epoll_create(size: c_int) -> c_int {
    e(sys_epoll_create(size))
}

/// Creates a new epoll instance with creation flags.
#[cfg(feature = "epoll")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn epoll_create1(flags: c_int) -> c_int {
    e(sys_epoll_create1(flags))
}

/// Control interface for an epoll file descriptor
#[cfg(feature = "epoll")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn epoll_ctl(
    epfd: c_int,
    op: c_int,
    fd: c_int,
    event: *mut ctypes::epoll_event,
) -> c_int {
    unsafe { e(sys_epoll_ctl(epfd, op, fd, event)) }
}

/// Waits for events on the epoll instance referred to by the file descriptor epfd.
#[cfg(feature = "epoll")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn epoll_wait(
    epfd: c_int,
    events: *mut ctypes::epoll_event,
    maxevents: c_int,
    timeout: c_int,
) -> c_int {
    unsafe { e(sys_epoll_wait(epfd, events, maxevents, timeout)) }
}

/// Monitor multiple file descriptors, waiting until one or more of the file descriptors become "ready" for some class of I/O operation
#[cfg(feature = "select")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn select(
    nfds: c_int,
    readfds: *mut ctypes::fd_set,
    writefds: *mut ctypes::fd_set,
    exceptfds: *mut ctypes::fd_set,
    timeout: *mut ctypes::timeval,
) -> c_int {
    unsafe { e(sys_select(nfds, readfds, writefds, exceptfds, timeout)) }
}

/// Poll file descriptors for I/O readiness.
#[cfg(feature = "poll")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn poll(
    fds: *mut ctypes::pollfd,
    nfds: ctypes::nfds_t,
    timeout: c_int,
) -> c_int {
    e(sys_poll(fds, nfds, timeout))
}
