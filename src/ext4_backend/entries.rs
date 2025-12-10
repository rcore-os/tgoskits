use crate::ext4_backend::jbd2::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::jbd2::jbdstruct::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::superblock::*;
use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::loopfile::*;
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
use alloc::vec::Vec;
/// Ext4 目录条目结构（传统格式）
/// 用于ext3/ext4的线性目录条目格式
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DirEntry {
    pub inode: u32,             // Inode号
    pub rec_len: u16,           // 目录条目长度
    pub name_len: u8,           // 文件名长度
    pub file_type: u8,          // 文件类型
    // 文件名紧跟其后（变长，最长255字节）
    pub name: [u8; DIRNAME_LEN]
}

/// Ext4 目录条目结构2（扩展格式）
/// 与Ext4DirEntry布局相同，但使用校验和
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DirEntry2 {
    pub inode: u32,             // Inode号
    pub rec_len: u16,           // 目录条目长度 占用的字节数
    pub name_len: u8,           // 文件名长度
    pub file_type: u8,          // 文件类型（EXT4_FT_*）
    // 文件名紧跟其后（变长，最长255字节） 写入时进行截断写入
    pub name: [u8; DIRNAME_LEN]
}

impl Ext4DirEntry2 {
    ///构造函数
    pub fn new(inode_num: u32, rec_len: u16, file_type: u8, name: &[u8]) -> Self {
        let mut name_buf = [0u8; DIRNAME_LEN];
        let len = core::cmp::min(name.len(), DIRNAME_LEN);
        name_buf[..len].copy_from_slice(&name[..len]);
        Ext4DirEntry2 {
            inode: inode_num,
            rec_len,
            name_len: len as u8,
            file_type,
            name: name_buf,
        }
    }

    /// 目录条目最小长度
    pub const MIN_DIR_ENTRY_LEN: u16 = 12;
    
    /// 文件名最大长度
    pub const MAX_NAME_LEN: u8 = 255;

    /// 计算目录条目实际占用长度（含对齐）
    pub fn entry_len(name_len: u8) -> u16 {
        let base_len = 8; 
        let total = base_len + name_len as u16;
        // 对齐到4字节边界
        ((total + 3) / 4) * 4
    }
}

// 文件类型常量
impl Ext4DirEntry2 {
    pub const EXT4_FT_UNKNOWN: u8 = 0;   // 未知类型
    pub const EXT4_FT_REG_FILE: u8 = 1;  // 普通文件
    pub const EXT4_FT_DIR: u8 = 2;       // 目录
    pub const EXT4_FT_CHRDEV: u8 = 3;    // 字符设备
    pub const EXT4_FT_BLKDEV: u8 = 4;    // 块设备
    pub const EXT4_FT_FIFO: u8 = 5;      // FIFO
    pub const EXT4_FT_SOCK: u8 = 6;      // 套接字
    pub const EXT4_FT_SYMLINK: u8 = 7;   // 符号链接
    pub const EXT4_FT_MAX: u8 = 8;       // 最大值
}

/// 目录条目尾部（用于校验和）
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DirEntryTail {
    pub det_reserved_zero1: u32,    // 保留，必须为0
    pub det_rec_len: u16,           // 12
    pub det_reserved_zero2: u8,     // 保留，必须为0
    pub det_reserved_ft: u8,        // 0xDE，用于标识这是尾部
    pub det_checksum: u32,          // 目录块的CRC32C校验和
}

impl Ext4DirEntryTail {
    pub const RESERVED_FT: u8 = 0xDE;
    pub const TAIL_LEN: u16 = 12;
}

/// HTree根节点信息结构
/// 用于哈希树索引的目录
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DxRoot {
    pub dot: Ext4DirEntry2,         // "." 条目
    pub dotdot: Ext4DirEntry2,      // ".." 条目
    pub info: Ext4DxRootInfo,       // 根信息
    // 后面跟着Ext4DxEntry数组
}

/// HTree根节点信息
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DxRootInfo {
    pub reserved_zero: u32,         // 保留，必须为0
    pub hash_version: u8,           // 哈希版本
    pub info_length: u8,            // 信息长度（8字节）
    pub indirect_levels: u8,        // 间接层数
    pub unused_flags: u8,           // 未使用的标志
}

impl Ext4DxRootInfo {
    pub const INFO_LENGTH: u8 = 8;
}

// 哈希版本常量
impl Ext4DxRootInfo {
    pub const DX_HASH_LEGACY: u8 = 0;           // 传统哈希
    pub const DX_HASH_HALF_MD4: u8 = 1;         // Half MD4
    pub const DX_HASH_TEA: u8 = 2;              // TEA
    pub const DX_HASH_LEGACY_UNSIGNED: u8 = 3;  // 传统无符号
    pub const DX_HASH_HALF_MD4_UNSIGNED: u8 = 4;// Half MD4无符号
    pub const DX_HASH_TEA_UNSIGNED: u8 = 5;     // TEA无符号
}

/// HTree条目结构
/// 用于哈希索引
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DxEntry {
    pub hash: u32,              // 哈希值
    pub block: u32,             // 块号
}

/// HTree计数信息
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DxCountlimit {
    pub limit: u16,             // 最大条目数
    pub count: u16,             // 当前条目数
}

/// 完整的HTree节点
#[repr(C)]
#[derive(Debug)]
pub struct Ext4DxNode {
    pub fake: Ext4DirEntry2,        // 伪造的目录条目
    pub countlimit: Ext4DxCountlimit, // 计数和限制
    // 后面跟着Ext4DxEntry数组
}

/// Extent状态树的叶子节点
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4ExtentStatus {
    pub es_lblk: u64,           // 第一个逻辑块
    pub es_len: u64,            // extent长度
    pub es_pblk: u64,           // 第一个物理块
}

/// 用于在目录中查找文件名的辅助结构
#[derive(Debug)]
pub struct Ext4DirEntryInfo<'a> {
    pub inode: u32,             // Inode号
    pub file_type: u8,          // 文件类型
    pub name: &'a [u8],         // 文件名切片
}

impl<'a> Ext4DirEntryInfo<'a> {
    /// 从原始字节数据解析目录条目
    pub fn parse_from_bytes(data: &'a [u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }

        let inode = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if inode == 0 {
            return None; // 无效条目
        }

        let rec_len = u16::from_le_bytes([data[4], data[5]]);
        let name_len = data[6] as usize;
        let file_type = data[7];

        if rec_len < 8 || name_len > 255 || data.len() < 8 + name_len {
            return None;
        }

        let name = &data[8..8 + name_len];

        Some(Ext4DirEntryInfo {
            inode,
            file_type,
            name,
        })
    }

    /// 获取文件名字符串（UTF-8）
    pub fn name_str(&self) -> Option<&str> {
        core::str::from_utf8(self.name).ok()
    }

    /// 检查是否是 "." 条目
    pub fn is_dot(&self) -> bool {
        self.name == b"."
    }

    /// 检查是否是 ".." 条目
    pub fn is_dotdot(&self) -> bool {
        self.name == b".."
    }
}

/// 目录块迭代器
/// 用于遍历目录块中的所有条目
pub struct DirEntryIterator<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> DirEntryIterator<'a> {
    /// 创建新的目录条目迭代器
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
}

impl<'a> Iterator for DirEntryIterator<'a> {
    type Item = (Ext4DirEntryInfo<'a>, u16); // (条目信息, rec_len)

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.data.len() {
            return None;
        }

        let remaining = &self.data[self.offset..];
        if remaining.len() < 8 {
            return None;
        }

        let rec_len = u16::from_le_bytes([remaining[4], remaining[5]]);
        if rec_len < 8 || rec_len as usize > remaining.len() {
            return None;
        }

        let entry_data = &remaining[..rec_len as usize];
        let entry_info = Ext4DirEntryInfo::parse_from_bytes(entry_data)?;

        self.offset += rec_len as usize;

        Some((entry_info, rec_len))
    }
}

/// 线性目录（Classic Directory）辅助函数
pub mod classic_dir {
    use super::*;

    /// 在线性目录块中查找文件名
    pub fn find_entry<'a>(
        block_data: &'a [u8],
        target_name: &[u8],
    ) -> Option<Ext4DirEntryInfo<'a>> {
        let iter = DirEntryIterator::new(block_data);
        for (entry, _) in iter {
            if entry.name == target_name {
                return Some(entry);
            }
        }
        None
    }

    /// 列出目录中的所有条目
    pub fn list_entries(block_data: &[u8]) -> Vec<Ext4DirEntryInfo> {
        let iter = DirEntryIterator::new(block_data);
        iter.map(|(entry, _)| entry).collect()
    }
}

/// HTree索引目录（Hash Tree Directory）辅助函数
pub mod htree_dir {
    use super::*;

    /// 计算文件名的哈希值
    pub fn calculate_hash(name: &[u8], hash_version: u8, hash_seed: &[u32; 4]) -> u32 {
        match hash_version {
            Ext4DxRootInfo::DX_HASH_LEGACY => legacy_hash(name),
            Ext4DxRootInfo::DX_HASH_HALF_MD4 => half_md4_hash(name, hash_seed),
            Ext4DxRootInfo::DX_HASH_TEA => tea_hash(name, hash_seed),
            _ => 0,
        }
    }

    /// 传统哈希算法（简化实现）
    fn legacy_hash(name: &[u8]) -> u32 {
        let mut hash = 0u32;
        for &byte in name {
            hash = hash.wrapping_mul(33).wrapping_add(byte as u32);
        }
        hash
    }

    /// Half MD4哈希算法（简化实现）
    fn half_md4_hash(name: &[u8], seed: &[u32; 4]) -> u32 {
        // 这是一个简化版本，实际实现需要完整的MD4算法
        let mut hash = seed[0];
        for &byte in name {
            hash = hash.wrapping_mul(1103515245).wrapping_add(byte as u32);
        }
        hash
    }

    /// TEA哈希算法（Tiny Encryption Algorithm）
    fn tea_hash(name: &[u8], seed: &[u32; 4]) -> u32 {
        let mut hash = seed[0];
        let mut buf = [0u32; 4];
        
        for chunk in name.chunks(16) {
            for (i, bytes) in chunk.chunks(4).enumerate() {
                if i >= 4 { break; }
                let mut val = 0u32;
                for &b in bytes {
                    val = (val << 8) | b as u32;
                }
                buf[i] = val;
            }
            
            // TEA算法的简化版本
            for _ in 0..4 {
                hash = hash.wrapping_add(buf[0] ^ buf[1]);
            }
        }
        hash
    }
}

/// 实现 DiskFormat trait 用于字节序转换
/// 注意：只处理固定大小的头部（8字节），不包括变长文件名
impl DiskFormat for Ext4DirEntry2 {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        Self {
            inode: read_u32_le(&bytes[0..4]),
            rec_len: read_u16_le(&bytes[4..6]),
            name_len: bytes[6],
            file_type: bytes[7],
            name:[0;DIRNAME_LEN]
        }
    }
    
    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        write_u32_le(self.inode, &mut bytes[0..4]);
        write_u16_le(self.rec_len, &mut bytes[4..6]);
        bytes[6] = self.name_len;
        bytes[7] = self.file_type;
    }
    
    fn disk_size() -> usize {
        8  // 固定头部大小，不包括变长文件名
    }
}
