use crate::ext4_backend::superblock::*;

// ============================================================================
// Journal 相关配置
// ============================================================================
/// JBD2 日志缓冲区最大数量
pub const JBD2_BUFFER_MAX: usize = 10; //最多10条缓存

// ============================================================================
// 块相关配置
// ============================================================================
/// Ext4 块大小（字节）
pub const BLOCK_SIZE: usize = 4096;//usize没问题
pub const BLOCK_SIZE_U32: u32 = BLOCK_SIZE as u32;

/// Ext4 块大小对数（log2）

/// 用于超级块的 s_log_block_size 字段
pub const LOG_BLOCK_SIZE: u32 = 2; // 4096 = 1024 << 2
// ============================================================================
// 块组相关配置
// ============================================================================

/// 块组描述符大小（字节）
/// 标准 Ext4（64位）：64字节
pub const GROUP_DESC_SIZE: u16 = 64;
/// 旧版 Ext4（32位）：32字节
pub const GROUP_DESC_SIZE_OLD: u16 = 32;
// ============================================================================
// Inode 相关配置
// ============================================================================

/// Inode 默认大小（字节）
///
/// NOTE: real inode size is stored in superblock.s_inode_size.
/// This constant should only be used as a fallback when s_inode_size is 0.
pub const DEFAULT_INODE_SIZE: u16 = 256;

// ============================================================================
// 数据结构缓存相关配置,在小的嵌入式系统中可以适当调小防止崩内存
// ============================================================================
///Inodecahe数量
pub const INODE_CACHE_MAX: usize = 128;
///Datablock cahce数量
pub const DATABLOCK_CACHE_MAX: usize = 128;
///BITMAP cache数量
pub const BITMAP_CACHE_MAX: usize = 128;

//============================================================================
//目录项DirEntry配置
//============================================================================
pub const DIRNAME_LEN: usize = 255; //目录名长度
///保留inodes数量
pub const RESERVED_INODES: u32 = 10;

// ============================================================================
// 文件系统布局
// ============================================================================

/// 超级块在分区中的偏移量（字节）
/// 总是从 1024 字节开始，跳过引导扇区
pub const SUPERBLOCK_OFFSET: u64 = 1024;

/// 超级块大小（字节）
pub const SUPERBLOCK_SIZE: usize = 1024;

/// 预留的 GDT 块数（用于未来扩展块组描述符）
pub const RESERVED_GDT_BLOCKS: u32 = 0;

// ============================================================================
// 特性标志
// ============================================================================

/// 默认的兼容特性标志
pub const DEFAULT_FEATURE_COMPAT: u32 =
    Ext4Superblock::EXT4_FEATURE_COMPAT_HAS_JOURNAL | Ext4Superblock::EXT4_FEATURE_COMPAT_DIR_INDEX;
/// 默认的不兼容特性标志
pub const DEFAULT_FEATURE_INCOMPAT: u32 = Ext4Superblock::EXT4_FEATURE_INCOMPAT_FILETYPE
    | Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT
    | Ext4Superblock::EXT4_FEATURE_INCOMPAT_EXTENTS;

/// 默认的只读兼容特性标志
pub const DEFAULT_FEATURE_RO_COMPAT: u32 = Ext4Superblock::EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE
    | Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER;

// ============================================================================
// 魔数和版本
// ============================================================================

/// Ext4 超级块魔数
pub const EXT4_SUPER_MAGIC: u16 = 0xEF53;

/// 文件系统版本（主版本号）
pub const EXT4_MAJOR_VERSION: u32 = 1;

/// 文件系统版本（次版本号）
pub const EXT4_MINOR_VERSION: u16 = 0;
