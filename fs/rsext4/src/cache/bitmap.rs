//! Bitmap cache helpers.

use alloc::{collections::BTreeMap, vec::Vec};

use log::debug;

use crate::{
    BITMAP_CACHE_MAX,
    blockdev::*,
    bmalloc::{AbsoluteBN, BGIndex},
    config::USE_MULTILEVEL_CACHE,
    error::*,
};

/// Type of bitmap stored in the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BitmapType {
    /// Block bitmap.
    Block,
    /// Inode bitmap.
    Inode,
}

/// Cache key for one bitmap in one block group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheKey {
    pub group_id: BGIndex,
    pub bitmap_type: BitmapType,
}

impl CacheKey {
    pub fn new_block(group_id: BGIndex) -> Self {
        Self {
            group_id,
            bitmap_type: BitmapType::Block,
        }
    }

    pub fn new_inode(group_id: BGIndex) -> Self {
        Self {
            group_id,
            bitmap_type: BitmapType::Inode,
        }
    }
}

/// Cached bitmap payload.
#[derive(Debug, Clone)]
pub struct CachedBitmap {
    /// Bitmap bytes.
    pub data: Vec<u8>,
    /// Whether the cache entry is dirty.
    pub dirty: bool,
    /// Physical block storing the bitmap.
    pub block_num: AbsoluteBN,
    /// Access timestamp for LRU eviction.
    pub last_access: u64,
}

impl CachedBitmap {
    pub fn new(data: Vec<u8>, block_num: AbsoluteBN) -> Self {
        Self {
            data,
            dirty: false,
            block_num,
            last_access: 0,
        }
    }

    /// Marks the bitmap entry dirty.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

/// Bitmap cache manager.
pub struct BitmapCache {
    /// Cached bitmaps.
    cache: BTreeMap<CacheKey, CachedBitmap>,
    /// Maximum number of cache entries.
    max_entries: usize,
    /// Access counter used by the LRU policy.
    access_counter: u64,
}

impl BitmapCache {
    /// Creates a bitmap cache.
    pub fn new(max_entries: usize) -> Self {
        Self {
            cache: BTreeMap::new(),
            max_entries,
            access_counter: 0,
        }
    }

    /// Creates a bitmap cache with the default size.
    pub fn create_default() -> Self {
        Self::new(BITMAP_CACHE_MAX)
    }

    /// Returns a cached bitmap, loading it from disk on demand.
    pub fn get_or_load<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: CacheKey,
        block_num: AbsoluteBN,
    ) -> Ext4Result<&CachedBitmap> {
        if !self.cache.contains_key(&key) {
            if self.cache.len() >= self.max_entries {
                self.evict_lru(block_dev)?;
            }

            block_dev.read_block(block_num)?;
            let buffer = block_dev.buffer();
            let data = buffer.to_vec();

            let bitmap = CachedBitmap::new(data, block_num);
            self.cache.insert(key, bitmap);
        }

        self.access_counter += 1;
        if let Some(bitmap) = self.cache.get_mut(&key) {
            bitmap.last_access = self.access_counter;
        }

        self.cache.get(&key).ok_or(Ext4Error::corrupted())
    }

    /// Returns a mutable cached bitmap, loading it from disk on demand.
    pub(crate) fn get_or_load_mut<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: CacheKey,
        block_num: AbsoluteBN,
    ) -> Ext4Result<&mut CachedBitmap> {
        if !self.cache.contains_key(&key) {
            if self.cache.len() >= self.max_entries {
                self.evict_lru(block_dev)?;
            }

            block_dev.read_block(block_num)?;
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
            Err(Ext4Error::corrupted())
        }
    }

    /// Returns a cached bitmap without loading from disk.
    pub fn get(&self, key: &CacheKey) -> Option<&CachedBitmap> {
        self.cache.get(key)
    }

    /// Returns a mutable cached bitmap without loading from disk.
    pub fn get_mut(&mut self, key: &CacheKey) -> Option<&mut CachedBitmap> {
        self.cache.get_mut(key)
    }

    /// Marks a cached bitmap dirty.
    pub fn mark_dirty(&mut self, key: &CacheKey) {
        if let Some(bitmap) = self.cache.get_mut(key) {
            bitmap.mark_dirty();
        }
    }

    /// Modifies one cached bitmap and marks it dirty.
    pub fn modify<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: CacheKey,
        block_num: AbsoluteBN,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockDevice,
        F: FnOnce(&mut [u8]),
    {
        let bitmap = self.get_or_load_mut(block_dev, key, block_num)?;
        debug!(
            "BitmapCache::modify: key=({}:{:?}) block_num={} before_dirty={} (will apply \
             in-memory changes)",
            key.group_id, key.bitmap_type, block_num, bitmap.dirty
        );

        f(&mut bitmap.data);
        bitmap.mark_dirty();

        if !USE_MULTILEVEL_CACHE {
            Self::write_bitmap_static(block_dev, bitmap.block_num, &bitmap.data)?;
            bitmap.dirty = false;
        }

        debug!(
            "BitmapCache::modify: key=({}:{:?}) block_num={} marked_dirty=true (bitmap updated in \
             cache, writeback deferred)",
            key.group_id, key.bitmap_type, block_num
        );
        Ok(())
    }

    /// Evicts the least recently used bitmap, flushing it first if needed.
    fn evict_lru<B: BlockDevice>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
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

    /// Evicts one cached bitmap.
    pub fn evict<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: &CacheKey,
    ) -> Ext4Result<()> {
        if let Some(bitmap) = self.cache.remove(key)
            && bitmap.dirty
        {
            Self::write_bitmap_static(block_dev, bitmap.block_num, &bitmap.data)?;
        }
        Ok(())
    }

    /// Flushes all dirty bitmaps to disk.
    pub fn flush_all<B: BlockDevice>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        let mut dirty_bitmaps: Vec<(CacheKey, AbsoluteBN, Vec<u8>)> = self
            .cache
            .iter()
            .filter(|(_, bitmap)| bitmap.dirty)
            .map(|(key, bitmap)| (*key, bitmap.block_num, bitmap.data.clone()))
            .collect();

        if dirty_bitmaps.is_empty() {
            return Ok(());
        }

        // Sort by physical block to keep writes ordered.
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

    /// Flushes one bitmap to disk.
    pub fn flush<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: &CacheKey,
    ) -> Ext4Result<()> {
        if let Some(bitmap) = self.cache.get(key)
            && bitmap.dirty
        {
            let block_num = bitmap.block_num;
            let data = bitmap.data.clone();

            Self::write_bitmap_static(block_dev, block_num, &data)?;

            if let Some(bitmap) = self.cache.get_mut(key) {
                bitmap.dirty = false;
            }
        }
        Ok(())
    }

    /// Writes one bitmap block to disk.
    fn write_bitmap_static<B: BlockDevice>(
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        data: &[u8],
    ) -> Ext4Result<()> {
        block_dev.read_block(block_num)?;
        let buffer = block_dev.buffer_mut();
        buffer[..data.len()].copy_from_slice(data);
        block_dev.write_block(block_num, true)?;
        Ok(())
    }

    /// Clears the cache without flushing.
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Returns cache statistics.
    pub fn stats(&self) -> CacheStats {
        let dirty_count = self.cache.values().filter(|b| b.dirty).count();

        CacheStats {
            total_entries: self.cache.len(),
            dirty_entries: dirty_count,
            max_entries: self.max_entries,
        }
    }
}

/// Cache statistics.
#[derive(Debug, Clone, Copy)]
pub struct CacheStats {
    pub total_entries: usize,
    pub dirty_entries: usize,
    pub max_entries: usize,
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn test_cache_key() {
        let key1 = CacheKey::new_block(BGIndex::new(0));
        let key2 = CacheKey::new_block(BGIndex::new(0));
        let key3 = CacheKey::new_inode(BGIndex::new(0));

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_cached_bitmap() {
        use crate::BLOCK_SIZE;
        let data = vec![0u8; BLOCK_SIZE];
        let mut bitmap = CachedBitmap::new(data, AbsoluteBN::new(10));

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
