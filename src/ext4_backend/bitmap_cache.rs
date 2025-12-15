//! 位图缓存模块

use crate::ext4_backend::blockdev::*;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use crate::ext4_backend::error::*;
use log::debug;

/// 位图类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BitmapType {
    /// 块位图
    Block,
    /// Inode位图
    Inode,
}

/// 缓存键：(块组ID, 位图类型)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheKey {
    pub group_id: u32,
    pub bitmap_type: BitmapType,
}

impl CacheKey {
    pub fn new_block(group_id: u32) -> Self {
        Self {
            group_id,
            bitmap_type: BitmapType::Block,
        }
    }

    pub fn new_inode(group_id: u32) -> Self {
        Self {
            group_id,
            bitmap_type: BitmapType::Inode,
        }
    }
}

/// 缓存的位图数据
#[derive(Debug, Clone)]
pub struct CachedBitmap {
    /// 位图数据
    pub data: Vec<u8>,
    /// 是否被修改（脏）
    pub dirty: bool,
    /// 磁盘块号
    pub block_num: u64,
    /// 最后访问时间戳（用于LRU）
    pub last_access: u64,
}

impl CachedBitmap {
    pub fn new(data: Vec<u8>, block_num: u64) -> Self {
        Self {
            data,
            dirty: false,
            block_num,
            last_access: 0,
        }
    }

    /// 标记为脏
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

/// 位图缓存管理器
pub struct BitmapCache {
    /// 缓存的位图
    cache: BTreeMap<CacheKey, CachedBitmap>,
    /// 最大缓存条目数（LRU淘汰）
    max_entries: usize,
    /// 访问计数器（用于LRU）
    access_counter: u64,
}

impl BitmapCache {
    /// 创建位图缓存
    pub fn new(max_entries: usize) -> Self {
        Self {
            cache: BTreeMap::new(),
            max_entries,
            access_counter: 0,
        }
    }

    /// 创建默认配置的缓存
    pub fn default() -> Self {
        Self::new(8)
    }

    /// 获取位图（如果不存在则从磁盘加载） - 只读视图
    /// * `block_dev` - 块设备
    /// * `key` - 缓存键
    /// * `block_num` - 位图在磁盘上的块号
    pub fn get_or_load<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: CacheKey,
        block_num: u64,
    ) -> BlockDevResult<&CachedBitmap> {
        if !self.cache.contains_key(&key) {
            if self.cache.len() >= self.max_entries {
                self.evict_lru(block_dev)?;
            }

            block_dev.read_block(block_num as u32)?;
            let buffer = block_dev.buffer();
            let data = buffer.to_vec();

            let bitmap = CachedBitmap::new(data, block_num);
            self.cache.insert(key, bitmap);
        }

        self.access_counter += 1;
        if let Some(bitmap) = self.cache.get_mut(&key) {
            bitmap.last_access = self.access_counter;
        }

        self.cache.get(&key).ok_or(BlockDevError::Corrupted)
    }

    /// 内部使用：获取可变引用（如果不存在则从磁盘加载）
    fn get_or_load_mut<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: CacheKey,
        block_num: u64,
    ) -> BlockDevResult<&mut CachedBitmap> {
        if !self.cache.contains_key(&key) {
            if self.cache.len() >= self.max_entries {
                self.evict_lru(block_dev)?;
            }

            block_dev.read_block(block_num as u32)?;
            let buffer = block_dev.buffer();
            let data = buffer.to_vec();

            let bitmap = CachedBitmap::new(data, block_num);
            self.cache.insert(key, bitmap);
        }

        self.access_counter += 1;
        if let Some(bitmap) = self.cache.get_mut(&key) {
            bitmap.last_access = self.access_counter;
            Ok(bitmap)
        } else {
            Err(BlockDevError::Corrupted)
        }
    }

    /// 获取已缓存的位图（不加载）
    pub fn get(&self, key: &CacheKey) -> Option<&CachedBitmap> {
        self.cache.get(key)
    }

    /// 获取可变引用
    pub fn get_mut(&mut self, key: &CacheKey) -> Option<&mut CachedBitmap> {
        self.cache.get_mut(key)
    }

    /// 标记位图为脏
    pub fn mark_dirty(&mut self, key: &CacheKey) {
        if let Some(bitmap) = self.cache.get_mut(key) {
            bitmap.mark_dirty();
        }
    }

    /// 使用闭包修改指定位图，并自动标记为脏
    pub fn modify<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: CacheKey,
        block_num: u64,
        f: F,
    ) -> BlockDevResult<()>
    where
        B: BlockDevice,
        F: FnOnce(&mut [u8]),
    {
        let bitmap = self.get_or_load_mut(block_dev, key, block_num)?;
        debug!(
            "BitmapCache::modify: key=({}:{:?}) block_num={} before_dirty={} (will apply in-memory changes)",
            key.group_id, key.bitmap_type, block_num, bitmap.dirty
        );

        f(&mut bitmap.data);
        bitmap.mark_dirty();

        debug!(
            "BitmapCache::modify: key=({}:{:?}) block_num={} marked_dirty=true (bitmap updated in cache, writeback deferred)",
            key.group_id, key.bitmap_type, block_num
        );
        Ok(())
    }

    /// LRU淘汰：找到最久未访问的并写回（如果脏）
    fn evict_lru<B: BlockDevice>(&mut self, block_dev: &mut Jbd2Dev<B>) -> BlockDevResult<()> {
        let lru_key = self
            .cache
            .iter()
            .min_by_key(|(_, bitmap)| bitmap.last_access)
            .map(|(key, _)| *key);

        if let Some(key) = lru_key {
            self.evict(block_dev, &key)?;
        }

        Ok(())
    }

    /// 淘汰指定的位图
    pub fn evict<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: &CacheKey,
    ) -> BlockDevResult<()> {
        if let Some(bitmap) = self.cache.remove(key)
            && bitmap.dirty {
                Self::write_bitmap_static(block_dev, bitmap.block_num, &bitmap.data)?;
            }
        Ok(())
    }

    /// 刷新所有脏位图到磁盘
    pub fn flush_all<B: BlockDevice>(&mut self, block_dev: &mut Jbd2Dev<B>) -> BlockDevResult<()> {
        let mut dirty_bitmaps: Vec<(CacheKey, u64, Vec<u8>)> = self
            .cache
            .iter()
            .filter(|(_, bitmap)| bitmap.dirty)
            .map(|(key, bitmap)| (*key, bitmap.block_num, bitmap.data.clone()))
            .collect();

        if dirty_bitmaps.is_empty() {
            return Ok(());
        }

        // 按物理块号排序，尽量让写入顺序更顺滑
        dirty_bitmaps.sort_by_key(|(_, block_num, _)| *block_num);

        debug!(
            "BitmapCache::flush_all: dirty_entries={} (will write all dirty bitmaps to disk)",
            dirty_bitmaps.len()
        );

        for (key, block_num, data) in dirty_bitmaps {
            debug!(
                "BitmapCache::flush_all: writing bitmap key=({}:{:?}) block_num={} to disk",
                key.group_id, key.bitmap_type, block_num
            );

            Self::write_bitmap_static(block_dev, block_num, &data)?;
        }

        for bitmap in self.cache.values_mut() {
            bitmap.dirty = false;
        }

        Ok(())
    }

    /// 刷新指定位图到磁盘
    pub fn flush<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: &CacheKey,
    ) -> BlockDevResult<()> {
        if let Some(bitmap) = self.cache.get(key)
            && bitmap.dirty {
                let block_num = bitmap.block_num;
                let data = bitmap.data.clone();

                // 写回磁盘
                Self::write_bitmap_static(block_dev, block_num, &data)?;

                // 清除脏标记
                if let Some(bitmap) = self.cache.get_mut(key) {
                    bitmap.dirty = false;
                }
            }
        Ok(())
    }

    /// 静态方法：写位图到磁盘
    fn write_bitmap_static<B: BlockDevice>(
        block_dev: &mut Jbd2Dev<B>,
        block_num: u64,
        data: &[u8],
    ) -> BlockDevResult<()> {
        block_dev.read_block(block_num as u32)?;
        let buffer = block_dev.buffer_mut();
        buffer[..data.len()].copy_from_slice(data);
        block_dev.write_block(block_num as u32, true)?;
        Ok(())
    }

    /// 清空缓存（不写回）
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// 获取缓存统计
    pub fn stats(&self) -> CacheStats {
        let dirty_count = self.cache.values().filter(|b| b.dirty).count();

        CacheStats {
            total_entries: self.cache.len(),
            dirty_entries: dirty_count,
            max_entries: self.max_entries,
        }
    }
}

/// 缓存统计信息
#[derive(Debug, Clone, Copy)]
pub struct CacheStats {
    pub total_entries: usize,
    pub dirty_entries: usize,
    pub max_entries: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn test_cache_key() {
        let key1 = CacheKey::new_block(0);
        let key2 = CacheKey::new_block(0);
        let key3 = CacheKey::new_inode(0);

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_cached_bitmap() {
        use crate::BLOCK_SIZE;
        let data = vec![0u8; BLOCK_SIZE];
        let mut bitmap = CachedBitmap::new(data, 10);

        assert!(!bitmap.dirty);
        bitmap.mark_dirty();
        assert!(bitmap.dirty);
    }

    #[test]
    fn test_bitmap_cache_basic() {
        let cache = BitmapCache::new(4);
        let stats = cache.stats();

        assert_eq!(stats.total_entries, 0);
        assert_eq!(stats.max_entries, 4);
    }
}
