//! 位图分配器模块

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
use crate::ext4_backend::blockgroup_description::*;
use crate::ext4_backend::mkd::*;
use crate::ext4_backend::tool::*;
use crate::ext4_backend::jbd2::jbd2::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::bitmap::*;
/// 块分配器错误类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    /// 没有空闲空间
    NoSpace,
    /// 位图错误
    BitmapError(BitmapError),
    /// 块组索引无效
    InvalidGroupIndex,
    /// 参数无效
    InvalidParameter,
}

impl From<BitmapError> for AllocError {
    fn from(err: BitmapError) -> Self {
        AllocError::BitmapError(err)
    }
}

impl core::fmt::Display for AllocError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AllocError::NoSpace => write!(f, "not have enough space"),
            AllocError::BitmapError(e) => write!(f, "bitmap error: {}", e),
            AllocError::InvalidGroupIndex => write!(f, "valid group index error"),
            AllocError::InvalidParameter => write!(f, "Valid parse error"),
        }
    }
}

/// 块分配结果
/// 包含分配的块号和所在的块组
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockAlloc {
    /// 块组索引
    pub group_idx: u32,
    /// 块组内的块索引
    pub block_in_group: u32,
    /// 全局块号
    pub global_block: u64,
}




///Bitmap Buffer
pub struct BitmapBuffer<'a>{
    data:BTreeMap<u32,(&'a mut [u8],&'a mut [u8])>,//group_id bitmap_data
}

/// Inode分配结果
/// 包含分配的inode号和所在的块组
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InodeAlloc {
    /// 块组索引
    pub group_idx: u32,
    /// 块组内的inode索引（从0开始）
    pub inode_in_group: u32,
    /// 全局inode号（从1开始）
    pub global_inode: u32,
}

/// 块分配器
/// 负责管理块的分配和释放
pub struct BlockAllocator {
    blocks_per_group: u32,
    first_data_block: u32,
}

impl BlockAllocator {
    /// 创建块分配器
    pub fn new(sb: &Ext4Superblock) -> Self {
        Self {
            blocks_per_group: sb.s_blocks_per_group,
            first_data_block: sb.s_first_data_block,
        }
    }

    /// 在指定块组中分配一个块
    /// * `bitmap_data` - 块位图数据（可变引用）
    /// * `group_idx` - 块组索引
    /// * `group_desc` - 块组描述符（用于读取元数据）
    pub fn alloc_block_in_group(
        &self,
        bitmap_data: &mut [u8],
        group_idx: u32,
        group_desc: &Ext4GroupDesc,
    ) -> Result<BlockAlloc, AllocError> {
        // 检查是否有空闲块
        if group_desc.free_blocks_count() == 0 {
            return Err(AllocError::NoSpace);
        }

        let mut bitmap = BlockBitmapMut::new(bitmap_data, self.blocks_per_group);

        // 查找第一个空闲块
        let block_in_group = self.find_free_block(&bitmap)?
            .ok_or(AllocError::NoSpace)?;

        // 分配块
        bitmap.allocate(block_in_group)?;

        // 计算全局块号
        let global_block = self.block_to_global(group_idx, block_in_group);

        Ok(BlockAlloc {
            group_idx,
            block_in_group,
            global_block,
        })
    }

    /// 在指定块组中分配连续的多个块
    /// * `bitmap_data` - 块位图数据
    /// * `group_idx` - 块组索引
    /// * `count` - 需要的连续块数
    pub fn alloc_contiguous_blocks(
        &self,
        bitmap_data: &mut [u8],
        group_idx: u32,
        count: u32,
    ) -> Result<BlockAlloc, AllocError> {
        if count == 0 {
            return Err(AllocError::InvalidParameter);
        }

        let mut bitmap = BlockBitmapMut::new(bitmap_data, self.blocks_per_group);

        // 查找连续的空闲块
        let block_in_group = self.find_contiguous_free_blocks(&bitmap, count)?
            .ok_or(AllocError::NoSpace)?;

        // 批量分配
        bitmap.allocate_range(block_in_group, count)?;

        let global_block = self.block_to_global(group_idx, block_in_group);

        Ok(BlockAlloc {
            group_idx,
            block_in_group,
            global_block,
        })
    }

    /// 释放一个块
    /// * `bitmap_data` - 块位图数据
    /// * `group_idx` - 块组索引
    /// * `block_in_group` - 块组内的块索引
    pub fn free_block(
        &self,
        bitmap_data: &mut [u8],
        block_in_group: u32,
    ) -> Result<(), AllocError> {
        let mut bitmap = BlockBitmapMut::new(bitmap_data, self.blocks_per_group);
        bitmap.free(block_in_group)?;
        Ok(())
    }

    /// 释放连续的多个块
    pub fn free_blocks(
        &self,
        bitmap_data: &mut [u8],
        start_block: u32,
        count: u32,
    ) -> Result<(), AllocError> {
        let mut bitmap = BlockBitmapMut::new(bitmap_data, self.blocks_per_group);
        bitmap.free_range(start_block, count)?;
        Ok(())
    }

    /// 查找第一个空闲块
    fn find_free_block(&self, bitmap: &BlockBitmapMut) -> Result<Option<u32>, AllocError> {
        for block_idx in 0..self.blocks_per_group {
            if bitmap.is_allocated(block_idx) == Some(false) {
                return Ok(Some(block_idx));
            }
        }
        Ok(None)
    }

    /// 查找连续的空闲块
    fn find_contiguous_free_blocks(
        &self,
        bitmap: &BlockBitmapMut,
        count: u32,
    ) -> Result<Option<u32>, AllocError> {
        let mut consecutive = 0u32;
        let mut start_idx = 0u32;

        for block_idx in 0..self.blocks_per_group {
            if bitmap.is_allocated(block_idx) == Some(false) {
                if consecutive == 0 {
                    start_idx = block_idx;
                }
                consecutive += 1;
                if consecutive == count {
                    return Ok(Some(start_idx));
                }
            } else {
                consecutive = 0;
            }
        }

        Ok(None)
    }

    /// 将块组内块号转换为全局块号
    fn block_to_global(&self, group_idx: u32, block_in_group: u32) -> u64 {
        (group_idx as u64 * self.blocks_per_group as u64) + 
        block_in_group as u64 + 
        self.first_data_block as u64
    }
}

/// Inode分配器
/// 负责管理inode的分配和释放
pub struct InodeAllocator {
    inodes_per_group: u32,
    first_inode: u32,
}

impl InodeAllocator {
    /// 创建inode分配器
    pub fn new(sb: &Ext4Superblock) -> Self {
        Self {
            inodes_per_group: sb.s_inodes_per_group,
            first_inode: sb.s_first_ino,
        }
    }

    /// 在指定块组中分配一个inode
    /// * `bitmap_data` - inode位图数据（可变引用）
    /// * `group_idx` - 块组索引
    /// * `group_desc` - 块组描述符
    pub fn alloc_inode_in_group(
        &self,
        bitmap_data: &mut [u8],
        group_idx: u32,
        group_desc: &Ext4GroupDesc,
    ) -> Result<InodeAlloc, AllocError> {
        // 检查是否有空闲inode
        if group_desc.free_inodes_count() == 0 {
            return Err(AllocError::NoSpace);
        }

        let mut bitmap = InodeBitmapMut::new(bitmap_data, self.inodes_per_group);

        // 查找第一个空闲inode
        let inode_in_group = self.find_free_inode(&bitmap)?
            .ok_or(AllocError::NoSpace)?;

        // 分配inode
        bitmap.allocate(inode_in_group)?;

        // 计算全局inode号（从1开始）
        let global_inode = self.inode_to_global(group_idx, inode_in_group);

        Ok(InodeAlloc {
            group_idx,
            inode_in_group,
            global_inode,
        })
    }

    /// 释放一个inode
    /// * `bitmap_data` - inode位图数据
    /// * `inode_in_group` - 块组内的inode索引
    pub fn free_inode(
        &self,
        bitmap_data: &mut [u8],
        inode_in_group: u32,
    ) -> Result<(), AllocError> {
        let mut bitmap = InodeBitmapMut::new(bitmap_data, self.inodes_per_group);
        bitmap.free(inode_in_group)?;
        Ok(())
    }

    /// 查找第一个空闲inode
    fn find_free_inode(&self, bitmap: &InodeBitmapMut) -> Result<Option<u32>, AllocError> {
        let start_idx = if self.first_inode > 0 {
            self.first_inode - 1  // 比如 first_ino=11 → 从 index 10 开始
        } else {
            0
        };

        for inode_idx in start_idx..self.inodes_per_group {
            if bitmap.is_allocated(inode_idx) == Some(false) {
                return Ok(Some(inode_idx));
            }
        }
        Ok(None)
    }

    /// 将块组内inode索引转换为全局inode号
    /// 注意：Ext4 的 inode 号从 1 开始
    fn inode_to_global(&self, group_idx: u32, inode_in_group: u32) -> u32 {
        group_idx * self.inodes_per_group + inode_in_group + 1
    }

    /// 将全局inode号转换为块组索引和块组内索引
    pub fn global_to_group(&self, global_inode: u32) -> (u32, u32) {
        let inode_idx = global_inode - 1; // 转换为从0开始的索引
        let group_idx = inode_idx / self.inodes_per_group;
        let inode_in_group = inode_idx % self.inodes_per_group;
        (group_idx, inode_in_group)
    }
}

use alloc::collections::btree_map::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::lazy_static;
//crete global inode_alloctor and block alloctor;
lazy_static!{
    
}





#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn test_block_allocator_single() {
        let mut sb = Ext4Superblock::default();
        sb.s_blocks_per_group = 1024;
        sb.s_first_data_block = 0;
        
        let allocator = BlockAllocator::new(&sb);
        
        let mut bitmap_data = vec![0u8; 128]; // 1024 bits
        let mut gd = Ext4GroupDesc::default();
        gd.bg_free_blocks_count_lo = 1024;
        
        let result = allocator.alloc_block_in_group(&mut bitmap_data, 0, &gd);
        assert!(result.is_ok());
        
        let alloc = result.unwrap();
        assert_eq!(alloc.group_idx, 0);
        assert_eq!(alloc.block_in_group, 0);
        assert_eq!(alloc.global_block, 0);
    }

    #[test]
    fn test_block_allocator_contiguous() {
        let mut sb = Ext4Superblock::default();
        sb.s_blocks_per_group = 1024;
        sb.s_first_data_block = 0;
        
        let allocator = BlockAllocator::new(&sb);
        
        let mut bitmap_data = vec![0u8; 128];
        
        let result = allocator.alloc_contiguous_blocks(&mut bitmap_data, 0, 5);
        assert!(result.is_ok());
        
        let alloc = result.unwrap();
        assert_eq!(alloc.block_in_group, 0);
    }

    #[test]
    fn test_inode_allocator() {
        let mut sb = Ext4Superblock::default();
        sb.s_inodes_per_group = 256;
        sb.s_first_ino = 11;
        
        let allocator = InodeAllocator::new(&sb);
        
        let mut bitmap_data = vec![0u8; 32]; // 256 bits
        let mut gd = Ext4GroupDesc::default();
        gd.bg_free_inodes_count_lo = 256;
        
        let result = allocator.alloc_inode_in_group(&mut bitmap_data, 0, &gd);
        assert!(result.is_ok());
        
        let alloc = result.unwrap();
        assert_eq!(alloc.group_idx, 0);
        assert!(alloc.inode_in_group >= 10); // 跳过保留inode
    }

    #[test]
    fn test_inode_global_conversion() {
        let mut sb = Ext4Superblock::default();
        sb.s_inodes_per_group = 256;
        sb.s_first_ino = 11;
        
        let allocator = InodeAllocator::new(&sb);
        
        // 测试转换
        let (group, inode_in_group) = allocator.global_to_group(257);
        assert_eq!(group, 1);
        assert_eq!(inode_in_group, 0);
        
        let global = allocator.inode_to_global(group, inode_in_group);
        assert_eq!(global, 257);
    }
}