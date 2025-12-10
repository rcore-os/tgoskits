use core::cell::RefCell;
use crate::ext4_backend::jbd2::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::jbd2::jbdstruct::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::superblock::*;
use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::entries::*;
use crate::ext4_backend::mkfile::*;
use crate::ext4_backend::*;
use crate::ext4_backend::bitmap_cache::*;
use crate::ext4_backend::datablock_cache::*;
use crate::ext4_backend::inodetable_cache::*;
use crate::ext4_backend::mkd::*;
use crate::ext4_backend::tool::*;
use crate::ext4_backend::jbd2::jbd2::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::bitmap::*;

/// Ext4 块组描述符结构
/// 块组描述符包含了块组的元数据信息，如位图位置、inode表位置等
/// 每个块组都有一个对应的块组描述符
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Ext4GroupDesc {
    // 0x00 - 基本信息（32字节，兼容ext2/ext3）
    pub bg_block_bitmap_lo: u32,        // 块位图块号（低32位）
    pub bg_inode_bitmap_lo: u32,        // Inode位图块号（低32位）
    pub bg_inode_table_lo: u32,         // Inode表起始块号（低32位）

    pub bg_free_blocks_count_lo: u16,   // 空闲块数（低16位）
    pub bg_free_inodes_count_lo: u16,   // 空闲inode数（低16位）
    pub bg_used_dirs_count_lo: u16,     // 目录数（低16位）

    pub bg_flags: u16,                  // 标志
    pub bg_exclude_bitmap_lo: u32,      // 快照排除位图块号（低32位）
    pub bg_block_bitmap_csum_lo: u16,   // 块位图校验和（低16位）
    pub bg_inode_bitmap_csum_lo: u16,   // Inode位图校验和（低16位）

    pub bg_itable_unused_lo: u16,       // 未使用的inode数（低16位）
    
    pub bg_checksum: u16,               // 块组描述符校验和

    // 0x20 - 扩展信息（64位支持，额外32字节）
    pub bg_block_bitmap_hi: u32,        // 块位图块号（高32位）
    pub bg_inode_bitmap_hi: u32,        // Inode位图块号（高32位）
    pub bg_inode_table_hi: u32,         // Inode表起始块号（高32位）
    pub bg_free_blocks_count_hi: u16,   // 空闲块数（高16位）
    pub bg_free_inodes_count_hi: u16,   // 空闲inode数（高16位）
    pub bg_used_dirs_count_hi: u16,     // 目录数（高16位）
    pub bg_itable_unused_hi: u16,       // 未使用的inode数（高16位）
    pub bg_exclude_bitmap_hi: u32,      // 快照排除位图块号（高32位）
    pub bg_block_bitmap_csum_hi: u16,   // 块位图校验和（高16位）
    pub bg_inode_bitmap_csum_hi: u16,   // Inode位图校验和（高16位）
    pub bg_reserved: u32,               // 保留字段
}

impl Ext4GroupDesc {
    /// 标准块组描述符大小（32字节）
    pub const GOOD_OLD_DESC_SIZE: usize = 32;
    
    /// 64位块组描述符大小（64字节）
    pub const EXT4_DESC_SIZE_64BIT: usize = 64;

    /// 获取块位图块号（64位）
    pub fn block_bitmap(&self) -> u64 {
        (self.bg_block_bitmap_hi as u64) << 32 | self.bg_block_bitmap_lo as u64
    }

    /// 获取inode位图块号（64位）
    pub fn inode_bitmap(&self) -> u64 {
        (self.bg_inode_bitmap_hi as u64) << 32 | self.bg_inode_bitmap_lo as u64
    }

    /// 获取inode表起始块号（64位）
    pub fn inode_table(&self) -> u64 {
        (self.bg_inode_table_hi as u64) << 32 | self.bg_inode_table_lo as u64
    }

    /// 获取空闲块数（32位）
    pub fn free_blocks_count(&self) -> u32 {
        (self.bg_free_blocks_count_hi as u32) << 16 | self.bg_free_blocks_count_lo as u32
    }

    /// 获取空闲inode数（32位）
    pub fn free_inodes_count(&self) -> u32 {
        (self.bg_free_inodes_count_hi as u32) << 16 | self.bg_free_inodes_count_lo as u32
    }

    /// 获取目录数（32位）
    pub fn used_dirs_count(&self) -> u32 {
        (self.bg_used_dirs_count_hi as u32) << 16 | self.bg_used_dirs_count_lo as u32
    }

    /// 获取未使用的inode数（32位）
    pub fn itable_unused(&self) -> u32 {
        (self.bg_itable_unused_hi as u32) << 16 | self.bg_itable_unused_lo as u32
    }

    /// 获取快照排除位图块号（64位）
    pub fn exclude_bitmap(&self) -> u64 {
        (self.bg_exclude_bitmap_hi as u64) << 32 | self.bg_exclude_bitmap_lo as u64
    }

    /// 获取块位图校验和（32位）
    pub fn block_bitmap_csum(&self) -> u32 {
        (self.bg_block_bitmap_csum_hi as u32) << 16 | self.bg_block_bitmap_csum_lo as u32
    }

    /// 获取inode位图校验和（32位）
    pub fn inode_bitmap_csum(&self) -> u32 {
        (self.bg_inode_bitmap_csum_hi as u32) << 16 | self.bg_inode_bitmap_csum_lo as u32
    }

    /// 检查块组是否未初始化（inode表和位图未初始化）
    pub fn is_uninit_bg(&self) -> bool {
        self.bg_flags & Self::EXT4_BG_INODE_UNINIT != 0
    }

    /// 检查块位图是否未初始化
    pub fn is_block_bitmap_uninit(&self) -> bool {
        self.bg_flags & Self::EXT4_BG_BLOCK_UNINIT != 0
    }

    /// 检查inode位图是否未初始化
    pub fn is_inode_bitmap_uninit(&self) -> bool {
        self.bg_flags & Self::EXT4_BG_INODE_UNINIT != 0
    }

    /// 检查inode表是否被清零
    pub fn is_inode_table_zeroed(&self) -> bool {
        self.bg_flags & Self::EXT4_BG_INODE_ZEROED != 0
    }
}

// 块组描述符标志常量
impl Ext4GroupDesc {
    /// Inode表和位图未初始化
    pub const EXT4_BG_INODE_UNINIT: u16 = 0x0001;
    
    /// 块位图未初始化
    pub const EXT4_BG_BLOCK_UNINIT: u16 = 0x0002;
    
    /// Inode表已清零
    pub const EXT4_BG_INODE_ZEROED: u16 = 0x0004;
}

/// 块组描述符表
/// 包含所有块组描述符的集合
#[derive(Debug)]
pub struct BlockGroupDescTable<'a> {
    data: &'a [u8],                 // 原始数据
    desc_size: usize,               // 每个描述符的大小
    group_count: u32,               // 块组数量
}

impl<'a> BlockGroupDescTable<'a> {
    /// 创建块组描述符表实例
    pub fn new(data: &'a [u8], desc_size: usize, group_count: u32) -> Self {
        Self {
            data,
            desc_size,
            group_count,
        }
    }

    /// 获取指定块组的描述符
    pub fn get_desc(&self, group_idx: u32) -> Option<&Ext4GroupDesc> {
        if group_idx >= self.group_count {
            return None;
        }

        let offset = (group_idx as usize) * self.desc_size;
        if offset + core::mem::size_of::<Ext4GroupDesc>() > self.data.len() {
            return None;
        }

        // 安全地将字节切片转换为结构体引用
        let desc_ptr = self.data[offset..].as_ptr() as *const Ext4GroupDesc;
        unsafe { Some(&*desc_ptr) }
    }

    /// 获取块组数量
    pub fn group_count(&self) -> u32 {
        self.group_count
    }

    /// 获取描述符大小
    pub fn desc_size(&self) -> usize {
        self.desc_size
    }

    /// 迭代所有块组描述符
    pub fn iter(&'a self) -> BlockGroupDescIter<'a> {
        BlockGroupDescIter {
            table: self,
            current: 0,
        }
    }

    /// 统计所有块组的总空闲块数
    pub fn total_free_blocks(&self) -> u64 {
        let mut total = 0u64;
        for desc in self.iter() {
            total += desc.free_blocks_count() as u64;
        }
        total
    }

    /// 统计所有块组的总空闲inode数
    pub fn total_free_inodes(&self) -> u64 {
        let mut total = 0u64;
        for desc in self.iter() {
            total += desc.free_inodes_count() as u64;
        }
        total
    }

    /// 统计所有块组的总目录数
    pub fn total_used_dirs(&self) -> u64 {
        let mut total = 0u64;
        for desc in self.iter() {
            total += desc.used_dirs_count() as u64;
        }
        total
    }

    /// 查找有足够空闲块的块组
    pub fn find_group_with_free_blocks(&self, needed: u32) -> Option<u32> {
        for (idx, desc) in self.iter().enumerate() {
            if desc.free_blocks_count() >= needed && !desc.is_block_bitmap_uninit() {
                return Some(idx as u32);
            }
        }
        None
    }

    /// 查找有空闲inode的块组
    pub fn find_group_with_free_inodes(&self) -> Option<u32> {
        for (idx, desc) in self.iter().enumerate() {
            if desc.free_inodes_count() > 0 && !desc.is_inode_bitmap_uninit() {
                return Some(idx as u32);
            }
        }
        None
    }
}

/// 块组描述符迭代器
pub struct BlockGroupDescIter<'a> {
    table: &'a BlockGroupDescTable<'a>,
    current: u32,
}

impl<'a> Iterator for BlockGroupDescIter<'a> {
    type Item = &'a Ext4GroupDesc;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current >= self.table.group_count {
            return None;
        }

        let desc = self.table.get_desc(self.current)?;
        self.current += 1;
        Some(desc)
    }
}

/// 可变块组描述符表
/// 用于修改块组描述符
pub struct BlockGroupDescTableMut<'a> {
    data: &'a mut [u8],             // 可变原始数据
    desc_size: usize,               // 每个描述符的大小
    group_count: u32,               // 块组数量
}

impl<'a> BlockGroupDescTableMut<'a> {
    /// 创建可变块组描述符表实例
    pub fn new(data: &'a mut [u8], desc_size: usize, group_count: u32) -> Self {
        Self {
            data,
            desc_size,
            group_count,
        }
    }

    /// 获取指定块组的可变描述符引用
    pub fn get_desc_mut(&mut self, group_idx: u32) -> Option<&mut Ext4GroupDesc> {
        if group_idx >= self.group_count {
            return None;
        }

        let offset = (group_idx as usize) * self.desc_size;
        if offset + core::mem::size_of::<Ext4GroupDesc>() > self.data.len() {
            return None;
        }

        // 安全地将字节切片转换为可变结构体引用
        let desc_ptr = self.data[offset..].as_mut_ptr() as *mut Ext4GroupDesc;
        unsafe { Some(&mut *desc_ptr) }
    }

    /// 更新块组的空闲块数
    pub fn update_free_blocks(&mut self, group_idx: u32, count: u32) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            desc.bg_free_blocks_count_lo = (count & 0xFFFF) as u16;
            desc.bg_free_blocks_count_hi = ((count >> 16) & 0xFFFF) as u16;
            true
        } else {
            false
        }
    }

    /// 更新块组的空闲inode数
    pub fn update_free_inodes(&mut self, group_idx: u32, count: u32) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            desc.bg_free_inodes_count_lo = (count & 0xFFFF) as u16;
            desc.bg_free_inodes_count_hi = ((count >> 16) & 0xFFFF) as u16;
            true
        } else {
            false
        }
    }

    /// 更新块组的目录数
    pub fn update_used_dirs(&mut self, group_idx: u32, count: u32) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            desc.bg_used_dirs_count_lo = (count & 0xFFFF) as u16;
            desc.bg_used_dirs_count_hi = ((count >> 16) & 0xFFFF) as u16;
            true
        } else {
            false
        }
    }

    /// 递增块组的目录数
    pub fn increment_used_dirs(&mut self, group_idx: u32) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            let count = desc.used_dirs_count() + 1;
            desc.bg_used_dirs_count_lo = (count & 0xFFFF) as u16;
            desc.bg_used_dirs_count_hi = ((count >> 16) & 0xFFFF) as u16;
            true
        } else {
            false
        }
    }

    /// 递减块组的目录数
    pub fn decrement_used_dirs(&mut self, group_idx: u32) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            let count = desc.used_dirs_count().saturating_sub(1);
            desc.bg_used_dirs_count_lo = (count & 0xFFFF) as u16;
            desc.bg_used_dirs_count_hi = ((count >> 16) & 0xFFFF) as u16;
            true
        } else {
            false
        }
    }

    /// 设置块组标志
    pub fn set_flags(&mut self, group_idx: u32, flags: u16) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            desc.bg_flags |= flags;
            true
        } else {
            false
        }
    }

    /// 清除块组标志
    pub fn clear_flags(&mut self, group_idx: u32, flags: u16) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            desc.bg_flags &= !flags;
            true
        } else {
            false
        }
    }
}

/// 块组信息统计
#[derive(Debug, Clone, Copy)]
pub struct BlockGroupStats {
    pub group_idx: u32,             // 块组索引
    pub free_blocks: u32,           // 空闲块数
    pub free_inodes: u32,           // 空闲inode数
    pub used_dirs: u32,             // 目录数
    pub itable_unused: u32,         // 未使用的inode数
    pub flags: u16,                 // 标志
}

impl BlockGroupStats {
    /// 从块组描述符提取统计信息
    pub fn from_desc(group_idx: u32, desc: &Ext4GroupDesc) -> Self {
        Self {
            group_idx,
            free_blocks: desc.free_blocks_count(),
            free_inodes: desc.free_inodes_count(),
            used_dirs: desc.used_dirs_count(),
            itable_unused: desc.itable_unused(),
            flags: desc.bg_flags,
        }
    }

    /// 计算已使用的inode数
    pub fn used_inodes(&self, inodes_per_group: u32) -> u32 {
        inodes_per_group.saturating_sub(self.free_inodes)
    }

    /// 计算已使用的块数
    pub fn used_blocks(&self, blocks_per_group: u32) -> u32 {
        blocks_per_group.saturating_sub(self.free_blocks)
    }

    /// 计算块利用率（百分比）
    pub fn block_usage_percent(&self, blocks_per_group: u32) -> f32 {
        if blocks_per_group == 0 {
            return 0.0;
        }
        let used = self.used_blocks(blocks_per_group);
        (used as f32 / blocks_per_group as f32) * 100.0
    }

    /// 计算inode利用率（百分比）
    pub fn inode_usage_percent(&self, inodes_per_group: u32) -> f32 {
        if inodes_per_group == 0 {
            return 0.0;
        }
        let used = self.used_inodes(inodes_per_group);
        (used as f32 / inodes_per_group as f32) * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_group_desc_64bit_values() {
        let mut desc = Ext4GroupDesc {
            bg_block_bitmap_lo: 0x12345678,
            bg_block_bitmap_hi: 0xABCDEF00,
            bg_inode_bitmap_lo: 0,
            bg_inode_bitmap_hi: 0,
            bg_inode_table_lo: 0,
            bg_inode_table_hi: 0,
            bg_free_blocks_count_lo: 100,
            bg_free_blocks_count_hi: 0,
            bg_free_inodes_count_lo: 200,
            bg_free_inodes_count_hi: 0,
            bg_used_dirs_count_lo: 10,
            bg_used_dirs_count_hi: 0,
            bg_flags: 0,
            bg_exclude_bitmap_lo: 0,
            bg_block_bitmap_csum_lo: 0,
            bg_inode_bitmap_csum_lo: 0,
            bg_itable_unused_lo: 0,
            bg_checksum: 0,
            bg_exclude_bitmap_hi: 0,
            bg_block_bitmap_csum_hi: 0,
            bg_inode_bitmap_csum_hi: 0,
            bg_itable_unused_hi: 0,
            bg_reserved: 0,
        };

        assert_eq!(desc.block_bitmap(), 0xABCDEF0012345678);
        assert_eq!(desc.free_blocks_count(), 100);
        assert_eq!(desc.free_inodes_count(), 200);
        assert_eq!(desc.used_dirs_count(), 10);
    }

    #[test]
    fn test_group_desc_flags() {
        let mut desc = Ext4GroupDesc {
            bg_flags: Ext4GroupDesc::EXT4_BG_INODE_UNINIT,
            ..Default::default()
        };

        assert!(desc.is_inode_bitmap_uninit());
        assert!(!desc.is_block_bitmap_uninit());
    }
}

/// 实现 DiskFormat trait 用于字节序转换
impl DiskFormat for Ext4GroupDesc {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        Self {
            bg_block_bitmap_lo: read_u32_le(&bytes[0..4]),
            bg_inode_bitmap_lo: read_u32_le(&bytes[4..8]),
            bg_inode_table_lo: read_u32_le(&bytes[8..12]),
            bg_free_blocks_count_lo: read_u16_le(&bytes[12..14]),
            bg_free_inodes_count_lo: read_u16_le(&bytes[14..16]),
            bg_used_dirs_count_lo: read_u16_le(&bytes[16..18]),
            bg_flags: read_u16_le(&bytes[18..20]),
            bg_exclude_bitmap_lo: read_u32_le(&bytes[20..24]),
            bg_block_bitmap_csum_lo: read_u16_le(&bytes[24..26]),
            bg_inode_bitmap_csum_lo: read_u16_le(&bytes[26..28]),
            bg_itable_unused_lo: read_u16_le(&bytes[28..30]),
            bg_checksum: read_u16_le(&bytes[30..32]),
            bg_block_bitmap_hi: read_u32_le(&bytes[32..36]),
            bg_inode_bitmap_hi: read_u32_le(&bytes[36..40]),
            bg_inode_table_hi: read_u32_le(&bytes[40..44]),
            bg_free_blocks_count_hi: read_u16_le(&bytes[44..46]),
            bg_free_inodes_count_hi: read_u16_le(&bytes[46..48]),
            bg_used_dirs_count_hi: read_u16_le(&bytes[48..50]),
            bg_itable_unused_hi: read_u16_le(&bytes[50..52]),
            bg_exclude_bitmap_hi: read_u32_le(&bytes[52..56]),
            bg_block_bitmap_csum_hi: read_u16_le(&bytes[56..58]),
            bg_inode_bitmap_csum_hi: read_u16_le(&bytes[58..60]),
            bg_reserved: read_u32_le(&bytes[60..64]),
        }
    }
    
    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        write_u32_le(self.bg_block_bitmap_lo, &mut bytes[0..4]);
        write_u32_le(self.bg_inode_bitmap_lo, &mut bytes[4..8]);
        write_u32_le(self.bg_inode_table_lo, &mut bytes[8..12]);
        write_u16_le(self.bg_free_blocks_count_lo, &mut bytes[12..14]);
        write_u16_le(self.bg_free_inodes_count_lo, &mut bytes[14..16]);
        write_u16_le(self.bg_used_dirs_count_lo, &mut bytes[16..18]);
        write_u16_le(self.bg_flags, &mut bytes[18..20]);
        write_u32_le(self.bg_exclude_bitmap_lo, &mut bytes[20..24]);
        write_u16_le(self.bg_block_bitmap_csum_lo, &mut bytes[24..26]);
        write_u16_le(self.bg_inode_bitmap_csum_lo, &mut bytes[26..28]);
        write_u16_le(self.bg_itable_unused_lo, &mut bytes[28..30]);
        write_u16_le(self.bg_checksum, &mut bytes[30..32]);
        write_u32_le(self.bg_block_bitmap_hi, &mut bytes[32..36]);
        write_u32_le(self.bg_inode_bitmap_hi, &mut bytes[36..40]);
        write_u32_le(self.bg_inode_table_hi, &mut bytes[40..44]);
        write_u16_le(self.bg_free_blocks_count_hi, &mut bytes[44..46]);
        write_u16_le(self.bg_free_inodes_count_hi, &mut bytes[46..48]);
        write_u16_le(self.bg_used_dirs_count_hi, &mut bytes[48..50]);
        write_u16_le(self.bg_itable_unused_hi, &mut bytes[50..52]);
        write_u32_le(self.bg_exclude_bitmap_hi, &mut bytes[52..56]);
        write_u16_le(self.bg_block_bitmap_csum_hi, &mut bytes[56..58]);
        write_u16_le(self.bg_inode_bitmap_csum_hi, &mut bytes[58..60]);
        write_u32_le(self.bg_reserved, &mut bytes[60..64]);
    }
    
    fn disk_size() -> usize {
        64
    }
}
