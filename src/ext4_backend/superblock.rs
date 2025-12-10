use crate::ext4_backend::jbd2::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::jbd2::jbdstruct::*;
use crate::ext4_backend::endian::*;
///UUID
pub struct UUID(pub [u32;4]);


/// Ext4 超级块结构
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4Superblock {
    // 0x00 - 基本文件系统信息
    pub s_inodes_count: u32,           // Inode总数
    pub s_blocks_count_lo: u32,        // 块总数（低32位）
    pub s_r_blocks_count_lo: u32,      // 保留块数（低32位）
    pub s_free_blocks_count_lo: u32,   // 空闲块数（低32位）
    pub s_free_inodes_count: u32,      // 空闲inode数
    pub s_first_data_block: u32,       // 第一个数据块
    pub s_log_block_size: u32,         // 块大小 = 1024 << s_log_block_size
    pub s_log_cluster_size: u32,       // 簇大小 = 1024 << s_log_cluster_size
    pub s_blocks_per_group: u32,       // 每个块组的块数
    pub s_clusters_per_group: u32,     // 每个块组的簇数
    pub s_inodes_per_group: u32,       // 每个块组的inode数
    pub s_mtime: u32,                  // 挂载时间
    pub s_wtime: u32,                  // 写入时间

    // 0x34 - 挂载计数和检查
    pub s_mnt_count: u16,              // 挂载次数
    pub s_max_mnt_count: u16,          // 最大挂载次数
    pub s_magic: u16,                  // 魔数 0xEF53
    pub s_state: u16,                  // 文件系统状态
    pub s_errors: u16,                 // 错误处理方式
    pub s_minor_rev_level: u16,        // 次版本号
    pub s_lastcheck: u32,              // 最后检查时间
    pub s_checkinterval: u32,          // 检查间隔
    pub s_creator_os: u32,             // 创建者操作系统
    pub s_rev_level: u32,              // 主版本号
    pub s_def_resuid: u16,             // 保留块的默认uid
    pub s_def_resgid: u16,             // 保留块的默认gid

    // 0x54 - EXT4_DYNAMIC_REV 扩展字段
    pub s_first_ino: u32,              // 第一个非保留inode
    pub s_inode_size: u16,             // Inode结构大小
    pub s_block_group_nr: u16,         // 此超级块所在的块组号
    pub s_feature_compat: u32,         // 兼容特性标志
    pub s_feature_incompat: u32,       // 不兼容特性标志
    pub s_feature_ro_compat: u32,      // 只读兼容特性标志
    pub s_uuid: [u8; 16],              // 128位UUID
    pub s_volume_name: [u8; 16],       // 卷名
    pub s_last_mounted: [u8; 64],      // 最后挂载路径
    pub s_algorithm_usage_bitmap: u32, // 压缩算法使用位图

    // 0xDC - 性能提示
    pub s_prealloc_blocks: u8,         // 预分配块数
    pub s_prealloc_dir_blocks: u8,     // 目录预分配块数
    pub s_reserved_gdt_blocks: u16,    // 保留的GDT块数

    // 0xE0 - 日志支持
    pub s_journal_uuid: [u8; 16],      // 日志超级块的UUID
    pub s_journal_inum: u32,           // 日志文件的inode号
    pub s_journal_dev: u32,            // 日志设备号
    pub s_last_orphan: u32,            // 孤儿inode列表头
    pub s_hash_seed: [u32; 4],         // HTREE哈希种子
    pub s_def_hash_version: u8,        // 默认哈希版本
    pub s_jnl_backup_type: u8,         // 日志备份类型
    pub s_desc_size: u16,              // 组描述符大小
    pub s_default_mount_opts: u32,     // 默认挂载选项
    pub s_first_meta_bg: u32,          // 第一个元块组

    // 0x100 - 文件系统创建时间
    pub s_mkfs_time: u32,              // 文件系统创建时间
    pub s_jnl_blocks: [u32; 17],       // 日志inode的备份

    // 0x150 - 64位支持
    pub s_blocks_count_hi: u32,        // 块总数（高32位）
    pub s_r_blocks_count_hi: u32,      // 保留块数（高32位）
    pub s_free_blocks_count_hi: u32,   // 空闲块数（高32位）
    pub s_min_extra_isize: u16,        // 所有inode至少有的额外字节数
    pub s_want_extra_isize: u16,       // 新inode应保留的额外字节数
    pub s_flags: u32,                  // 杂项标志
    pub s_raid_stride: u16,            // RAID步长
    pub s_mmp_interval: u16,           // MMP检查等待秒数
    pub s_mmp_block: u64,              // MMP检查块号
    pub s_raid_stripe_width: u32,      // RAID条带上的块数

    // 0x170 - Flexible Block Groups
    pub s_log_groups_per_flex: u8,     // 弹性块组大小
    pub s_checksum_type: u8,           // 元数据校验和算法类型
    pub s_encryption_level: u8,         //加密版本
    pub s_reserved_pad: u8,           // 填充
    pub s_kbytes_written: u64,         // 生命周期写入的KB数
    pub s_snapshot_inum: u32,          // 活动快照的inode号
    pub s_snapshot_id: u32,            // 活动快照的顺序ID
    pub s_snapshot_r_blocks_count: u64,// 快照未来使用的保留块数
    pub s_snapshot_list: u32,          // 快照列表头的inode号

    // 0x194 - 错误信息
    pub s_error_count: u32,            // 文件系统错误数
    pub s_first_error_time: u32,       // 第一次错误时间
    pub s_first_error_ino: u32,        // 第一次错误的inode
    pub s_first_error_block: u64,      // 第一次错误的块号
    pub s_first_error_func: [u8; 32],  // 第一次错误的函数名
    pub s_first_error_line: u32,       // 第一次错误的行号
    pub s_last_error_time: u32,        // 最后一次错误时间
    pub s_last_error_ino: u32,         // 最后一次错误的inode
    pub s_last_error_line: u32,        // 最后一次错误的行号
    pub s_last_error_block: u64,       // 最后一次错误的块号
    pub s_last_error_func: [u8; 32],   // 最后一次错误的函数名

    // 0x1D4 - 挂载选项
    pub s_mount_opts: [u8; 64],        // 挂载选项字符串

    // 0x214 - 用户配额和组配额inode
    pub s_usr_quota_inum: u32,         // 用于跟踪用户配额的inode
    pub s_grp_quota_inum: u32,         // 用于跟踪组配额的inode
    pub s_overhead_blocks: u32,        // 文件系统开销块数
    pub s_backup_bgs: [u32; 2],        // 有稀疏超级块2的块组

    // 0x224 - 加密支持
    pub s_encrypt_algos: [u8; 4],      // 加密算法
    pub s_encrypt_pw_salt: [u8; 16],   // 用于字符串到密钥算法的盐

    // 0x234 - 丢失+找到的inode
    pub s_lpf_ino: u32,                // lost+found的inode位置
    pub s_prj_quota_inum: u32,         // 用于跟踪项目配额的inode
    pub s_checksum_seed: u32,          // 用于元数据校验和的crc32c种子
    pub s_wtime_hi: u8,                // s_wtime的高8位
    pub s_mtime_hi: u8,                // s_mtime的高8位
    pub s_mkfs_time_hi: u8,            // s_mkfs_time的高8位
    pub s_lastcheck_hi: u8,            // s_lastcheck的高8位
    pub s_first_error_time_hi: u8,     // s_first_error_time的高8位
    pub s_last_error_time_hi: u8,      // s_last_error_time的高8位
    pub s_first_error_errcode: u8,     // 第一次错误的错误代码
    pub s_last_error_errcode: u8,      // 最后一次错误的错误代码
    pub s_encoding: u16,               // 文件名编码
    pub s_encoding_flags: u16,         // 文件名编码标志
    pub s_orphan_file_inum: u32,       // 孤儿文件的inode

    // 0x24C - 保留填充到1024字节
    pub s_reserved: [u32; 94],         // 填充到1024字节
    pub s_checksum: u32,               // 超级块校验和
}

impl Default for Ext4Superblock {
    fn default() -> Self {
        Self {
            s_inodes_count: 0,
            s_blocks_count_lo: 0,
            s_r_blocks_count_lo: 0,
            s_free_blocks_count_lo: 0,
            s_free_inodes_count: 0,
            s_first_data_block: 0,
            s_log_block_size: 0,
            s_log_cluster_size: 0,
            s_blocks_per_group: 0,
            s_clusters_per_group: 0,
            s_inodes_per_group: 0,
            s_mtime: 0,
            s_wtime: 0,
            s_mnt_count: 0,
            s_max_mnt_count: 0,
            s_magic: 0,
            s_state: 0,
            s_errors: 0,
            s_minor_rev_level: 0,
            s_lastcheck: 0,
            s_checkinterval: 0,
            s_creator_os: 0,
            s_rev_level: 0,
            s_def_resuid: 0,
            s_def_resgid: 0,
            s_first_ino: 0,
            s_inode_size: 0,
            s_block_group_nr: 0,
            s_feature_compat: Self::EXT4_FEATURE_COMPAT_HAS_JOURNAL,
            s_feature_incompat: Self::EXT4_FEATURE_INCOMPAT_EXTENTS,
            s_feature_ro_compat: Self::EXT4_FEATURE_RO_COMPAT_HUGE_FILE,
            s_uuid: [0; 16],
            s_volume_name: [0; 16],
            s_last_mounted: [0; 64],
            s_algorithm_usage_bitmap: 0,
            s_prealloc_blocks: 0,
            s_prealloc_dir_blocks: 0,
            s_reserved_gdt_blocks: RESERVED_GDT_BLOCKS as u16,  // 使用配置的预留GDT块数
            s_journal_uuid: [0; 16],
            s_journal_inum: JOURNAL_FILE_INODE as u32,
            s_journal_dev: 0,
            s_last_orphan: 0,
            s_hash_seed: [0; 4],
            s_def_hash_version: 1, //默认Legacy版本
            s_jnl_backup_type: 0,
            s_desc_size: 0,
            s_default_mount_opts: 0,
            s_first_meta_bg: 0,
            s_mkfs_time: 0,
            s_jnl_blocks: [0; 17],
            s_blocks_count_hi: 0,
            s_r_blocks_count_hi: 0,
            s_free_blocks_count_hi: 0,
            s_min_extra_isize: 0,
            s_want_extra_isize: 0,
            s_flags: 0,
            s_raid_stride: 0,
            s_mmp_interval: 0,
            s_mmp_block: 0,
            s_raid_stripe_width: 0,
            s_log_groups_per_flex: 0,
            s_checksum_type: 0,
            s_reserved_pad: 0,
            s_kbytes_written: 0,
            s_snapshot_inum: 0,
            s_snapshot_id: 0,
            s_snapshot_r_blocks_count: 0,
            s_snapshot_list: 0,
            s_error_count: 0,
            s_first_error_time: 0,
            s_first_error_ino: 0,
            s_first_error_block: 0,
            s_first_error_func: [0; 32],
            s_first_error_line: 0,
            s_last_error_time: 0,
            s_last_error_ino: 0,
            s_last_error_line: 0,
            s_last_error_block: 0,
            s_last_error_func: [0; 32],
            s_mount_opts: [0; 64],
            s_usr_quota_inum: 0,
            s_grp_quota_inum: 0,
            s_overhead_blocks: 0,
            s_backup_bgs: [0; 2],
            s_encrypt_algos: [0; 4],
            s_encrypt_pw_salt: [0; 16],
            s_lpf_ino: 0,
            s_prj_quota_inum: 0,
            s_checksum_seed: 0,
            s_wtime_hi: 0,
            s_mtime_hi: 0,
            s_mkfs_time_hi: 0,
            s_lastcheck_hi: 0,
            s_first_error_time_hi: 0,
            s_last_error_time_hi: 0,
            s_first_error_errcode: 0,
            s_last_error_errcode: 0,
            s_encoding: 0,
            s_encoding_flags: 0,
            s_orphan_file_inum: 0,
            s_reserved: [0; 94],
            s_checksum: 0,
            s_encryption_level:0
        }
    }
}

impl Ext4Superblock {
    /// EXT4超级块魔数
    pub const EXT4_SUPER_MAGIC: u16 = 0xEF53;
    
    /// 超级块在分区中的偏移量（字节）
    pub const SUPERBLOCK_OFFSET: u64 = 1024;
    
    /// 超级块大小（字节）
    pub const SUPERBLOCK_SIZE: usize = 1024;

    /// 检查超级块魔数是否有效
    pub fn is_valid(&self) -> bool {
        self.s_magic == Self::EXT4_SUPER_MAGIC
    }

    /// 获取块大小（字节）
    pub fn block_size(&self) -> u64 {
        1024 << self.s_log_block_size
    }

    /// 获取块总数（64位）
    pub fn blocks_count(&self) -> u64 {
        (self.s_blocks_count_hi as u64) << 32 | self.s_blocks_count_lo as u64
    }

    /// 获取空闲块数（64位）
    pub fn free_blocks_count(&self) -> u64 {
        (self.s_free_blocks_count_hi as u64) << 32 | self.s_free_blocks_count_lo as u64
    }

    /// 获取保留块数（64位）
    pub fn reserved_blocks_count(&self) -> u64 {
        (self.s_r_blocks_count_hi as u64) << 32 | self.s_r_blocks_count_lo as u64
    }

    /// 获取块组数量
    pub fn block_groups_count(&self) -> u32 {
        let blocks = self.blocks_count();
        let blocks_per_group = self.s_blocks_per_group as u64;
        ((blocks + blocks_per_group - 1) / blocks_per_group) as u32
    }

    /// 每组块数
    pub fn blocks_per_group(&self) -> u32 {
        self.s_blocks_per_group
    }

    /// 每组 inode 数
    pub fn inodes_per_group(&self) -> u32 {
        self.s_inodes_per_group
    }

    /// inode 大小
    pub fn inode_size(&self) -> u16 {
        self.s_inode_size
    }

    /// 每个块能容纳多少个组描述符
    pub fn descs_per_block(&self) -> u32 {
        let block_size = self.block_size() as u32;
        let desc_size = self.s_desc_size as u32;
        if desc_size == 0 { 0 } else { block_size / desc_size }
    }

    /// 每个块组的 inode 表占用多少个块
    pub fn inode_table_blocks(&self) -> u32 {
        let block_size = self.block_size() as u32;
        let inode_size = self.s_inode_size as u32;
        let inodes_per_group = self.s_inodes_per_group;
        if block_size == 0 { 0 } else { (inodes_per_group * inode_size + block_size - 1) / block_size }
    }

    /// 判断兼容特性是否启用
    pub fn has_feature_compat(&self, feature: u32) -> bool {
        self.s_feature_compat & feature != 0
    }

    /// 判断不兼容特性是否启用
    pub fn has_feature_incompat(&self, feature: u32) -> bool {
        self.s_feature_incompat & feature != 0
    }

    /// 判断只读兼容特性是否启用
    pub fn has_feature_ro_compat(&self, feature: u32) -> bool {
        self.s_feature_ro_compat & feature != 0
    }

    /// 是否启用了 extent 特性
    pub fn has_extents(&self) -> bool {
        self.has_feature_incompat(Self::EXT4_FEATURE_INCOMPAT_EXTENTS)
    }

    /// 是否启用了 journal 特性
    pub fn has_journal(&self) -> bool {
        self.has_feature_compat(Self::EXT4_FEATURE_COMPAT_HAS_JOURNAL)
    }
}

// 文件系统状态常量
impl Ext4Superblock {
    pub const EXT4_VALID_FS: u16 = 0x0001;      // 未挂载的干净文件系统
    pub const EXT4_ERROR_FS: u16 = 0x0002;      // 检测到错误的文件系统
    pub const EXT4_ORPHAN_FS: u16 = 0x0004;     // 孤儿正在被恢复
}

// 错误处理方式常量
impl Ext4Superblock {
    pub const EXT4_ERRORS_CONTINUE: u16 = 1;    // 继续执行
    pub const EXT4_ERRORS_RO: u16 = 2;          // 重新挂载为只读
    pub const EXT4_ERRORS_PANIC: u16 = 3;       // 内核恐慌
}

// 创建者操作系统常量
impl Ext4Superblock {
    pub const EXT4_OS_LINUX: u32 = 0;
    pub const EXT4_OS_HURD: u32 = 1;
    pub const EXT4_OS_MASIX: u32 = 2;
    pub const EXT4_OS_FREEBSD: u32 = 3;
    pub const EXT4_OS_LITES: u32 = 4;
}

// 版本号常量
impl Ext4Superblock {
    pub const EXT4_GOOD_OLD_REV: u32 = 0;       // 原始格式
    pub const EXT4_DYNAMIC_REV: u32 = 1;        // 动态inode大小
}

// 兼容特性标志
impl Ext4Superblock {
    // 兼容特性标志
    pub const EXT4_FEATURE_COMPAT_DIR_PREALLOC: u32 = 0x0001;
    pub const EXT4_FEATURE_COMPAT_IMAGIC_INODES: u32 = 0x0002;
    pub const EXT4_FEATURE_COMPAT_HAS_JOURNAL: u32 = 0x0004;
    pub const EXT4_FEATURE_COMPAT_EXT_ATTR: u32 = 0x0008;
    pub const EXT4_FEATURE_COMPAT_RESIZE_INODE: u32 = 0x0010;
    pub const EXT4_FEATURE_COMPAT_DIR_INDEX: u32 = 0x0020;
    pub const EXT4_FEATURE_COMPAT_LAZY_BG: u32 = 0x0040;          
    pub const EXT4_FEATURE_COMPAT_EXCLUDE_INODE: u32 = 0x0080;
    pub const EXT4_FEATURE_COMPAT_EXCLUDE_BITMAP: u32 = 0x0100;
    pub const EXT4_FEATURE_COMPAT_SPARSE_SUPER2: u32 = 0x0200;
    pub const EXT4_FEATURE_COMPAT_FAST_COMMIT: u32 = 0x0400;     
    pub const EXT4_FEATURE_COMPAT_ORPHAN_FILE: u32 = 0x1000;     
}

// 不兼容特性标志
impl Ext4Superblock {
    pub const EXT4_FEATURE_INCOMPAT_COMPRESSION: u32 = 0x0001;
    pub const EXT4_FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;
    pub const EXT4_FEATURE_INCOMPAT_RECOVER: u32 = 0x0004;
    pub const EXT4_FEATURE_INCOMPAT_JOURNAL_DEV: u32 = 0x0008;
    pub const EXT4_FEATURE_INCOMPAT_META_BG: u32 = 0x0010;
    pub const EXT4_FEATURE_INCOMPAT_EXTENTS: u32 = 0x0040;
    pub const EXT4_FEATURE_INCOMPAT_64BIT: u32 = 0x0080;
    pub const EXT4_FEATURE_INCOMPAT_MMP: u32 = 0x0100;
    pub const EXT4_FEATURE_INCOMPAT_FLEX_BG: u32 = 0x0200;
    pub const EXT4_FEATURE_INCOMPAT_EA_INODE: u32 = 0x0400;
    pub const EXT4_FEATURE_INCOMPAT_DIRDATA: u32 = 0x1000;
    pub const EXT4_FEATURE_INCOMPAT_CSUM_SEED: u32 = 0x2000;
    pub const EXT4_FEATURE_INCOMPAT_LARGEDIR: u32 = 0x4000;
    pub const EXT4_FEATURE_INCOMPAT_INLINE_DATA: u32 = 0x8000;
    pub const EXT4_FEATURE_INCOMPAT_ENCRYPT: u32 = 0x10000;
}

// 只读兼容特性标志
impl Ext4Superblock {
    pub const EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
    pub const EXT4_FEATURE_RO_COMPAT_LARGE_FILE: u32 = 0x0002;
    pub const EXT4_FEATURE_RO_COMPAT_BTREE_DIR: u32 = 0x0004;
    pub const EXT4_FEATURE_RO_COMPAT_HUGE_FILE: u32 = 0x0008;
    pub const EXT4_FEATURE_RO_COMPAT_GDT_CSUM: u32 = 0x0010;
    pub const EXT4_FEATURE_RO_COMPAT_DIR_NLINK: u32 = 0x0020;
    pub const EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE: u32 = 0x0040;
    pub const EXT4_FEATURE_RO_COMPAT_HAS_SNAPSHOT: u32 = 0x0080;  
    pub const EXT4_FEATURE_RO_COMPAT_QUOTA: u32 = 0x0100;
    pub const EXT4_FEATURE_RO_COMPAT_BIGALLOC: u32 = 0x0200;
    pub const EXT4_FEATURE_RO_COMPAT_METADATA_CSUM: u32 = 0x0400;
    pub const EXT4_FEATURE_RO_COMPAT_REPLICA: u32 = 0x0800;       
    pub const EXT4_FEATURE_RO_COMPAT_READONLY: u32 = 0x1000;
    pub const EXT4_FEATURE_RO_COMPAT_PROJECT: u32 = 0x2000;
    pub const EXT4_FEATURE_RO_COMPAT_VERITY: u32 = 0x8000;       
    pub const EXT4_FEATURE_RO_COMPAT_ORPHAN_PRESENT: u32 = 0x10000; 
}

// 实现 DiskFormat trait，用于小端序列化/反序列化超级块


impl DiskFormat for Ext4Superblock {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let mut sb = Self::default();
        let mut offset = 0;
        
        // Macro to read u32
        macro_rules! read_u32 {
            () => {{
                let val = read_u32_le(&bytes[offset..]);
                offset += 4;
                val
            }};
        }
        
        // Macro to read u16
        macro_rules! read_u16 {
            () => {{
                let val = read_u16_le(&bytes[offset..]);
                offset += 2;
                val
            }};
        }
        
        // Macro to read u64
        macro_rules! read_u64 {
            () => {{
                let val = read_u64_le(&bytes[offset..]);
                offset += 8;
                val
            }};
        }
        
        // Macro to read u8
        macro_rules! read_u8 {
            () => {{
                let val = bytes[offset];
                offset += 1;
                val
            }};
        }
        
        // Macro to read byte array
        macro_rules! read_bytes {
            ($len:expr) => {{
                let mut arr = [0u8; $len];
                arr.copy_from_slice(&bytes[offset..offset + $len]);
                offset += $len;
                arr
            }};
        }
        
        // Macro to read u32 array
        macro_rules! read_u32_array {
            ($len:expr) => {{
                let mut arr = [0u32; $len];
                for i in 0..$len {
                    arr[i] = read_u32!();
                }
                arr
            }};
        }
        
        // 0x00 - Basic filesystem information
        sb.s_inodes_count = read_u32!();
        sb.s_blocks_count_lo = read_u32!();
        sb.s_r_blocks_count_lo = read_u32!();
        sb.s_free_blocks_count_lo = read_u32!();
        sb.s_free_inodes_count = read_u32!();
        sb.s_first_data_block = read_u32!();
        sb.s_log_block_size = read_u32!();
        sb.s_log_cluster_size = read_u32!();
        sb.s_blocks_per_group = read_u32!();
        sb.s_clusters_per_group = read_u32!();
        sb.s_inodes_per_group = read_u32!();
        sb.s_mtime = read_u32!();
        sb.s_wtime = read_u32!();
        
        // 0x34 - Mount count and check
        sb.s_mnt_count = read_u16!();
        sb.s_max_mnt_count = read_u16!();
        sb.s_magic = read_u16!();
        sb.s_state = read_u16!();
        sb.s_errors = read_u16!();
        sb.s_minor_rev_level = read_u16!();
        sb.s_lastcheck = read_u32!();
        sb.s_checkinterval = read_u32!();
        sb.s_creator_os = read_u32!();
        sb.s_rev_level = read_u32!();
        sb.s_def_resuid = read_u16!();
        sb.s_def_resgid = read_u16!();
        
        // 0x54 - EXT4_DYNAMIC_REV extended fields
        sb.s_first_ino = read_u32!();
        sb.s_inode_size = read_u16!();
        sb.s_block_group_nr = read_u16!();
        sb.s_feature_compat = read_u32!();
        sb.s_feature_incompat = read_u32!();
        sb.s_feature_ro_compat = read_u32!();
        sb.s_uuid = read_bytes!(16);
        sb.s_volume_name = read_bytes!(16);
        sb.s_last_mounted = read_bytes!(64);
        sb.s_algorithm_usage_bitmap = read_u32!();
        
        // 0xDC - Performance hints
        sb.s_prealloc_blocks = read_u8!();
        sb.s_prealloc_dir_blocks = read_u8!();
        sb.s_reserved_gdt_blocks = read_u16!();
        
        // 0xE0 - Journaling support
        sb.s_journal_uuid = read_bytes!(16);
        sb.s_journal_inum = read_u32!();
        sb.s_journal_dev = read_u32!();
        sb.s_last_orphan = read_u32!();
        sb.s_hash_seed = read_u32_array!(4);
        sb.s_def_hash_version = read_u8!();
        sb.s_jnl_backup_type = read_u8!();
        sb.s_desc_size = read_u16!();
        sb.s_default_mount_opts = read_u32!();
        sb.s_first_meta_bg = read_u32!();
        
        // 0x100 - Filesystem creation time
        sb.s_mkfs_time = read_u32!();
        sb.s_jnl_blocks = read_u32_array!(17);
        
        // 0x150 - 64bit support
        sb.s_blocks_count_hi = read_u32!();
        sb.s_r_blocks_count_hi = read_u32!();
        sb.s_free_blocks_count_hi = read_u32!();
        sb.s_min_extra_isize = read_u16!();
        sb.s_want_extra_isize = read_u16!();
        sb.s_flags = read_u32!();
        sb.s_raid_stride = read_u16!();
        sb.s_mmp_interval = read_u16!();
        sb.s_mmp_block = read_u64!();
        sb.s_raid_stripe_width = read_u32!();
        
        // 0x170 - Flexible Block Groups
        sb.s_log_groups_per_flex = read_u8!();
        sb.s_checksum_type = read_u8!();
        sb.s_encryption_level = read_u8!();
        sb.s_reserved_pad = read_u8!();
        sb.s_kbytes_written = read_u64!();
        sb.s_snapshot_inum = read_u32!();
        sb.s_snapshot_id = read_u32!();
        sb.s_snapshot_r_blocks_count = read_u64!();
        sb.s_snapshot_list = read_u32!();
        
        // 0x194 - Error information
        sb.s_error_count = read_u32!();
        sb.s_first_error_time = read_u32!();
        sb.s_first_error_ino = read_u32!();
        sb.s_first_error_block = read_u64!();
        sb.s_first_error_func = read_bytes!(32);
        sb.s_first_error_line = read_u32!();
        sb.s_last_error_time = read_u32!();
        sb.s_last_error_ino = read_u32!();
        sb.s_last_error_line = read_u32!();
        sb.s_last_error_block = read_u64!();
        sb.s_last_error_func = read_bytes!(32);
        
        // 0x1D4 - Mount options
        sb.s_mount_opts = read_bytes!(64);
        
        // 0x214 - User and group quota inodes
        sb.s_usr_quota_inum = read_u32!();
        sb.s_grp_quota_inum = read_u32!();
        sb.s_overhead_blocks = read_u32!();
        sb.s_backup_bgs = [read_u32!(), read_u32!()];
        
        // 0x224 - Encryption support
        sb.s_encrypt_algos = read_bytes!(4);
        sb.s_encrypt_pw_salt = read_bytes!(16);
        
        // 0x234 - Lost+found inode
        sb.s_lpf_ino = read_u32!();
        sb.s_prj_quota_inum = read_u32!();
        sb.s_checksum_seed = read_u32!();
        sb.s_wtime_hi = read_u8!();
        sb.s_mtime_hi = read_u8!();
        sb.s_mkfs_time_hi = read_u8!();
        sb.s_lastcheck_hi = read_u8!();
        sb.s_first_error_time_hi = read_u8!();
        sb.s_last_error_time_hi = read_u8!();
        sb.s_first_error_errcode = read_u8!();
        sb.s_last_error_errcode = read_u8!();
        sb.s_encoding = read_u16!();
        sb.s_encoding_flags = read_u16!();
        sb.s_orphan_file_inum = read_u32!();
        
        // 0x24C - Reserved padding to 1024 bytes
        sb.s_reserved = read_u32_array!(94);
        sb.s_checksum = read_u32!();
        
        sb
    }
    
    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        let mut offset = 0;
        
        // Macro to write u32
        macro_rules! write_u32 {
            ($val:expr) => {{
                write_u32_le($val, &mut bytes[offset..]);
                offset += 4;
            }};
        }
        
        // Macro to write u16
        macro_rules! write_u16 {
            ($val:expr) => {{
                write_u16_le($val, &mut bytes[offset..]);
                offset += 2;
            }};
        }
        
        // Macro to write u64
        macro_rules! write_u64 {
            ($val:expr) => {{
                write_u64_le($val, &mut bytes[offset..]);
                offset += 8;
            }};
        }
        
        // Macro to write u8
        macro_rules! write_u8 {
            ($val:expr) => {{
                bytes[offset] = $val;
                offset += 1;
            }};
        }
        
        // Macro to write byte array
        macro_rules! write_bytes {
            ($arr:expr) => {{
                let len = $arr.len();
                bytes[offset..offset + len].copy_from_slice(&$arr);
                offset += len;
            }};
        }
        
        // Macro to write u32 array
        macro_rules! write_u32_array {
            ($arr:expr) => {{
                for val in $arr.iter() {
                    write_u32!(*val);
                }
            }};
        }
        
        // 0x00 - Basic filesystem information
        write_u32!(self.s_inodes_count);
        write_u32!(self.s_blocks_count_lo);
        write_u32!(self.s_r_blocks_count_lo);
        write_u32!(self.s_free_blocks_count_lo);
        write_u32!(self.s_free_inodes_count);
        write_u32!(self.s_first_data_block);
        write_u32!(self.s_log_block_size);
        write_u32!(self.s_log_cluster_size);
        write_u32!(self.s_blocks_per_group);
        write_u32!(self.s_clusters_per_group);
        write_u32!(self.s_inodes_per_group);
        write_u32!(self.s_mtime);
        write_u32!(self.s_wtime);
        
        // 0x34 - Mount count and check
        write_u16!(self.s_mnt_count);
        write_u16!(self.s_max_mnt_count);
        write_u16!(self.s_magic);
        write_u16!(self.s_state);
        write_u16!(self.s_errors);
        write_u16!(self.s_minor_rev_level);
        write_u32!(self.s_lastcheck);
        write_u32!(self.s_checkinterval);
        write_u32!(self.s_creator_os);
        write_u32!(self.s_rev_level);
        write_u16!(self.s_def_resuid);
        write_u16!(self.s_def_resgid);
        
        // 0x54 - EXT4_DYNAMIC_REV extended fields
        write_u32!(self.s_first_ino);
        write_u16!(self.s_inode_size);
        write_u16!(self.s_block_group_nr);
        write_u32!(self.s_feature_compat);
        write_u32!(self.s_feature_incompat);
        write_u32!(self.s_feature_ro_compat);
        write_bytes!(self.s_uuid);
        write_bytes!(self.s_volume_name);
        write_bytes!(self.s_last_mounted);
        write_u32!(self.s_algorithm_usage_bitmap);
        
        // 0xDC - Performance hints
        write_u8!(self.s_prealloc_blocks);
        write_u8!(self.s_prealloc_dir_blocks);
        write_u16!(self.s_reserved_gdt_blocks);
        
        // 0xE0 - Journaling support
        write_bytes!(self.s_journal_uuid);
        write_u32!(self.s_journal_inum);
        write_u32!(self.s_journal_dev);
        write_u32!(self.s_last_orphan);
        write_u32_array!(self.s_hash_seed);
        write_u8!(self.s_def_hash_version);
        write_u8!(self.s_jnl_backup_type);
        write_u16!(self.s_desc_size);
        write_u32!(self.s_default_mount_opts);
        write_u32!(self.s_first_meta_bg);
        
        // 0x100 - Filesystem creation time
        write_u32!(self.s_mkfs_time);
        write_u32_array!(self.s_jnl_blocks);
        
        // 0x150 - 64bit support
        write_u32!(self.s_blocks_count_hi);
        write_u32!(self.s_r_blocks_count_hi);
        write_u32!(self.s_free_blocks_count_hi);
        write_u16!(self.s_min_extra_isize);
        write_u16!(self.s_want_extra_isize);
        write_u32!(self.s_flags);
        write_u16!(self.s_raid_stride);
        write_u16!(self.s_mmp_interval);
        write_u64!(self.s_mmp_block);
        write_u32!(self.s_raid_stripe_width);
        
        // 0x170 - Flexible Block Groups
        write_u8!(self.s_log_groups_per_flex);
        write_u8!(self.s_checksum_type);
        write_u8!(self.s_encryption_level);
        write_u8!(self.s_reserved_pad);
        write_u64!(self.s_kbytes_written);
        write_u32!(self.s_snapshot_inum);
        write_u32!(self.s_snapshot_id);
        write_u64!(self.s_snapshot_r_blocks_count);
        write_u32!(self.s_snapshot_list);
        
        // 0x194 - Error information
        write_u32!(self.s_error_count);
        write_u32!(self.s_first_error_time);
        write_u32!(self.s_first_error_ino);
        write_u64!(self.s_first_error_block);
        write_bytes!(self.s_first_error_func);
        write_u32!(self.s_first_error_line);
        write_u32!(self.s_last_error_time);
        write_u32!(self.s_last_error_ino);
        write_u32!(self.s_last_error_line);
        write_u64!(self.s_last_error_block);
        write_bytes!(self.s_last_error_func);
        
        // 0x1D4 - Mount options
        write_bytes!(self.s_mount_opts);
        
        // 0x214 - User and group quota inodes
        write_u32!(self.s_usr_quota_inum);
        write_u32!(self.s_grp_quota_inum);
        write_u32!(self.s_overhead_blocks);
        write_u32!(self.s_backup_bgs[0]);
        write_u32!(self.s_backup_bgs[1]);
        
        // 0x224 - Encryption support
        write_bytes!(self.s_encrypt_algos);
        write_bytes!(self.s_encrypt_pw_salt);
        
        // 0x234 - Lost+found inode
        write_u32!(self.s_lpf_ino);
        write_u32!(self.s_prj_quota_inum);
        write_u32!(self.s_checksum_seed);
        write_u8!(self.s_wtime_hi);
        write_u8!(self.s_mtime_hi);
        write_u8!(self.s_mkfs_time_hi);
        write_u8!(self.s_lastcheck_hi);
        write_u8!(self.s_first_error_time_hi);
        write_u8!(self.s_last_error_time_hi);
        write_u8!(self.s_first_error_errcode);
        write_u8!(self.s_last_error_errcode);
        write_u16!(self.s_encoding);
        write_u16!(self.s_encoding_flags);
        write_u32!(self.s_orphan_file_inum);
        
        // 0x24C - Reserved padding to 1024 bytes
        write_u32_array!(self.s_reserved);
        write_u32!(self.s_checksum);
    }
    
    fn disk_size() -> usize {
        Self::SUPERBLOCK_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_superblock_disk_format_roundtrip() {
        // 创建测试超级块
        let mut sb = Ext4Superblock::default();
        sb.s_magic = Ext4Superblock::EXT4_SUPER_MAGIC;
        sb.s_inodes_count = 1024;
        sb.s_blocks_count_lo = 32768;
        sb.s_blocks_count_hi = 0;
        sb.s_log_block_size = 2; // 4KB blocks
        sb.s_blocks_per_group = 8192;
        sb.s_inodes_per_group = 256;
        sb.s_uuid = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        sb.s_hash_seed = [0x12345678, 0x9ABCDEF0, 0x11111111, 0x22222222];
        sb.s_inode_size = 256;
        sb.s_rev_level = Ext4Superblock::EXT4_DYNAMIC_REV;
        
        // 序列化
        let mut bytes = [0u8; 1024];
        sb.to_disk_bytes(&mut bytes);
        
        // 验证魔数位置正确（偏移 0x38）
        assert_eq!(bytes[0x38], 0x53);
        assert_eq!(bytes[0x39], 0xEF);
        
        // 反序列化
        let sb2 = Ext4Superblock::from_disk_bytes(&bytes);
        
        // 验证关键字段
        assert_eq!(sb2.s_magic, Ext4Superblock::EXT4_SUPER_MAGIC);
        assert_eq!(sb2.s_inodes_count, 1024);
        assert_eq!(sb2.s_blocks_count_lo, 32768);
        assert_eq!(sb2.s_blocks_count_hi, 0);
        assert_eq!(sb2.s_log_block_size, 2);
        assert_eq!(sb2.s_blocks_per_group, 8192);
        assert_eq!(sb2.s_inodes_per_group, 256);
        assert_eq!(sb2.s_uuid, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        assert_eq!(sb2.s_hash_seed, [0x12345678, 0x9ABCDEF0, 0x11111111, 0x22222222]);
        assert_eq!(sb2.s_inode_size, 256);
        assert_eq!(sb2.s_rev_level, Ext4Superblock::EXT4_DYNAMIC_REV);
        assert!(sb2.is_valid());
    }
    
    #[test]
    fn test_superblock_disk_size() {
        assert_eq!(Ext4Superblock::disk_size(), 1024);
    }
    
    #[test]
    fn test_superblock_64bit_values() {
        let mut sb = Ext4Superblock::default();
        sb.s_blocks_count_lo = 0xFFFFFFFF;
        sb.s_blocks_count_hi = 0x00000001;
        
        let mut bytes = [0u8; 1024];
        sb.to_disk_bytes(&mut bytes);
        
        let sb2 = Ext4Superblock::from_disk_bytes(&bytes);
        
        // 验证 64位值正确
        assert_eq!(sb2.blocks_count(), 0x1FFFFFFFF);
        assert_eq!(sb2.s_blocks_count_lo, 0xFFFFFFFF);
        assert_eq!(sb2.s_blocks_count_hi, 0x00000001);
    }
}
