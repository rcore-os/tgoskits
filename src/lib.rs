#![doc = include_str!("../README.md")]
#![cfg_attr(not(doc), no_std)]
#![feature(doc_cfg)]
#![feature(core_io_borrowed_buf)]
#![cfg_attr(not(borrowedbuf_init), feature(maybe_uninit_fill))]
#![cfg_attr(not(maybe_uninit_slice), feature(maybe_uninit_slice))]
#![warn(missing_docs)]

#[cfg(feature = "alloc")]
extern crate alloc;

#[doc(no_inline)]
pub use axerrno::{AxError as Error, AxErrorKind as ErrorKind, AxResult as Result};

include!(concat!(env!("OUT_DIR"), "/config.rs"));

mod buffered;
pub mod prelude;
mod read;
mod seek;
mod write;

pub use self::{buffered::*, read::*, seek::*, write::*};

/// I/O poll results.
#[derive(Debug, Default, Clone, Copy)]
pub struct PollState {
    /// Object can be read now.
    pub readable: bool,
    /// Object can be writen now.
    pub writable: bool,
}
