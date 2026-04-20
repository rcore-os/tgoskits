//! Core ext4 directory entry structures.

use crate::config::*;

/// Legacy ext4 directory entry layout.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DirEntry {
    pub inode: u32,
    pub rec_len: u16,
    pub name_len: u8,
    pub file_type: u8,
    pub name: [u8; DIRNAME_LEN],
}

/// Extended ext4 directory entry layout.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DirEntry2 {
    pub inode: u32,
    pub rec_len: u16,
    pub name_len: u8,
    pub file_type: u8,
    pub name: [u8; DIRNAME_LEN],
}

impl Ext4DirEntry2 {
    /// Creates a directory entry, truncating the name to the on-disk limit.
    pub fn new(inode_num: u32, rec_len: u16, file_type: u8, name: &[u8]) -> Self {
        let mut name_buf = [0u8; DIRNAME_LEN];
        let len = core::cmp::min(
            name.len(),
            core::cmp::min(Self::MAX_NAME_LEN as usize, DIRNAME_LEN),
        );
        name_buf[..len].copy_from_slice(&name[..len]);
        Ext4DirEntry2 {
            inode: inode_num,
            rec_len,
            name_len: len as u8,
            file_type,
            name: name_buf,
        }
    }

    /// Minimum encoded directory entry length.
    pub const MIN_DIR_ENTRY_LEN: u16 = 12;

    /// Maximum filename length stored by the format.
    pub const MAX_NAME_LEN: u8 = 255;

    /// Returns the aligned entry length for a filename.
    pub fn entry_len(name_len: u8) -> u16 {
        let base_len = 8;
        let total = base_len + name_len as u16;
        total.div_ceil(4) * 4
    }
}

impl Ext4DirEntry2 {
    pub const EXT4_FT_UNKNOWN: u8 = 0;
    pub const EXT4_FT_REG_FILE: u8 = 1;
    pub const EXT4_FT_DIR: u8 = 2;
    pub const EXT4_FT_CHRDEV: u8 = 3;
    pub const EXT4_FT_BLKDEV: u8 = 4;
    pub const EXT4_FT_FIFO: u8 = 5;
    pub const EXT4_FT_SOCK: u8 = 6;
    pub const EXT4_FT_SYMLINK: u8 = 7;
    pub const EXT4_FT_MAX: u8 = 8;
}

/// Directory entry tail used for checksummed directory blocks.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DirEntryTail {
    pub det_reserved_zero1: u32,
    pub det_rec_len: u16,
    pub det_reserved_zero2: u8,
    pub det_reserved_ft: u8,
    pub det_checksum: u32,
}

impl Default for Ext4DirEntryTail {
    fn default() -> Self {
        Self {
            det_reserved_zero1: 0,
            det_rec_len: Self::TAIL_LEN,
            det_reserved_zero2: 0,
            det_reserved_ft: Self::RESERVED_FT,
            det_checksum: 0,
        }
    }
}

impl Ext4DirEntryTail {
    pub const RESERVED_FT: u8 = 0xDE;
    pub const TAIL_LEN: u16 = 12;

    pub fn new() -> Self {
        Self::default()
    }
}

/// Leaf node metadata used by the extent status tree.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4ExtentStatus {
    pub es_lblk: u64,
    pub es_len: u64,
    pub es_pblk: u64,
}
