//! [ArceOS] user program library for C apps.
//!
//! ## Cargo Features
//!
//! - CPU
//!     - `smp`: Enable SMP (symmetric multiprocessing) support.
//!     - `fp-simd`: Enable floating point and SIMD support.
//! - Interrupts:
//!     - `irq`: Enable interrupt handling support.
//! - Memory
//!     - `alloc`: Enable dynamic memory allocation.
//!     - `tls`: Enable thread-local storage.
//! - Task management
//!     - `multitask`: Enable multi-threading support.
//! - Upperlayer stacks
//!     - `fs`: Enable file system support.
//!     - `net`: Enable networking support.
//! - Lib C functions
//!     - `fd`: Enable file descriptor table.
//!     - `pipe`: Enable pipe support.
//!     - `select`: Enable synchronous I/O multiplexing ([select]) support.
//!     - `epoll`: Enable event polling ([epoll]) support.
//!
//! [ArceOS]: https://github.com/arceos-org/arceos
//! [select]: https://man7.org/linux/man-pages/man2/select.2.html
//! [epoll]: https://man7.org/linux/man-pages/man7/epoll.7.html

#![cfg_attr(all(not(test), not(doc)), no_std)]
#![cfg_attr(feature = "tls", feature(thread_local))]
#![allow(clippy::missing_safety_doc)]

#[cfg(feature = "alloc")]
extern crate alloc;
extern crate ax_driver as _;
#[macro_use]
extern crate ax_log;
extern crate ax_runtime;

mod ctypes {
    #[rustfmt::skip]
    #[allow(dead_code, non_snake_case, non_camel_case_types, non_upper_case_globals, clippy::upper_case_acronyms)]
    mod libctypes {
        include!(concat!(env!("OUT_DIR"), "/libctypes_gen.rs"));
    }

    pub use libctypes::*;
}

#[macro_use]
mod utils;
mod backend;

pub(crate) use backend::sys_sched_yield;

#[cfg(feature = "fd")]
mod fd;
#[cfg(feature = "fs")]
mod fs;
#[cfg(any(feature = "select", feature = "poll", feature = "epoll"))]
mod io_multiplex;
#[cfg(feature = "alloc")]
mod malloc;
#[cfg(feature = "net")]
mod net;
#[cfg(feature = "pipe")]
mod pipe;
#[cfg(feature = "multitask")]
mod pthread;
#[cfg(feature = "alloc")]
mod strftime;
#[cfg(feature = "fp-simd")]
mod strtod;

mod errno;
mod io;
mod mktime;
mod rand;
mod resource;
mod setjmp;
mod system;
mod time;
mod unistd;

#[cfg(feature = "fd")]
pub use self::fd::{ax_fcntl, close, dup, dup2, dup3};
#[cfg(feature = "fs")]
pub use self::fs::{ax_open, fstat, getcwd, lseek, lstat, rename, stat};
#[cfg(not(test))]
pub use self::io::write;
#[cfg(feature = "poll")]
pub use self::io_multiplex::poll;
#[cfg(feature = "select")]
pub use self::io_multiplex::select;
#[cfg(feature = "epoll")]
pub use self::io_multiplex::{epoll_create, epoll_create1, epoll_ctl, epoll_wait};
#[cfg(feature = "alloc")]
pub use self::malloc::{free, malloc};
#[cfg(feature = "net")]
pub use self::net::{
    accept, bind, connect, freeaddrinfo, getaddrinfo, getpeername, getsockname, listen, recv,
    recvfrom, send, sendto, setsockopt, shutdown, socket,
};
#[cfg(feature = "pipe")]
pub use self::pipe::pipe;
#[cfg(feature = "multitask")]
pub use self::pthread::{pthread_create, pthread_exit, pthread_join, pthread_self};
#[cfg(feature = "multitask")]
pub use self::pthread::{pthread_mutex_init, pthread_mutex_lock, pthread_mutex_unlock};
#[cfg(feature = "alloc")]
pub use self::strftime::strftime;
#[cfg(feature = "fp-simd")]
pub use self::strtod::{strtod, strtof};
pub use self::{
    errno::strerror,
    io::{read, writev},
    mktime::mktime,
    rand::{rand, random, srand},
    resource::{getrlimit, setrlimit},
    setjmp::{longjmp, setjmp},
    system::sysconf,
    time::{clock_gettime, nanosleep},
    unistd::{abort, exit, getpid},
};
