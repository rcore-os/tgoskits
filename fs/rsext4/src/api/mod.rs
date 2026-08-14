//! High-level filesystem API exports.

use alloc::vec::Vec;

use crate::{blockdev::*, dir::*, error::*, ext4::*, file::*, loopfile::*, *};

mod file_handle;
mod io;

pub use file_handle::OpenFile;
pub use io::{lseek, open, read, read_at, write_at};
