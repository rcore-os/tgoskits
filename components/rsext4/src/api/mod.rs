//! High-level filesystem API exports.

use alloc::vec::Vec;

use crate::{BLOCK_SIZE, blockdev::*, dir::*, error::*, ext4::*, file::*, loopfile::*, *};

mod file_handle;
mod fs;
mod io;

pub use file_handle::OpenFile;
pub use fs::{fs_mount, fs_umount};
pub use io::{lseek, open, read, read_at, write_at};
