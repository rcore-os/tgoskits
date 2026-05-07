//! High-level filesystem API exports.

use alloc::vec::Vec;

use crate::{blockdev::*, error::*, ext4, ext4::*};

mod file_handle;
mod fs;
mod io;

pub use file_handle::*;
pub use fs::{fs_mount, fs_umount};
pub use io::*;
