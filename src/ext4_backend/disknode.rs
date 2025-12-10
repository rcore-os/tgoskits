use core::mem::transmute;

use alloc::vec::Vec;
use log::error;
use crate::ext4_backend::jbd2::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::jbd2::jbdstruct::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::superblock::*;
use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::entries::*;
use crate::ext4_backend::mkfile::*;
use crate::ext4_backend::*;
use crate::ext4_backend::bmalloc::*;
use crate::ext4_backend::bitmap_cache::*;
use crate::ext4_backend::datablock_cache::*;
use crate::ext4_backend::inodetable_cache::*;
use crate::ext4_backend::blockgroup_description::*;
use crate::ext4_backend::mkd::*;
use crate::ext4_backend::tool::*;
use crate::ext4_backend::jbd2::jbd2::*;
use crate::ext4_backend::ext4::*;


/// Ext4 磁盘Inode结构
/// Inode是文件系统中存储文件元数据的核心数据结构
/// 每个文件和目录都有一个对应的inode
#[repr(C)]
#[derive(Debug, Clone, Copy,Default)]
pub struct Ext4Inode {
    // 0x00 - 基本文件属性
    pub i_mode: u16,                   // 文件模式（类型和权限）
    pub i_uid: u16,                    // 所有者用户ID（低16位）
    pub i_size_lo: u32,                // 文件大小（低32位，字节）
    pub i_atime: u32,                  // 访问时间（秒）
    pub i_ctime: u32,                  // 状态改变时间（秒）
    pub i_mtime: u32,                  // 修改时间（秒）
    pub i_dtime: u32,                  // 删除时间（秒）
    pub i_gid: u16,                    // 所有者组ID（低16位）
    pub i_links_count: u16,            // 硬链接计数
    pub i_blocks_lo: u32,              // 块计数（低32位）
    pub i_flags: u32,                  // 文件标志

    // 0x20 - 操作系统相关字段1
    pub l_i_version: u32,              // Linux：inode版本（用于NFS）

    // 0x24 - 数据块指针（60字节）
    pub i_block: [u32; 15],            // 块指针数组或extent树

    // 0x64 - 文件版本和属性
    pub i_generation: u32,             // 文件版本（用于NFS）
    pub i_file_acl_lo: u32,            // 扩展属性块（低32位）
    pub i_size_high: u32,              // 文件大小（高32位）或目录ACL
    pub i_obso_faddr: u32,             // 废弃的碎片地址

    // 0x74 - 操作系统相关字段2（12字节）
    pub l_i_blocks_high: u16,          // 块计数（高16位）
    pub l_i_file_acl_high: u16,        // 扩展属性块（高16位）
    pub l_i_uid_high: u16,             // 所有者用户ID（高16位）
    pub l_i_gid_high: u16,             // 所有者组ID（高16位）
    pub l_i_checksum_lo: u16,          // inode校验和（低16位）
    pub l_i_reserved: u16,             // 保留字段

    // 0x80 - 扩展字段（对于大inode）
    pub i_extra_isize: u16,            // 额外inode大小
    pub i_checksum_hi: u16,            // inode校验和（高16位）
    pub i_ctime_extra: u32,            // 额外的状态改变时间（纳秒 + epoch高2位）
    pub i_mtime_extra: u32,            // 额外的修改时间（纳秒 + epoch高2位）
    pub i_atime_extra: u32,            // 额外的访问时间（纳秒 + epoch高2位）
    pub i_crtime: u32,                 // 创建时间（秒）
    pub i_crtime_extra: u32,           // 额外的创建时间（纳秒 + epoch高2位）
    pub i_version_hi: u32,             // 版本号（高32位）
    pub i_projid: u32,                 // 项目ID
}

impl Ext4Inode {
    
    /// 写入初始extend header便捷函数
    pub fn write_extend_header(&mut self){
        let per_extent_header_offset = Ext4ExtentHeader::disk_size();
        let current_offset = 0;
        let mut extent_buffer :[u8;60]= [0;60];
        let header = Ext4ExtentHeader::new();
        //写入header
        header.to_disk_bytes(&mut extent_buffer[current_offset..current_offset+per_extent_header_offset]);
        //转换写回
        
        let new_slice:[u32;15] = unsafe {
            transmute(extent_buffer)
        };
        self.i_block.copy_from_slice(&new_slice);
    }

    /// 标准inode大小（128字节）
    pub const GOOD_OLD_INODE_SIZE: u16 = 128;
    
    /// 大inode默认大小（256字节）
    pub const LARGE_INODE_SIZE: u16 = 256;

    /// 获取完整的文件大小（64位）
    pub fn size(&self) -> u64 {
        (self.i_size_high as u64) << 32 | self.i_size_lo as u64
    }

    /// 获取完整的块数（48位）
    pub fn blocks_count(&self) -> u64 {
        (self.l_i_blocks_high as u64) << 32 | self.i_blocks_lo as u64
    }

    /// 获取完整的UID（32位）
    pub fn uid(&self) -> u32 {
        (self.l_i_uid_high as u32) << 16 | self.i_uid as u32
    }

    /// 获取完整的GID（32位）
    pub fn gid(&self) -> u32 {
        (self.l_i_gid_high as u32) << 16 | self.i_gid as u32
    }

    /// 获取完整的扩展属性块号（48位）
    pub fn file_acl(&self) -> u64 {
        (self.l_i_file_acl_high as u64) << 32 | self.i_file_acl_lo as u64
    }

    /// 检查是否是目录
    pub fn is_dir(&self) -> bool {
        self.i_mode & Self::S_IFMT == Self::S_IFDIR
    }

    /// 检查是否是普通文件
    pub fn is_file(&self) -> bool {
        self.i_mode & Self::S_IFMT == Self::S_IFREG
    }

    /// 检查是否是符号链接
    pub fn is_symlink(&self) -> bool {
        self.i_mode & Self::S_IFMT == Self::S_IFLNK
    }

    /// 检查是否使用extent树
    pub fn is_extent(&self) -> bool {
        self.i_flags & Self::EXT4_EXTENTS_FL != 0
    }
    ///检查是否有extend树的结构
    pub fn have_extend_header(&self)->bool{
        unsafe {
           let header_ptr =  self.i_block.as_ptr() as *const _ as *const Ext4ExtentHeader;
           if (*header_ptr).eh_magic==Ext4ExtentHeader::EXT4_EXT_MAGIC {
               return true;
           }else {
            use log::info;
               info!("No tree header!!!");
               return false;
           }
        }
    }
}

// 文件模式常量 - 文件类型
impl Ext4Inode {
    pub const S_IFMT: u16 = 0xF000;     // 文件类型位掩码
    pub const S_IFSOCK: u16 = 0xC000;   // 套接字
    pub const S_IFLNK: u16 = 0xA000;    // 符号链接
    pub const S_IFREG: u16 = 0x8000;    // 普通文件
    pub const S_IFBLK: u16 = 0x6000;    // 块设备
    pub const S_IFDIR: u16 = 0x4000;    // 目录
    pub const S_IFCHR: u16 = 0x2000;    // 字符设备
    pub const S_IFIFO: u16 = 0x1000;    // FIFO
}

// 文件模式常量 - 权限位
impl Ext4Inode {
    pub const S_ISUID: u16 = 0x0800;    // 设置UID位
    pub const S_ISGID: u16 = 0x0400;    // 设置GID位
    pub const S_ISVTX: u16 = 0x0200;    // 粘滞位
    pub const S_IRWXU: u16 = 0x01C0;    // 所有者权限掩码
    pub const S_IRUSR: u16 = 0x0100;    // 所有者读权限
    pub const S_IWUSR: u16 = 0x0080;    // 所有者写权限
    pub const S_IXUSR: u16 = 0x0040;    // 所有者执行权限
    pub const S_IRWXG: u16 = 0x0038;    // 组权限掩码
    pub const S_IRGRP: u16 = 0x0020;    // 组读权限
    pub const S_IWGRP: u16 = 0x0010;    // 组写权限
    pub const S_IXGRP: u16 = 0x0008;    // 组执行权限
    pub const S_IRWXO: u16 = 0x0007;    // 其他用户权限掩码
    pub const S_IROTH: u16 = 0x0004;    // 其他用户读权限
    pub const S_IWOTH: u16 = 0x0002;    // 其他用户写权限
    pub const S_IXOTH: u16 = 0x0001;    // 其他用户执行权限
}

// Inode标志常量
impl Ext4Inode {
    pub const EXT4_SECRM_FL: u32 = 0x00000001;        // 安全删除
    pub const EXT4_UNRM_FL: u32 = 0x00000002;         // 可恢复删除
    pub const EXT4_COMPR_FL: u32 = 0x00000004;        // 压缩文件
    pub const EXT4_SYNC_FL: u32 = 0x00000008;         // 同步更新
    pub const EXT4_IMMUTABLE_FL: u32 = 0x00000010;    // 不可修改
    pub const EXT4_APPEND_FL: u32 = 0x00000020;       // 只能追加
    pub const EXT4_NODUMP_FL: u32 = 0x00000040;       // 不转储
    pub const EXT4_NOATIME_FL: u32 = 0x00000080;      // 不更新访问时间
    pub const EXT4_DIRTY_FL: u32 = 0x00000100;        // 脏数据
    pub const EXT4_COMPRBLK_FL: u32 = 0x00000200;     // 一个或多个压缩簇
    pub const EXT4_NOCOMPR_FL: u32 = 0x00000400;      // 不压缩
    pub const EXT4_ENCRYPT_FL: u32 = 0x00000800;      // 加密文件
    pub const EXT4_INDEX_FL: u32 = 0x00001000;        // 哈希索引目录
    pub const EXT4_IMAGIC_FL: u32 = 0x00002000;       // AFS目录
    pub const EXT4_JOURNAL_DATA_FL: u32 = 0x00004000; // 日志文件数据
    pub const EXT4_NOTAIL_FL: u32 = 0x00008000;       // 文件尾不合并
    pub const EXT4_DIRSYNC_FL: u32 = 0x00010000;      // 目录同步更新
    pub const EXT4_TOPDIR_FL: u32 = 0x00020000;       // 顶层目录
    pub const EXT4_HUGE_FILE_FL: u32 = 0x00040000;    // 巨大文件
    pub const EXT4_EXTENTS_FL: u32 = 0x00080000;      // 使用extent树
    pub const EXT4_EA_INODE_FL: u32 = 0x00200000;     // 扩展属性inode
    pub const EXT4_EOFBLOCKS_FL: u32 = 0x00400000;    // EOF后的块
    pub const EXT4_SNAPFILE_FL: u32 = 0x01000000;     // 快照文件
    pub const EXT4_SNAPFILE_DELETED_FL: u32 = 0x04000000; // 快照被删除
    pub const EXT4_SNAPFILE_SHRUNK_FL: u32 = 0x08000000;  // 快照收缩
    pub const EXT4_INLINE_DATA_FL: u32 = 0x10000000;  // 内联数据
    pub const EXT4_PROJINHERIT_FL: u32 = 0x20000000;  // 创建时继承项目ID
    pub const EXT4_RESERVED_FL: u32 = 0x80000000;     // 保留
}

/// Extent头部结构
/// 用于extent树的节点头部
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4ExtentHeader {
    pub eh_magic: u16,          // 魔数 0xF30A
    pub eh_entries: u16,        // 有效条目数
    pub eh_max: u16,            // 容量
    pub eh_depth: u16,          // 树的深度
    pub eh_generation: u32,     // 生成号
}

impl Ext4ExtentHeader {
    pub const EXT4_EXT_MAGIC: u16 = 0xF30A;
    ///默认根节点配置 4个条目 最大容量 深度 生成号
    pub fn new()->Self{
        Self { eh_magic: Self::EXT4_EXT_MAGIC, eh_entries: 0, eh_max: 4, eh_depth: 0, eh_generation: 0 }
    }
}

/// Extent索引结构
/// 用于extent树的内部节点
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4ExtentIdx {
    pub ei_block: u32,          // 此索引覆盖的第一个逻辑块
    pub ei_leaf_lo: u32,        // 指向下一层的物理块（低32位）
    pub ei_leaf_hi: u16,        // 指向下一层的物理块（高16位）
    pub ei_unused: u16,         // 保留未使用
}

/// Extent叶子结构
/// 用于extent树的叶子节点，表示实际的数据块映射
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4Extent {
    pub ee_block: u32,          // 第一个逻辑块
    pub ee_len: u16,            // extent长度（块数）
    pub ee_start_hi: u16,       // 物理块号（高16位）
    pub ee_start_lo: u32,       // 物理块号（低32位）
}


impl Default for Ext4Extent {
    fn default() -> Self {
        Self { ee_block: 0, ee_len: Self::EXT_INIT_MAX_LEN, ee_start_hi: 0, ee_start_lo: 0 }
    }
}
impl Ext4Extent {
 


    /// extent最大长度（已初始化）
    pub const EXT_INIT_MAX_LEN: u16 = 32768;
    
    /// extent最大长度（未初始化）
    pub const EXT_UNINIT_MAX_LEN: u16 = 32768;

    ///默认配置
    pub fn new(logic_start:u32,start_phy_block:u64,len:u16)->Self{
        let high = (start_phy_block >> 32) as u16;
        let low = (start_phy_block & 0xffffffff) as u32;
        Self { ee_block: logic_start, ee_len: len, ee_start_hi: high, ee_start_lo: low }
    }

    /// 获取完整的起始物理块号（48位）
    pub fn start_block(&self) -> u64 {
        (self.ee_start_hi as u64) << 32 | self.ee_start_lo as u64
    }

    /// 检查extent是否已初始化
    pub fn is_initialized(&self) -> bool {
        self.ee_len <= Self::EXT_INIT_MAX_LEN
    }
}

/// 实现 DiskFormat trait 用于字节序转换
impl DiskFormat for Ext4ExtentHeader {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        Self {
            eh_magic: read_u16_le(&bytes[0..2]),
            eh_entries: read_u16_le(&bytes[2..4]),
            eh_max: read_u16_le(&bytes[4..6]),
            eh_depth: read_u16_le(&bytes[6..8]),
            eh_generation: read_u32_le(&bytes[8..12]),
        }
    }
    
    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        write_u16_le(self.eh_magic, &mut bytes[0..2]);
        write_u16_le(self.eh_entries, &mut bytes[2..4]);
        write_u16_le(self.eh_max, &mut bytes[4..6]);
        write_u16_le(self.eh_depth, &mut bytes[6..8]);
        write_u32_le(self.eh_generation, &mut bytes[8..12]);
    }
    
    fn disk_size() -> usize {
        12
    }
}

/// 实现 DiskFormat trait 用于字节序转换
impl DiskFormat for Ext4ExtentIdx {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        Self {
            ei_block: read_u32_le(&bytes[0..4]),
            ei_leaf_lo: read_u32_le(&bytes[4..8]),
            ei_leaf_hi: read_u16_le(&bytes[8..10]),
            ei_unused: read_u16_le(&bytes[10..12]),
        }
    }
    
    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        write_u32_le(self.ei_block, &mut bytes[0..4]);
        write_u32_le(self.ei_leaf_lo, &mut bytes[4..8]);
        write_u16_le(self.ei_leaf_hi, &mut bytes[8..10]);
        write_u16_le(self.ei_unused, &mut bytes[10..12]);
    }
    
    fn disk_size() -> usize {
        12
    }
}

/// 实现 DiskFormat trait 用于字节序转换
impl DiskFormat for Ext4Extent {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        Self {
            ee_block: read_u32_le(&bytes[0..4]),
            ee_len: read_u16_le(&bytes[4..6]),
            ee_start_hi: read_u16_le(&bytes[6..8]),
            ee_start_lo: read_u32_le(&bytes[8..12]),
        }
    }
    
    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        write_u32_le(self.ee_block, &mut bytes[0..4]);
        write_u16_le(self.ee_len, &mut bytes[4..6]);
        write_u16_le(self.ee_start_hi, &mut bytes[6..8]);
        write_u32_le(self.ee_start_lo, &mut bytes[8..12]);
    }
    
    fn disk_size() -> usize {
        12
    }
}

/// 实现 DiskFormat trait for Ext4Inode
impl DiskFormat for Ext4Inode {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let mut inode = Self {
            i_mode: read_u16_le(&bytes[0..2]),
            i_uid: read_u16_le(&bytes[2..4]),
            i_size_lo: read_u32_le(&bytes[4..8]),
            i_atime: read_u32_le(&bytes[8..12]),
            i_ctime: read_u32_le(&bytes[12..16]),
            i_mtime: read_u32_le(&bytes[16..20]),
            i_dtime: read_u32_le(&bytes[20..24]),
            i_gid: read_u16_le(&bytes[24..26]),
            i_links_count: read_u16_le(&bytes[26..28]),
            i_blocks_lo: read_u32_le(&bytes[28..32]),
            i_flags: read_u32_le(&bytes[32..36]),
            l_i_version: read_u32_le(&bytes[36..40]),
            i_block: [0; 15],
            i_generation: read_u32_le(&bytes[100..104]),
            i_file_acl_lo: read_u32_le(&bytes[104..108]),
            i_size_high: read_u32_le(&bytes[108..112]),
            i_obso_faddr: read_u32_le(&bytes[112..116]),
            l_i_blocks_high: read_u16_le(&bytes[116..118]),
            l_i_file_acl_high: read_u16_le(&bytes[118..120]),
            l_i_uid_high: read_u16_le(&bytes[120..122]),
            l_i_gid_high: read_u16_le(&bytes[122..124]),
            l_i_checksum_lo: read_u16_le(&bytes[124..126]),
            l_i_reserved: read_u16_le(&bytes[126..128]),
            i_extra_isize: 0,
            i_checksum_hi: 0,
            i_ctime_extra: 0,
            i_mtime_extra: 0,
            i_atime_extra: 0,
            i_crtime: 0,
            i_crtime_extra: 0,
            i_version_hi: 0,
            i_projid: 0,
        };
        
        // 读取i_block数组
        for i in 0..15 {
            inode.i_block[i] = read_u32_le(&bytes[40 + i * 4..44 + i * 4]);
        }
        
        // 如果是大inode（256字节），读取额外字段
        if bytes.len() >= 256 {
            inode.i_extra_isize = read_u16_le(&bytes[128..130]);
            inode.i_checksum_hi = read_u16_le(&bytes[130..132]);
            inode.i_ctime_extra = read_u32_le(&bytes[132..136]);
            inode.i_mtime_extra = read_u32_le(&bytes[136..140]);
            inode.i_atime_extra = read_u32_le(&bytes[140..144]);
            inode.i_crtime = read_u32_le(&bytes[144..148]);
            inode.i_crtime_extra = read_u32_le(&bytes[148..152]);
            inode.i_version_hi = read_u32_le(&bytes[152..156]);
            inode.i_projid = read_u32_le(&bytes[156..160]);
        }
        
        inode
    }
    
    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        write_u16_le(self.i_mode, &mut bytes[0..2]);
        write_u16_le(self.i_uid, &mut bytes[2..4]);
        write_u32_le(self.i_size_lo, &mut bytes[4..8]);
        write_u32_le(self.i_atime, &mut bytes[8..12]);
        write_u32_le(self.i_ctime, &mut bytes[12..16]);
        write_u32_le(self.i_mtime, &mut bytes[16..20]);
        write_u32_le(self.i_dtime, &mut bytes[20..24]);
        write_u16_le(self.i_gid, &mut bytes[24..26]);
        write_u16_le(self.i_links_count, &mut bytes[26..28]);
        write_u32_le(self.i_blocks_lo, &mut bytes[28..32]);
        write_u32_le(self.i_flags, &mut bytes[32..36]);
        write_u32_le(self.l_i_version, &mut bytes[36..40]);
        
        // 写入i_block数组
        for i in 0..15 {
            write_u32_le(self.i_block[i], &mut bytes[40 + i * 4..44 + i * 4]);
        }
        
        write_u32_le(self.i_generation, &mut bytes[100..104]);
        write_u32_le(self.i_file_acl_lo, &mut bytes[104..108]);
        write_u32_le(self.i_size_high, &mut bytes[108..112]);
        write_u32_le(self.i_obso_faddr, &mut bytes[112..116]);
        write_u16_le(self.l_i_blocks_high, &mut bytes[116..118]);
        write_u16_le(self.l_i_file_acl_high, &mut bytes[118..120]);
        write_u16_le(self.l_i_uid_high, &mut bytes[120..122]);
        write_u16_le(self.l_i_gid_high, &mut bytes[122..124]);
        write_u16_le(self.l_i_checksum_lo, &mut bytes[124..126]);
        write_u16_le(self.l_i_reserved, &mut bytes[126..128]);
        
        // 如果是大inode（256字节），写入额外字段
        if bytes.len() >= 256 {
            write_u16_le(self.i_extra_isize, &mut bytes[128..130]);
            write_u16_le(self.i_checksum_hi, &mut bytes[130..132]);
            write_u32_le(self.i_ctime_extra, &mut bytes[132..136]);
            write_u32_le(self.i_mtime_extra, &mut bytes[136..140]);
            write_u32_le(self.i_atime_extra, &mut bytes[140..144]);
            write_u32_le(self.i_crtime, &mut bytes[144..148]);
            write_u32_le(self.i_crtime_extra, &mut bytes[148..152]);
            write_u32_le(self.i_version_hi, &mut bytes[152..156]);
            write_u32_le(self.i_projid, &mut bytes[156..160]);
        }
    }
    
    fn disk_size() -> usize {
        256  // 大inode的默认大小
    }
}



