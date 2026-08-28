//! Bitmap cache helpers.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

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
    pub data: Arc<Vec<u8>>,
    /// Whether the cache entry is dirty.
    pub dirty: bool,
    /// Physical block storing the bitmap.
    pub block_num: AbsoluteBN,
    /// Access timestamp for LRU eviction.
    pub last_access: u64,
    /// Generation counter bumped on every access.
    pub generation: u64,
}

impl CachedBitmap {
    pub fn new(data: Vec<u8>, block_num: AbsoluteBN) -> Self {
        Self {
            data: Arc::new(data),
            dirty: false,
            block_num,
            last_access: 0,
            generation: 0,
        }
    }

    /// Marks the bitmap entry dirty.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

/// Bitmap cache owned exclusively by one mounted filesystem.
///
/// The cache deliberately has no internal lock. Callers need mutable access to
/// the mounted filesystem before changing cache state; an OS adapter may place
/// its own sleepable lock around that owner.
#[derive(Clone)]
pub struct BitmapCache {
    cache: BTreeMap<CacheKey, CachedBitmap>,
    max_entries: usize,
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
    pub fn get_or_load<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: CacheKey,
        block_num: AbsoluteBN,
    ) -> Ext4Result<CachedBitmap> {
        self.ensure_loaded(block_dev, key, block_num)?;
        self.touch(key);
        self.cache.get(&key).cloned().ok_or(Ext4Error::corrupted())
    }

    fn ensure_loaded<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: CacheKey,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        if self.cache.contains_key(&key) {
            return Ok(());
        }

        // Read first so a read failure cannot evict a valid cached entry.
        let mut data = alloc::vec![0u8; block_dev.block_size() as usize];
        block_dev.read_blocks(&mut data, block_num, 1)?;

        if self.cache.len() >= self.max_entries
            && let Some(victim_key) = self.lru_key()
        {
            let victim = self
                .cache
                .get(&victim_key)
                .cloned()
                .ok_or(Ext4Error::corrupted())?;
            if victim.dirty {
                Self::write_bitmap_static(block_dev, victim.block_num, &victim.data)?;
            }
            self.cache.remove(&victim_key);
        }

        self.cache.insert(key, CachedBitmap::new(data, block_num));
        Ok(())
    }

    fn touch(&mut self, key: CacheKey) {
        self.access_counter = self.access_counter.saturating_add(1);
        if let Some(bitmap) = self.cache.get_mut(&key) {
            bitmap.last_access = self.access_counter;
            bitmap.generation = bitmap.generation.saturating_add(1);
        }
    }

    fn lru_key(&self) -> Option<CacheKey> {
        self.cache
            .iter()
            .min_by_key(|(_, bitmap)| bitmap.last_access)
            .map(|(key, _)| *key)
    }

    /// Returns a cached bitmap without loading from disk.
    pub fn get(&self, key: &CacheKey) -> Option<CachedBitmap> {
        self.cache.get(key).cloned()
    }

    /// Returns an owned mutable-view snapshot and refreshes its LRU state.
    pub fn get_mut(&mut self, key: &CacheKey) -> Option<CachedBitmap> {
        self.touch(*key);
        self.cache.get(key).cloned()
    }

    /// Marks a cached bitmap dirty.
    pub fn mark_dirty(&mut self, key: &CacheKey) {
        if let Some(bitmap) = self.cache.get_mut(key) {
            bitmap.mark_dirty();
            bitmap.generation = bitmap.generation.saturating_add(1);
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
        B: BlockIo,
        F: FnOnce(&mut [u8]),
    {
        self.ensure_loaded(block_dev, key, block_num)?;
        self.touch(key);

        let bitmap = self.cache.get_mut(&key).ok_or(Ext4Error::corrupted())?;
        f(Arc::make_mut(&mut bitmap.data).as_mut_slice());
        bitmap.mark_dirty();
        bitmap.generation = bitmap.generation.saturating_add(1);

        if !USE_MULTILEVEL_CACHE {
            let data = bitmap.data.clone();
            let block_num = bitmap.block_num;
            Self::write_bitmap_static(block_dev, block_num, &data)?;
            let bitmap = self.cache.get_mut(&key).ok_or(Ext4Error::corrupted())?;
            bitmap.dirty = false;
            bitmap.generation = bitmap.generation.saturating_add(1);
        }
        Ok(())
    }

    /// Evicts one cached bitmap.
    pub fn evict<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: &CacheKey,
    ) -> Ext4Result<()> {
        let Some(bitmap) = self.cache.get(key).cloned() else {
            return Ok(());
        };
        if bitmap.dirty {
            Self::write_bitmap_static(block_dev, bitmap.block_num, &bitmap.data)?;
        }
        self.cache.remove(key);
        Ok(())
    }

    /// Flushes all dirty bitmaps to disk.
    pub fn flush_all<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        let mut dirty = self
            .cache
            .iter()
            .filter(|(_, bitmap)| bitmap.dirty)
            .map(|(key, bitmap)| (*key, bitmap.block_num, bitmap.data.clone()))
            .collect::<Vec<_>>();
        dirty.sort_by_key(|(_, block_num, _)| *block_num);

        for (_, block_num, data) in &dirty {
            Self::write_bitmap_static(block_dev, *block_num, data)?;
        }
        for (key, ..) in dirty {
            if let Some(bitmap) = self.cache.get_mut(&key) {
                bitmap.dirty = false;
                bitmap.generation = bitmap.generation.saturating_add(1);
            }
        }
        Ok(())
    }

    /// Flushes one bitmap to disk.
    pub fn flush<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: &CacheKey,
    ) -> Ext4Result<()> {
        let Some(bitmap) = self.cache.get(key).cloned() else {
            return Ok(());
        };
        if bitmap.dirty {
            Self::write_bitmap_static(block_dev, bitmap.block_num, &bitmap.data)?;
            let bitmap = self.cache.get_mut(key).ok_or(Ext4Error::corrupted())?;
            bitmap.dirty = false;
            bitmap.generation = bitmap.generation.saturating_add(1);
        }
        Ok(())
    }

    /// Clears the cache without flushing.
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Returns cache statistics.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            total_entries: self.cache.len(),
            dirty_entries: self.cache.values().filter(|bitmap| bitmap.dirty).count(),
            max_entries: self.max_entries,
        }
    }

    fn write_bitmap_static<B: BlockIo>(
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        data: &[u8],
    ) -> Ext4Result<()> {
        let block_size = block_dev.block_size() as usize;
        let mut buffer = alloc::vec![0u8; block_size];
        block_dev.read_blocks(&mut buffer, block_num, 1)?;
        let len = core::cmp::min(data.len(), block_size);
        buffer[..len].copy_from_slice(&data[..len]);
        block_dev.write_blocks(&buffer, block_num, 1, true)
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
        let data = vec![0u8; crate::config::BLOCK_SIZE];
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
