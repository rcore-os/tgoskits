//! Configuration constants used across the ext4 implementation.

use crate::superblock::*;

// ============================================================================
// Block geometry
// ============================================================================
/// Default filesystem block size selected by rsext4 mkfs.
pub const BLOCK_SIZE: usize = 4096;
pub const BLOCK_SIZE_U32: u32 = BLOCK_SIZE as u32;
/// Smallest filesystem block size supported by ext4.
pub const MIN_BLOCK_SIZE: u32 = 1024;
/// Largest filesystem block size currently supported by the portable core.
pub const MAX_BLOCK_SIZE: u32 = 4096;

/// Log2 delta stored in `s_log_block_size`.
///
/// ext4 encodes the real block size as `1024 << s_log_block_size`, so `2`
/// means a 4 KiB block size.
pub const LOG_BLOCK_SIZE: u32 = 2;
// ============================================================================
// Block-group layout
// ============================================================================

/// Size of a 64-bit ext4 group descriptor in bytes.
pub const GROUP_DESC_SIZE: u16 = 64;
/// Size of a legacy 32-bit ext4 group descriptor in bytes.
pub const GROUP_DESC_SIZE_OLD: u16 = 32;
/// Largest group descriptor accepted by Linux ext4.
pub const GROUP_DESC_SIZE_MAX: u16 = 1024;
// ============================================================================
// Inode geometry
// ============================================================================

/// Inode size selected by rsext4 mkfs.
pub const DEFAULT_INODE_SIZE: u16 = 256;

/// Fixed inode size used by revision-0 ext2/ext4 superblocks.
pub const GOOD_OLD_INODE_SIZE: u16 = 128;

// ============================================================================
// Cache sizing
// ============================================================================
/// Enables the multi-level cache stack for inode tables, data blocks, bitmaps,
/// and group descriptors.
pub const USE_MULTILEVEL_CACHE: bool = cfg!(feature = "USE_MULTILEVEL_CACHE");
/// Maximum number of inode-table cache entries.
pub const INODE_CACHE_MAX: usize = 128;
/// Maximum number of data-block cache entries.
///
/// Kept small for read-modify-write hot blocks and truncate-generation
/// bookkeeping only; bulk read capacity is provided by the block-layer
/// cache below this crate.
pub const DATABLOCK_CACHE_MAX: usize = 32;
/// Maximum number of bitmap cache entries.
pub const BITMAP_CACHE_MAX: usize = 128;

// ============================================================================
// Directory entry layout
// ============================================================================
/// Maximum ext4 directory entry name length.
pub const DIRNAME_LEN: usize = 255;
/// Number of reserved inode numbers at the start of the filesystem.
pub const RESERVED_INODES: u32 = 10;

// ============================================================================
// Filesystem layout
// ============================================================================

/// On-disk byte offset of the primary superblock.
///
/// ext4 keeps the primary superblock at byte offset 1024 so the leading boot
/// area remains untouched.
pub const SUPERBLOCK_OFFSET: u64 = 1024;

/// Serialized superblock size in bytes.
pub const SUPERBLOCK_SIZE: usize = 1024;

/// Number of reserved GDT blocks kept for future online resize growth.
pub const RESERVED_GDT_BLOCKS: u32 = 0;

// ============================================================================
// Feature flags
// ============================================================================

/// Default COMPAT feature bitset written by mkfs.
pub const DEFAULT_FEATURE_COMPAT: u32 =
    Ext4Superblock::EXT4_FEATURE_COMPAT_HAS_JOURNAL | Ext4Superblock::EXT4_FEATURE_COMPAT_DIR_INDEX;
/// Default INCOMPAT feature bitset written by mkfs.
pub const DEFAULT_FEATURE_INCOMPAT: u32 = Ext4Superblock::EXT4_FEATURE_INCOMPAT_FILETYPE
    | Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT
    | Ext4Superblock::EXT4_FEATURE_INCOMPAT_EXTENTS;

/// Default RO_COMPAT feature bitset written by mkfs.
pub const DEFAULT_FEATURE_RO_COMPAT: u32 = Ext4Superblock::EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE
    | Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER
    | Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM;

// ============================================================================
// Magic values and versioning
// ============================================================================

/// ext4 superblock magic stored in `s_magic`.
pub const EXT4_SUPER_MAGIC: u16 = 0xEF53;

/// Filesystem major revision advertised by mkfs.
pub const EXT4_MAJOR_VERSION: u32 = 1;

/// Filesystem minor revision advertised by mkfs.
pub const EXT4_MINOR_VERSION: u16 = 0;
