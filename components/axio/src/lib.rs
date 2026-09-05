#![doc = include_str!("../README.md")]
#![cfg_attr(not(doc), no_std)]
#![feature(core_io)]
#![feature(core_io_borrowed_buf)]
#![feature(borrowed_buf_init)]
#![feature(maybe_uninit_fill)]
#![feature(min_specialization)]
#![warn(missing_docs)]

#[cfg(feature = "alloc")]
extern crate alloc;

mod error;

pub use error::{Error, ErrorKind, IoError, IoResult, Result};

/// Default buffer size for I/O operations.
pub const DEFAULT_BUF_SIZE: usize = 1024 * 2;

mod buffered;
mod iobuf;
pub mod prelude;
mod read;
mod seek;
mod utils;
mod write;

pub use self::{buffered::*, iobuf::*, read::*, seek::*, utils::*, write::*};

/// I/O poll results.
#[derive(Debug, Default, Clone, Copy)]
pub struct PollState {
    /// Object can be read now.
    pub readable: bool,
    /// Object can be writen now.
    pub writable: bool,
    /// Monotonic token changed when the object's read readiness may have
    /// changed.
    pub read_readiness_version: u64,
    /// Monotonic token changed when the object's write readiness may have
    /// changed.
    pub write_readiness_version: u64,
}
