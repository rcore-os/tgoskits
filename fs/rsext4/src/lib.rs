//! Core ext4 filesystem implementation.
//!
//! This crate contains the main filesystem domains:
//! - Filesystem mount, sync, and mkfs (`ext4`)
//! - Block device and journal integration (`blockdev`, `loopfile`, `jbd2`)
//! - Block groups, bitmaps, and caches (`blockgroup_description`, `bitmap`, `cache`)
//! - File and directory operations (`file`, `dir`, `entries`)
//! - Disk metadata structures (`disknode`, `superblock`)
//! - Supporting configuration and utilities (`config`, `endian`, `tool`)

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;
#[cfg(test)]
mod test_contracts;

// Re-export shared configuration constants for external callers.
pub use blockdev::Jbd2Dev;
pub use bmalloc::InodeNumber;
pub use config::{
    BITMAP_CACHE_MAX, BLOCK_SIZE, BLOCK_SIZE_U32, DATABLOCK_CACHE_MAX, DEFAULT_FEATURE_COMPAT,
    DEFAULT_FEATURE_INCOMPAT, DEFAULT_FEATURE_RO_COMPAT, DEFAULT_INODE_SIZE, DIRNAME_LEN,
    EXT4_MAJOR_VERSION, EXT4_MINOR_VERSION, EXT4_SUPER_MAGIC, GOOD_OLD_INODE_SIZE, GROUP_DESC_SIZE,
    GROUP_DESC_SIZE_OLD, INODE_CACHE_MAX, LOG_BLOCK_SIZE, MAX_BLOCK_SIZE, MIN_BLOCK_SIZE,
    RESERVED_GDT_BLOCKS, RESERVED_INODES, SUPERBLOCK_OFFSET, SUPERBLOCK_SIZE,
};
pub use dir::{FileName, mkdir, mkdir_with_owner};
pub use disknode::{DeviceNumber, Ext4TimeSpec, Ext4Timestamp};
// Re-export the unified error model.
pub use error::{ErrorContext, Ext4Error, Ext4ErrorKind, Ext4Result, FeatureSet};
pub use ext4::{
    DirectoryCursor, DirectoryEntry, DirectoryEntryType, DirectoryReader, Ext4, Ext4FileSystem,
    FilePermissions, InodeFlags, InodeInfo, InodeMetadataUpdate, MkfsOptions, MountOptions,
    MutationContext, SpecialInodeKind, format, mkfs, mkfs_with_options, umount,
};
pub use file::{
    FileExtent, FileExtentMap, FileExtentState, FileExtentTarget, PreallocationOptions,
    RangeOperation, RenameOptions, RenameOutcome, UnlinkOutcome, XattrName, XattrNamespace,
    XattrSetMode, ZeroRangeOptions, create_symbol_link, create_symbol_link_with_owner, delete_dir,
    delete_file, inspect_inode_extents, is_dir_empty, link, mkfile, mkfile_with_owner,
    operate_inode_range, preallocate_inode, punch_hole_inode, read_file, read_inode_data_into,
    reap_unlinked_inode, rename, truncate, truncate_inode, unlink, write_file, write_inode_data,
    zero_range_inode,
};
pub use io::{BlockIo, DeviceCapabilities, DeviceGeometry, SectorId, WriteFlags};
pub use metadata::{chmod, chown, set_flags, set_project, utimens};
pub use runtime::{
    Clock, Delay, EntropySource, Event, MmpIdentity, MountServices, MountedServices, NoopObserver,
    Observer,
};

pub mod bitmap;
pub mod blockdev;
pub mod blockgroup_description;
pub mod bmalloc;
pub mod cache;
pub mod checksum;
pub mod config;
pub mod crc32c;
pub mod dir;
pub mod disknode;
pub mod endian;
pub mod entries;
pub mod error;
pub mod ext4;
pub mod extents_tree;
pub mod file;
pub(crate) mod hashtree;
mod indirect;
pub mod io;
pub mod jbd2;
pub mod loopfile;
pub mod metadata;
pub mod runtime;
pub mod superblock;
pub mod tool;
