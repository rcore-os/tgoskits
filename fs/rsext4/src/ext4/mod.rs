//! Core filesystem state, mount, allocation, and mkfs helpers.

use ::alloc::{collections::VecDeque, vec::Vec};

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
mod owned;
mod sync;
mod system_zone;

pub use fs::{Ext4FileSystem, FileSystemStats};
pub use lookup::{file_entry_exisr, file_entry_exist, find_file};
pub use mkfs::{
    BlcokGroupLayout, BlockGroupLayout, FsLayoutInfo, MkfsOptions, compute_fs_layout, mkfs,
    mkfs_with_options,
};
pub use mount::{MountOptions, mount, mount_with_options, mount_with_options_and_observer};
pub use owned::{DirectoryEntry, DirectoryEntryType, Ext4, InodeInfo, MutationContext};
pub use sync::umount;
pub(crate) use system_zone::SystemZoneMap;
