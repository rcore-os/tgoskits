//! Bitmap cache helpers.

use alloc::{collections::BTreeMap, vec::Vec};

use log::debug;
use spin::Mutex as SpinMutex;

use crate::{
    BITMAP_CACHE_MAX,
    blockdev::*,
    bmalloc::{AbsoluteBN, BGIndex},
    config::USE_MULTILEVEL_CACHE,
    error::*,
};

/// Snapshot type for lock-free LRU eviction.
/// `(lru_key, optional dirty data: (block_num, data))`
type BitmapLruSnapshot = Option<(CacheKey, Option<(AbsoluteBN, Vec<u8>)>)>;

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

/// Bitmap cache internal state — protected by `SpinMutex`.
struct BitmapCacheInner {
    /// Cached bitmaps.
    cache: BTreeMap<CacheKey, CachedBitmap>,
    /// Maximum number of cache entries.
    max_entries: usize,
    /// Access counter used by the LRU policy.
    access_counter: u64,
}

/// Bitmap cache manager with internal spinlock for SMP-safe concurrent access.
///
/// All methods take `&self`; the internal `SpinMutex` provides interior mutability.
pub struct BitmapCache {
    inner: SpinMutex<BitmapCacheInner>,
}

impl BitmapCache {
    /// Creates a bitmap cache.
    pub fn new(max_entries: usize) -> Self {
        Self {
            inner: SpinMutex::new(BitmapCacheInner {
                cache: BTreeMap::new(),
                max_entries,
                access_counter: 0,
            }),
        }
    }

    /// Creates a bitmap cache with the default size.
    pub fn create_default() -> Self {
        Self::new(BITMAP_CACHE_MAX)
    }

    /// Returns a cached bitmap, loading it from disk on demand.
    pub fn get_or_load<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        key: CacheKey,
        block_num: AbsoluteBN,
    ) -> Ext4Result<CachedBitmap> {
        let mut inner = self.inner.lock();

        if !inner.cache.contains_key(&key) {
            // Phase 1: snapshot LRU eviction info while holding the lock.
            let evict_info = if inner.cache.len() >= inner.max_entries {
                inner.snapshot_lru()
            } else {
                None
            };

            drop(inner);

            // Phase 2: do I/O without holding the spinlock.
            if let Some((_lru_key, Some((lru_bn, ref lru_data)))) = evict_info {
                Self::write_bitmap_static(block_dev, lru_bn, lru_data)?;
            }

            let mut buf = alloc::vec![0u8; crate::config::BLOCK_SIZE];
            block_dev.read_blocks(&mut buf, block_num, 1)?;

            // Phase 3: reacquire the lock and apply the eviction + insertion.
            inner = self.inner.lock();

            if let Some((lru_key, _)) = evict_info {
                inner.cache.remove(&lru_key);
            }

            inner
                .cache
                .entry(key)
                .or_insert_with(|| CachedBitmap::new(buf, block_num));
        }

        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        if let Some(bitmap) = inner.cache.get_mut(&key) {
            bitmap.last_access = new_counter;
        }

        inner.cache.get(&key).cloned().ok_or(Ext4Error::corrupted())
    }

    /// Returns a mutable cached bitmap, loading it from disk on demand.
    pub(crate) fn get_or_load_mut<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        key: CacheKey,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        let mut inner = self.inner.lock();

        if !inner.cache.contains_key(&key) {
            // Phase 1: snapshot LRU eviction info while holding the lock.
            let evict_info = if inner.cache.len() >= inner.max_entries {
                inner.snapshot_lru()
            } else {
                None
            };

            drop(inner);

            // Phase 2: do I/O without holding the spinlock.
            if let Some((_lru_key, Some((lru_bn, ref lru_data)))) = evict_info {
                Self::write_bitmap_static(block_dev, lru_bn, lru_data)?;
            }

            let mut buf = alloc::vec![0u8; crate::config::BLOCK_SIZE];
            block_dev.read_blocks(&mut buf, block_num, 1)?;

            // Phase 3: reacquire the lock and apply the eviction + insertion.
            inner = self.inner.lock();

            if let Some((lru_key, _)) = evict_info {
                inner.cache.remove(&lru_key);
            }

            // Re-check after reacquiring: another thread may have inserted the
            // same key while we held no lock.
            inner
                .cache
                .entry(key)
                .or_insert_with(|| CachedBitmap::new(buf, block_num));
        }

        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        if let Some(bitmap) = inner.cache.get_mut(&key) {
            bitmap.last_access = new_counter;
        }
        Ok(())
    }

    /// Returns a cached bitmap without loading from disk.
    pub fn get(&self, key: &CacheKey) -> Option<CachedBitmap> {
        self.inner.lock().cache.get(key).cloned()
    }

    /// Returns a mutable cached bitmap without loading from disk.
    pub fn get_mut(&self, key: &CacheKey) -> Option<CachedBitmap> {
        let mut inner = self.inner.lock();
        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        inner.cache.get_mut(key).map(|bitmap| {
            bitmap.last_access = new_counter;
            bitmap.clone()
        })
    }

    /// Marks a cached bitmap dirty.
    pub fn mark_dirty(&self, key: &CacheKey) {
        if let Some(bitmap) = self.inner.lock().cache.get_mut(key) {
            bitmap.mark_dirty();
        }
    }

    /// Modifies one cached bitmap and marks it dirty.
    pub fn modify<B, F>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        key: CacheKey,
        block_num: AbsoluteBN,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockDevice,
        F: FnOnce(&mut [u8]),
    {
        self.get_or_load_mut(block_dev, key, block_num)?;

        let mut inner = self.inner.lock();
        if let Some(bitmap) = inner.cache.get_mut(&key) {
            debug!(
                "BitmapCache::modify: key=({}:{:?}) block_num={} before_dirty={}",
                key.group_id, key.bitmap_type, block_num, bitmap.dirty
            );

            f(&mut bitmap.data);
            bitmap.mark_dirty();

            if !USE_MULTILEVEL_CACHE {
                let data = bitmap.data.clone();
                let blk = bitmap.block_num;
                drop(inner);
                Self::write_bitmap_static(block_dev, blk, &data)?;
                inner = self.inner.lock();
                if let Some(bitmap) = inner.cache.get_mut(&key) {
                    bitmap.dirty = false;
                }
            }

            debug!(
                "BitmapCache::modify: key=({}:{:?}) block_num={} marked_dirty=true",
                key.group_id, key.bitmap_type, block_num
            );
        }
        Ok(())
    }

    /// Evicts one cached bitmap.
    pub fn evict<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        key: &CacheKey,
    ) -> Ext4Result<()> {
        self.inner.lock().do_evict(block_dev, key)
    }

    /// Flushes all dirty bitmaps to disk.
    pub fn flush_all<B: BlockDevice>(&self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        self.inner.lock().do_flush_all(block_dev)
    }

    /// Flushes one bitmap to disk.
    pub fn flush<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        key: &CacheKey,
    ) -> Ext4Result<()> {
        self.inner.lock().do_flush(block_dev, key)
    }

    /// Clears the cache without flushing.
    pub fn clear(&self) {
        self.inner.lock().cache.clear();
    }

    /// Returns cache statistics.
    pub fn stats(&self) -> CacheStats {
        let inner = self.inner.lock();
        let dirty_count = inner.cache.values().filter(|b| b.dirty).count();

        CacheStats {
            total_entries: inner.cache.len(),
            dirty_entries: dirty_count,
            max_entries: inner.max_entries,
        }
    }

    /// Writes one bitmap block to disk (static helper, uses local buffer).
    fn write_bitmap_static<B: BlockDevice>(
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        data: &[u8],
    ) -> Ext4Result<()> {
        let mut buf = alloc::vec![0u8; crate::config::BLOCK_SIZE];
        block_dev.read_blocks(&mut buf, block_num, 1)?;
        buf[..data.len()].copy_from_slice(data);
        block_dev.write_blocks(&buf, block_num, 1, true)?;
        Ok(())
    }
}

/// Cache statistics.
#[derive(Debug, Clone, Copy)]
pub struct CacheStats {
    pub total_entries: usize,
    pub dirty_entries: usize,
    pub max_entries: usize,
}

// ── Inner methods (caller holds `self.inner.lock()`) ─────────────────────────

impl BitmapCacheInner {
    /// Snapshots the LRU bitmap for lock-free eviction.
    ///
    /// Returns `Some(lru_key, Some(data))` when dirty, `Some(lru_key, None)` when
    /// clean, `None` when the cache is empty.  The caller must do the I/O
    /// *without* holding the spinlock, then re-lock and remove `lru_key`.
    fn snapshot_lru(&self) -> BitmapLruSnapshot {
        let lru_key = self
            .cache
            .iter()
            .min_by_key(|(_, bitmap)| bitmap.last_access)
            .map(|(key, _)| *key)?;

        let dirty_info = self.cache.get(&lru_key).and_then(|bitmap| {
            if bitmap.dirty {
                Some((bitmap.block_num, bitmap.data.clone()))
            } else {
                None
            }
        });

        Some((lru_key, dirty_info))
    }

    fn do_evict<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: &CacheKey,
    ) -> Ext4Result<()> {
        if let Some(bitmap) = self.cache.remove(key)
            && bitmap.dirty
        {
            BitmapCache::write_bitmap_static(block_dev, bitmap.block_num, &bitmap.data)?;
        }
        Ok(())
    }

    fn do_flush<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        key: &CacheKey,
    ) -> Ext4Result<()> {
        if let Some(bitmap) = self.cache.get(key)
            && bitmap.dirty
        {
            let data = bitmap.data.clone();
            BitmapCache::write_bitmap_static(block_dev, bitmap.block_num, &data)?;
            if let Some(bitmap) = self.cache.get_mut(key) {
                bitmap.dirty = false;
            }
        }
        Ok(())
    }

    fn do_flush_all<B: BlockDevice>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        let mut dirty_bitmaps: Vec<(CacheKey, AbsoluteBN, Vec<u8>)> = self
            .cache
            .iter()
            .filter(|(_, bitmap)| bitmap.dirty)
            .map(|(key, bitmap)| (*key, bitmap.block_num, bitmap.data.clone()))
            .collect();

        if dirty_bitmaps.is_empty() {
            return Ok(());
        }

        dirty_bitmaps.sort_by_key(|(_, block_num, _)| *block_num);

        debug!(
            "BitmapCache::flush_all: dirty_entries={}",
            dirty_bitmaps.len()
        );

        for (key, block_num, data) in dirty_bitmaps {
            debug!(
                "BitmapCache::flush_all: writing bitmap key=({}:{:?}) block_num={}",
                key.group_id, key.bitmap_type, block_num
            );
            BitmapCache::write_bitmap_static(block_dev, block_num, &data)?;
        }

        for bitmap in self.cache.values_mut() {
            bitmap.dirty = false;
        }
        Ok(())
    }
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
