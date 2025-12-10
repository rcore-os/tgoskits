//! Inode表缓存模块
//! 
//! 提供inode结构的缓存管理，支持延迟写回和LRU淘汰

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use crate::ext4_backend::jbd2::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::jbd2::jbdstruct::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::superblock::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::extents_tree::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::entries::*;
use crate::ext4_backend::mkfile::*;


/// Inode缓存键（全局inode号）
pub type InodeCacheKey = u64;

/// 缓存的inode数据
#[derive(Debug, Clone)]
pub struct CachedInode {
    /// Inode结构体
    pub inode: Ext4Inode,
    /// 是否被修改（脏）
    pub dirty: bool,
    /// Inode在磁盘上的位置（块号）
    pub block_num: u64,
    /// 在块内的偏移
    pub offset_in_block: usize,
    /// Inode号
    pub inode_num: u64,
    /// 最后访问时间戳
    pub last_access: u64,
}

impl CachedInode {
    pub fn new(inode: Ext4Inode, inode_num: u64, block_num: u64, offset: usize) -> Self {
        Self {
            inode,
            dirty: false,
            block_num,
            offset_in_block: offset,
            inode_num,
            last_access: 0,
        }
    }
    
    /// 标记为脏
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// 生成一个轻量级句柄，供外部在 modify 中使用
    pub fn handle(&self) -> InodeHandle {
        InodeHandle {
            inode_num: self.inode_num,
        }
    }
}

/// Inode 句柄
#[derive(Debug, Clone, Copy)]
pub struct InodeHandle {
    pub inode_num: u64,
}

/// Inode缓存管理器
pub struct InodeCache {
    /// 缓存的inode
    cache: BTreeMap<InodeCacheKey, CachedInode>,
    /// 最大缓存条目数
    max_entries: usize,
    /// 访问计数器
    access_counter: u64,
    /// 每个inode的大小=
    inode_size: usize,
}

impl InodeCache {
    /// 创建inode缓存
    /// * `max_entries` - 最大缓存条目数，建议16-64个
    /// * `inode_size` - inode大小（通常是256字节）
    pub fn new(max_entries: usize, inode_size: usize) -> Self {
        Self {
            cache: BTreeMap::new(),
            max_entries,
            access_counter: 0,
            inode_size,
        }
    }
    
    /// 创建默认配置的缓存（最多32个inode，256字节大小）
    pub fn default() -> Self {
        Self::new(INODE_CACHE_MAX, INODE_SIZE as usize)
    }


    
    /// 计算inode在磁盘上的位置
    /// * `inode_num` - inode号（从1开始）
    /// * `inodes_per_group` - 每个块组的inode数
    /// * `inode_table_start` - inode表起始块号（从块组描述符获取）
    /// * `block_size` - 块大小
    /// 
    /// # 返回
    /// (块号, 块内偏移, 块组索引)
    pub fn calc_inode_location(
        &self,
        inode_num: u32,
        inodes_per_group: u32,
        inode_table_start: u64,
        block_size: usize,
    ) -> (u64, usize, u32) {
        let inode_idx = inode_num - 1;
        
        let idx_in_group = inode_idx % inodes_per_group;
        let group_idx = inode_idx / inodes_per_group;
        
        let byte_offset = idx_in_group as usize * self.inode_size;
        
        let block_offset = byte_offset / block_size;
        let offset_in_block = byte_offset % block_size;
        
        let block_num = inode_table_start + block_offset as u64;
        
        (block_num, offset_in_block, group_idx)
    }
    
    /// 从磁盘加载inode
    fn load_inode<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: u64,
        block_num: u64,
        offset: usize,
    ) -> BlockDevResult<Ext4Inode> {
        block_dev.read_block(block_num as u32)?;
        let buffer = block_dev.buffer();
        
        if offset + self.inode_size > buffer.len() {
            return Err(BlockDevError::Corrupted);
        }
        
        let inode = Ext4Inode::from_disk_bytes(&buffer[offset..offset + self.inode_size]);
        
        Ok(inode)
    }
    
    /// 获取inode（如果不存在则从磁盘加载，只读）
    /// * `block_dev` - 块设备
    /// * `inode_num` - inode号
    /// * `block_num` - inode所在的块号
    /// * `offset` - 在块内的偏移
    pub fn get_or_load<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: u64,
        block_num: u64,
        offset: usize,
    ) -> BlockDevResult<&CachedInode> {
        // 如果缓存中不存在，则加载
        if !self.cache.contains_key(&inode_num) {
            // 检查是否需要淘汰
            if self.cache.len() >= self.max_entries {
                self.evict_lru(block_dev)?;
            }

            // 从磁盘加载
            let inode = self.load_inode(block_dev, inode_num, block_num, offset)?;
            let cached = CachedInode::new(inode, inode_num, block_num, offset);
            self.cache.insert(inode_num, cached);
        }

        // 更新访问时间
        self.access_counter += 1;
        if let Some(cached) = self.cache.get_mut(&inode_num) {
            cached.last_access = self.access_counter;
        }

        self
            .cache
            .get(&inode_num)
            .ok_or(BlockDevError::Corrupted)
    }

    /// 获取可变引用（如果不存在则从磁盘加载）
    fn get_or_load_mut<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: u64,
        block_num: u64,
        offset: usize,
    ) -> BlockDevResult<&mut CachedInode> {
        // 如果缓存中不存在，则加载
        if !self.cache.contains_key(&inode_num) {
            if self.cache.len() >= self.max_entries {
                self.evict_lru(block_dev)?;
            }

            let inode = self.load_inode(block_dev, inode_num, block_num, offset)?;
            let cached = CachedInode::new(inode, inode_num, block_num, offset);
            self.cache.insert(inode_num, cached);
        }

        // 更新访问时间并返回可变引用
        self.access_counter += 1;
        if let Some(cached) = self.cache.get_mut(&inode_num) {
            cached.last_access = self.access_counter;
            Ok(cached)
        } else {
            Err(BlockDevError::Corrupted)
        }
    }
    
    /// 获取已缓存的inode（不加载）
    pub fn get(&self, inode_num: u64) -> Option<&CachedInode> {
        self.cache.get(&inode_num)
    }
    
    /// 获取可变引用
    pub fn get_mut(&mut self, inode_num: u64) -> Option<&mut CachedInode> {
        if let Some(cached) = self.cache.get_mut(&inode_num) {
            self.access_counter += 1;
            cached.last_access = self.access_counter;
            Some(cached)
        } else {
            None
        }
    }
    
    /// 标记inode为脏
    pub fn mark_dirty(&mut self, inode_num: u64) {
        if let Some(cached) = self.cache.get_mut(&inode_num) {
            cached.mark_dirty();
        }
    }

    /// 使用闭包修改指定inode，并自动标记为脏
    pub fn modify<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: u64,
        block_num: u64,
        offset: usize,
        f: F,
    ) -> BlockDevResult<()>
    where
        B: BlockDevice,
        F: FnOnce(&mut Ext4Inode),
    {
        let cached = self.get_or_load_mut(block_dev, inode_num, block_num, offset)?;
        f(&mut cached.inode);
        cached.mark_dirty();
        Ok(())
    }

    /// 使用句柄修改inode的便捷方法
    pub fn modify_by_handle<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        handle: InodeHandle,
        block_num: u64,
        offset: usize,
        f: F,
    ) -> BlockDevResult<()>
    where
        B: BlockDevice,
        F: FnOnce(&mut Ext4Inode),
    {
        self.modify(block_dev, handle.inode_num, block_num, offset, f)
    }
    
    /// LRU淘汰
    fn evict_lru<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> BlockDevResult<()> {
        let lru_key = self.cache
            .iter()
            .min_by_key(|(_, cached)| cached.last_access)
            .map(|(key, _)| *key);
        
        if let Some(key) = lru_key {
            self.evict(block_dev, key)?;
        }
        
        Ok(())
    }
    
    /// 淘汰指定的inode
    pub fn evict<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: u64,
    ) -> BlockDevResult<()> {
        if let Some(cached) = self.cache.remove(&inode_num) {
            if cached.dirty {
                Self::write_inode_static(
                    block_dev,
                    &cached.inode,
                    cached.block_num,
                    cached.offset_in_block,
                    self.inode_size,
                )?;
            }
        }
        Ok(())
    }
    
    /// 刷新所有脏inode到磁盘
    pub fn flush_all<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> BlockDevResult<()> {
        // 收集所有脏 inode，对应 (block_num, offset_in_block, encoded_bytes)
        let mut dirty_inodes: Vec<(u64, usize, Vec<u8>)> = self
            .cache
            .values()
            .filter(|cached| cached.dirty)
            .map(|cached| {
                let mut buffer = alloc::vec![0u8; self.inode_size];
                cached.inode.to_disk_bytes(&mut buffer);
                (cached.block_num, cached.offset_in_block, buffer)
            })
            .collect();

        if dirty_inodes.is_empty() {
            return Ok(());
        }

        // 先按 (block_num, offset) 排序，方便按块聚合写回
        dirty_inodes.sort_by_key(|(block_num, offset, _)| (*block_num, *offset));

        let mut idx = 0usize;
        while idx < dirty_inodes.len() {
            let (block_num, _, _) = dirty_inodes[idx];

            // 读出当前 inode 表块到 Jbd2Dev 的 buffer
            block_dev.read_block(block_num as u32)?;
            {
                let buffer = block_dev.buffer_mut();

                // 将该块上所有脏 inode 的字节写入同一个 buffer 中
                while idx < dirty_inodes.len() && dirty_inodes[idx].0 == block_num {
                    let (_b, offset, ref data) = dirty_inodes[idx];
                    let end = offset + data.len();
                    if end > buffer.len() {
                        return Err(BlockDevError::Corrupted);
                    }
                    buffer[offset..end].copy_from_slice(data);
                    idx += 1;
                }
            }

            // 该 inode 表块只调用一次 write_block，作为 metadata 走 JBD2
            block_dev.write_block(block_num as u32, true)?;
        }

        // 清除所有脏标记
        for cached in self.cache.values_mut() {
            cached.dirty = false;
        }

        Ok(())
    }
    
    /// 刷新指定inode到磁盘
    pub fn flush<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: u64,
    ) -> BlockDevResult<()> {
        if let Some(cached) = self.cache.get(&inode_num) {
            if cached.dirty {
                let block_num = cached.block_num;
                let offset = cached.offset_in_block;
                let mut buffer = alloc::vec![0u8; self.inode_size];
                cached.inode.to_disk_bytes(&mut buffer);
                
                Self::write_inode_bytes_static(block_dev, block_num, offset, &buffer)?;
                
                if let Some(cached) = self.cache.get_mut(&inode_num) {
                    cached.dirty = false;
                }
            }
        }
        Ok(())
    }
    
    /// 写inode到磁盘
    fn write_inode_static<B: BlockDevice>(
        block_dev: &mut Jbd2Dev<B>,
        inode: &Ext4Inode,
        block_num: u64,
        offset: usize,
        inode_size: usize,
    ) -> BlockDevResult<()> {
        let mut buffer = alloc::vec![0u8; inode_size];
        inode.to_disk_bytes(&mut buffer);
        Self::write_inode_bytes_static(block_dev, block_num, offset, &buffer)
    }
    
    /// 写inode字节到磁盘
    fn write_inode_bytes_static<B: BlockDevice>(
        block_dev: &mut Jbd2Dev<B>,
        block_num: u64,
        offset: usize,
        data: &[u8],
    ) -> BlockDevResult<()> {
        block_dev.read_block(block_num as u32)?;
        let buffer = block_dev.buffer_mut();
        
        buffer[offset..offset + data.len()].copy_from_slice(data);
        
        block_dev.write_block(block_num as u32,true)?;//只供崩溃恢复用
        Ok(())
    }
    
    /// 清空缓存（不写回）
    pub fn clear(&mut self) {
        self.cache.clear();
    }
    
    /// 获取缓存统计
    pub fn stats(&self) -> InodeCacheStats {
        let dirty_count = self.cache.values()
            .filter(|c| c.dirty)
            .count();
        
        InodeCacheStats {
            total_entries: self.cache.len(),
            dirty_entries: dirty_count,
            max_entries: self.max_entries,
        }
    }
}

/// Inode缓存统计信息
#[derive(Debug, Clone, Copy)]
pub struct InodeCacheStats {
    pub total_entries: usize,
    pub dirty_entries: usize,
    pub max_entries: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_inode_location_calc() {
        let cache = InodeCache::default();
        
        let inodes_per_group = 128;
        let inode_table_start = 100;
        let block_size = BLOCK_SIZE;
        
        let (block, offset, group) = cache.calc_inode_location(
            1, inodes_per_group, inode_table_start, block_size
        );
        assert_eq!(block, 100);
        assert_eq!(offset, 0);
        assert_eq!(group, 0);
        
     
        let (block, offset, group) = cache.calc_inode_location(
            (INODES_PER_BLOCK + 1) as u32,
            inodes_per_group,
            inode_table_start,
            block_size,
        );
        assert_eq!(block, inode_table_start + 1);
        assert_eq!(offset, 0);
        assert_eq!(group, 0);
    }
    
    #[test]
    fn test_inode_cache_basic() {
        let cache = InodeCache::new(4, 256);
        let stats = cache.stats();
        
        assert_eq!(stats.total_entries, 0);
        assert_eq!(stats.max_entries, 4);
    }
}
