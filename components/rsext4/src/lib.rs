//! Core ext4 filesystem implementation.
//!
//! This crate contains the main filesystem domains:
//! - Filesystem mount, sync, and mkfs (`api`, `ext4`)
//! - Block device and journal integration (`blockdev`, `loopfile`, `jbd2`)
//! - Block groups, bitmaps, and caches (`blockgroup_description`, `bitmap`, `cache`)
//! - File and directory operations (`file`, `dir`, `entries`)
//! - Disk metadata structures (`disknode`, `superblock`)
//! - Supporting configuration and utilities (`config`, `endian`, `tool`)

#![no_std]

extern crate alloc;

// Re-export shared configuration constants for external callers.
// Re-export the most frequently used public APIs.
pub use api::{lseek, open, read_at, write_at};
pub use blockdev::{BlockDevice, Jbd2Dev};
pub use config::{
    BITMAP_CACHE_MAX, BLOCK_SIZE, BLOCK_SIZE_U32, DATABLOCK_CACHE_MAX, DEFAULT_FEATURE_COMPAT,
    DEFAULT_FEATURE_INCOMPAT, DEFAULT_FEATURE_RO_COMPAT, DEFAULT_INODE_SIZE, DIRNAME_LEN,
    EXT4_MAJOR_VERSION, EXT4_MINOR_VERSION, EXT4_SUPER_MAGIC, GROUP_DESC_SIZE, GROUP_DESC_SIZE_OLD,
    INODE_CACHE_MAX, JBD2_BUFFER_MAX, LOG_BLOCK_SIZE, RESERVED_GDT_BLOCKS, RESERVED_INODES,
    SUPERBLOCK_OFFSET, SUPERBLOCK_SIZE,
};
pub use dir::mkdir;
pub use disknode::{Ext4TimeSpec, Ext4Timestamp};
// Re-export the unified error model.
pub use error::{Errno, Ext4Error, Ext4Result};
pub use ext4::{Ext4FileSystem, find_file, mkfs, mount, umount};
pub use file::{
    create_symbol_link, delete_dir, delete_file, link, mkfile, mv, read_file, rename, truncate,
    unlink, write_file,
};
pub use metadata::{chmod, chown, set_flags, set_project, utimens};

pub mod api;
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
pub mod hashtree;
pub mod jbd2;
pub mod loopfile;
pub mod metadata;
pub mod superblock;
pub mod tool;
