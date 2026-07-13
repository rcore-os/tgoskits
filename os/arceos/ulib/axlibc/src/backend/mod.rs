//! Direct ArceOS backends for the crate's C and POSIX ABI adapters.
//!
//! These modules translate C ABI arguments and POSIX semantics into ArceOS
//! runtime and kernel-module operations. They are private to `ax-libc` and do
//! not define a cross-crate syscall interface.

mod stdio;

pub mod io;
pub mod process;
pub mod resource;
pub mod system;
pub mod time;

#[cfg(feature = "fd")]
pub mod fd_table;
#[cfg(feature = "fs")]
pub mod fs;
#[cfg(any(feature = "select", feature = "poll", feature = "epoll"))]
pub mod io_multiplex;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "pipe")]
pub mod pipe;
#[cfg(feature = "multitask")]
pub mod pthread;

#[cfg(feature = "fd")]
pub use fd_table::{sys_close, sys_dup, sys_dup2, sys_fcntl};
#[cfg(feature = "fs")]
pub use fs::{sys_fstat, sys_getcwd, sys_lseek, sys_lstat, sys_open, sys_rename, sys_stat};
#[cfg(feature = "poll")]
pub use io_multiplex::sys_poll;
#[cfg(feature = "select")]
pub use io_multiplex::sys_select;
#[cfg(feature = "epoll")]
pub use io_multiplex::{sys_epoll_create, sys_epoll_create1, sys_epoll_ctl, sys_epoll_wait};
#[cfg(feature = "net")]
pub use net::{
    sys_accept, sys_bind, sys_connect, sys_freeaddrinfo, sys_getaddrinfo, sys_getpeername,
    sys_getsockname, sys_listen, sys_recv, sys_recvfrom, sys_send, sys_sendto, sys_setsockopt,
    sys_shutdown, sys_socket,
};
#[cfg(feature = "pipe")]
pub use pipe::sys_pipe;
#[cfg(feature = "multitask")]
pub use pthread::mutex::{
    sys_pthread_mutex_destroy, sys_pthread_mutex_init, sys_pthread_mutex_lock,
    sys_pthread_mutex_trylock, sys_pthread_mutex_unlock,
};
#[cfg(feature = "multitask")]
pub use pthread::{sys_pthread_create, sys_pthread_exit, sys_pthread_join, sys_pthread_self};

pub use self::{
    io::{sys_read, sys_write, sys_writev},
    process::{sys_exit, sys_getpid, sys_sched_yield},
    resource::{sys_getrlimit, sys_setrlimit},
    system::sys_sysconf,
    time::{sys_clock_gettime, sys_nanosleep},
};
