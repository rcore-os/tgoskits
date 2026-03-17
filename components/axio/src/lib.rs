#![doc = include_str!("../README.md")]
#![cfg_attr(not(doc), no_std)]
#![feature(doc_cfg)]
#![feature(core_io_borrowed_buf)]
#![feature(min_specialization)]
#![feature(maybe_uninit_fill)]
#![cfg_attr(not(maybe_uninit_slice), feature(maybe_uninit_slice))]
#![warn(missing_docs)]

#[cfg(feature = "alloc")]
extern crate alloc;

#[doc(no_inline)]
pub use axerrno::{AxError as Error, AxErrorKind as ErrorKind, AxResult as Result};

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
}
