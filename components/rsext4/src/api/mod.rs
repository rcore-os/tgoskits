//! High-level filesystem API exports.

use alloc::vec::Vec;

use crate::{BLOCK_SIZE, blockdev::*, dir::*, error::*, ext4::*, file::*, loopfile::*, *};

mod file_handle;
mod fs;
mod io;
mod oldio;

pub use file_handle::*;
pub use fs::{fs_mount, fs_umount};
pub use io::*;
pub use oldio::*;
