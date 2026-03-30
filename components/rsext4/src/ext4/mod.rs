//! Core filesystem state, mount, allocation, and mkfs helpers.

use ::alloc::{collections::VecDeque, vec::Vec};
use log::{debug, error, info, trace, warn};

use crate::{
    bitmap::InodeBitmap,
    blockdev::*,
    blockgroup_description::*,
    bmalloc::*,
    cache::{bitmap::CacheKey, *},
    checksum::*,
    config::*,
    crc32c::ext4_superblock_has_metadata_csum,
    dir::*,
    disknode::*,
    endian::*,
    error::*,
    jbd2::{jbd2::*, jbdstruct::*},
    loopfile::*,
    superblock::*,
    tool::*,
};

mod alloc;
mod fs;
mod lookup;
mod mkfs;
mod mount;
mod sync;

pub use fs::{Ext4FileSystem, FileSystemStats};
pub use lookup::{file_entry_exisr, find_file};
pub use mkfs::{BlcokGroupLayout, FsLayoutInfo, compute_fs_layout, mkfs};
pub use mount::mount;
pub use sync::umount;
