//! Data block cache helpers.

use alloc::{collections::BTreeMap, vec::Vec};

use spin::Mutex as SpinMutex;

use crate::{blockdev::*, bmalloc::AbsoluteBN, config::*, error::*};
/// Cache key for one physical data block.
pub type BlockCacheKey = AbsoluteBN;

/// Cached data block.
#[derive(Debug, Clone)]
pub struct CachedBlock {
    /// Block contents.
    pub data: Vec<u8>,
    /// Whether the cache entry is dirty.
    pub dirty: bool,
    /// Physical block number.
    pub block_num: AbsoluteBN,
    /// Access timestamp used for LRU eviction.
    pub last_access: u64,
}

impl CachedBlock {
    pub fn new(data: Vec<u8>, block_num: AbsoluteBN) -> Self {
        Self {
            data,
            dirty: false,
            block_num,
            last_access: 0,
        }
    }

    /// Marks the block dirty.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

/// Data block cache internal state — protected by `SpinMutex`.
struct DataBlockCacheInner {
    /// Cached blocks.
    cache: BTreeMap<BlockCacheKey, CachedBlock>,
    /// Maximum number of cache entries.
    max_entries: usize,
    /// Access counter used by the LRU policy.
    access_counter: u64,
    /// Filesystem block size.
    block_size: usize,
}

/// Data block cache manager with internal spinlock for SMP-safe concurrent access.
///
/// All methods take `&self`; the internal `SpinMutex` provides interior mutability.
/// Callers must ensure the block device (`Jbd2Dev`) is externally synchronized.
pub struct DataBlockCache {
    inner: SpinMutex<DataBlockCacheInner>,
    /// Filesystem block size in bytes (immutable after construction).
    block_size: usize,
}

impl DataBlockCache {
    /// Creates a data block cache.
    pub fn new(max_entries: usize, block_size: usize) -> Self {
        Self {
            inner: SpinMutex::new(DataBlockCacheInner {
                cache: BTreeMap::new(),
                max_entries,
                access_counter: 0,
                block_size,
            }),
            block_size,
        }
    }

    /// Creates a data block cache with default settings.
    pub fn create_default() -> Self {
        Self::new(64, BLOCK_SIZE)
    }

    /// Loads one block from disk using a caller-provided buffer.
    fn load_block<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<Vec<u8>> {
        let mut buf = alloc::vec![0u8; self.block_size];
        block_dev.read_blocks(&mut buf, block_num, 1)?;
        Ok(buf)
    }

    /// Returns a cached block, loading it from disk on demand.
    pub fn get_or_load<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<CachedBlock> {
        let mut inner = self.inner.lock();

        if !inner.cache.contains_key(&block_num) {
            // Phase 1: snapshot LRU eviction info while holding the lock.
            let evict_info = if inner.cache.len() >= inner.max_entries {
                inner.snapshot_lru()
            } else {
                None
            };

            drop(inner);

            // Phase 2: do I/O without holding the spinlock.
            if let Some((_lru_key, Some(ref lru_data))) = evict_info {
                Self::write_block_static(block_dev, _lru_key, lru_data, self.block_size)?;
            }

            let data = self.load_block(block_dev, block_num)?;

            // Phase 3: reacquire the lock and apply the eviction + insertion.
            inner = self.inner.lock();

            if let Some((lru_key, _)) = evict_info {
                inner.cache.remove(&lru_key);
            }

            inner
                .cache
                .entry(block_num)
                .or_insert_with(|| CachedBlock::new(data, block_num));
        }

        // Refresh the LRU timestamp.
        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        if let Some(cached) = inner.cache.get_mut(&block_num) {
            cached.last_access = new_counter;
        }

        inner
            .cache
            .get(&block_num)
            .cloned()
            .ok_or(Ext4Error::corrupted())
    }

    /// Returns a mutable cached block, loading it from disk on demand.
    fn get_or_load_mut<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        let mut inner = self.inner.lock();

        if !inner.cache.contains_key(&block_num) {
            // Phase 1: snapshot LRU eviction info while holding the lock.
            let evict_info = if inner.cache.len() >= inner.max_entries {
                inner.snapshot_lru()
            } else {
                None
            };

            drop(inner);

            // Phase 2: do I/O without holding the spinlock.
            if let Some((_lru_key, Some(ref lru_data))) = evict_info {
                Self::write_block_static(block_dev, _lru_key, lru_data, self.block_size)?;
            }

            let data = self.load_block(block_dev, block_num)?;

            // Phase 3: reacquire the lock and apply the eviction + insertion.
            inner = self.inner.lock();

            if let Some((lru_key, _)) = evict_info {
                inner.cache.remove(&lru_key);
            }

            // Re-check after reacquiring: another thread may have inserted the
            // same key while we held no lock.
            inner
                .cache
                .entry(block_num)
                .or_insert_with(|| CachedBlock::new(data, block_num));
        }

        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        if let Some(cached) = inner.cache.get_mut(&block_num) {
            cached.last_access = new_counter;
        }
        Ok(())
    }

    /// Returns a cached block without loading from disk.
    pub fn get(&self, block_num: AbsoluteBN) -> Option<CachedBlock> {
        self.inner.lock().cache.get(&block_num).cloned()
    }

    /// Returns a mutable cached block without loading from disk.
    pub fn get_mut(&self, block_num: AbsoluteBN) -> Option<CachedBlock> {
        let mut inner = self.inner.lock();
        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        inner.cache.get_mut(&block_num).map(|cached| {
            cached.last_access = new_counter;
            cached.clone()
        })
    }

    /// Creates a brand-new cached block and marks it dirty.
    pub fn create_new<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<CachedBlock> {
        let mut inner = self.inner.lock();

        // Phase 1: snapshot eviction info while holding the lock.
        // Two evictions may be needed: (a) the same block number from a
        // previous incarnation, and (b) an LRU slot to stay within max_entries.
        let evict_existing = inner.snapshot_block_for_evict(block_num);
        let evict_lru_info = if inner.cache.len() >= inner.max_entries && evict_existing.is_none() {
            // Only evict LRU if we did not already free a slot above.
            inner.snapshot_lru()
        } else {
            None
        };

        drop(inner);

        // Phase 2: do I/O without holding the spinlock.
        if let Some(ref data) = evict_existing {
            Self::write_block_static(block_dev, block_num, data, self.block_size)?;
        }
        if let Some((lru_key, Some(ref lru_data))) = evict_lru_info {
            Self::write_block_static(block_dev, lru_key, lru_data, self.block_size)?;
        }

        // Phase 3: reacquire the lock and apply evictions + insertion.
        inner = self.inner.lock();

        if evict_existing.is_some() {
            inner.cache.remove(&block_num);
        }
        if let Some((lru_key, _)) = evict_lru_info {
            inner.cache.remove(&lru_key);
        }

        let data = alloc::vec![0u8; inner.block_size];
        let mut cached = CachedBlock::new(data, block_num);
        cached.dirty = true;

        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        cached.last_access = new_counter;

        inner.cache.insert(block_num, cached);
        inner
            .cache
            .get(&block_num)
            .cloned()
            .ok_or(Ext4Error::corrupted())
    }

    /// Marks a cached data block dirty.
    pub fn mark_dirty(&self, block_num: AbsoluteBN) {
        let mut inner = self.inner.lock();
        if let Some(cached) = inner.cache.get_mut(&block_num) {
            cached.mark_dirty();
        }
    }

    /// Modifies one cached block and marks it dirty.
    pub fn modify<B, F>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockDevice,
        F: FnOnce(&mut [u8]),
    {
        self.get_or_load_mut(block_dev, block_num)?;

        let mut inner = self.inner.lock();
        let cached = inner
            .cache
            .get_mut(&block_num)
            .ok_or(Ext4Error::corrupted())?;
        f(&mut cached.data);
        cached.mark_dirty();

        if !USE_MULTILEVEL_CACHE {
            let data = cached.data.clone();
            let blk = cached.block_num;
            drop(inner);
            Self::write_block_static(block_dev, blk, &data, self.block_size)?;

            inner = self.inner.lock();
            if let Some(cached) = inner.cache.get_mut(&block_num) {
                cached.dirty = false;
            }
        }
        Ok(())
    }

    /// Initializes a newly allocated data block through a closure.
    pub fn modify_new<B, F>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockDevice,
        F: FnOnce(&mut [u8]),
    {
        let _cached = self.create_new(block_dev, block_num)?;
        self.modify(block_dev, block_num, f)
    }

    /// Evicts one cached block.
    pub fn evict<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        self.inner.lock().do_evict(block_dev, block_num)
    }

    /// Flushes all dirty cached blocks to disk.
    pub fn flush_all<B: BlockDevice>(&self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        self.inner.lock().do_flush_all(block_dev)
    }

    /// Flushes one cached block to disk.
    pub fn flush<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        self.inner.lock().do_flush(block_dev, block_num)
    }

    /// Invalidate one cached block without flushing it.
    pub fn invalidate(&self, block_num: AbsoluteBN) {
        self.inner.lock().cache.remove(&block_num);
    }

    /// Clears the cache without flushing.
    pub fn clear(&self) {
        self.inner.lock().cache.clear();
    }

    /// Returns cache statistics.
    pub fn stats(&self) -> DataBlockCacheStats {
        let inner = self.inner.lock();
        let dirty_count = inner.cache.values().filter(|c| c.dirty).count();
        let total_size = inner.cache.len() * inner.block_size;

        DataBlockCacheStats {
            total_entries: inner.cache.len(),
            dirty_entries: dirty_count,
            max_entries: inner.max_entries,
            total_size_bytes: total_size,
        }
    }

    /// Writes one block to disk (static helper, takes runtime block_size).
    fn write_block_static<B: BlockDevice>(
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        data: &[u8],
        block_size: usize,
    ) -> Ext4Result<()> {
        let mut buf = alloc::vec![0u8; block_size];
        block_dev.read_blocks(&mut buf, block_num, 1)?;
        buf[..data.len()].copy_from_slice(data);
        block_dev.write_blocks(&buf, block_num, 1, false)?;
        Ok(())
    }
}

/// Data block cache statistics.
#[derive(Debug, Clone, Copy)]
pub struct DataBlockCacheStats {
    pub total_entries: usize,
    pub dirty_entries: usize,
    pub max_entries: usize,
    pub total_size_bytes: usize,
}

// ── Inner methods (caller holds `self.inner.lock()`) ─────────────────────────

impl DataBlockCacheInner {
    /// Snapshots the LRU data block for lock-free eviction.
    ///
    /// Returns `Some(lru_key, Some(data))` when dirty, `Some(lru_key, None)` when
    /// clean, `None` when the cache is empty.  The caller must do the I/O
    /// *without* holding the spinlock, then re-lock and remove `lru_key`.
    fn snapshot_lru(&self) -> Option<(AbsoluteBN, Option<Vec<u8>>)> {
        let lru_key = self
            .cache
            .iter()
            .min_by_key(|(_, cached)| cached.last_access)
            .map(|(key, _)| *key)?;

        let dirty_data = self.cache.get(&lru_key).and_then(|cached| {
            if cached.dirty {
                Some(cached.data.clone())
            } else {
                None
            }
        });

        Some((lru_key, dirty_data))
    }

    /// Snapshots a single block for lock-free eviction.
    ///
    /// Returns `Some(data)` when the block exists and is dirty, `None` when
    /// the block does not exist or is clean.
    fn snapshot_block_for_evict(&self, block_num: AbsoluteBN) -> Option<Vec<u8>> {
        self.cache.get(&block_num).and_then(|cached| {
            if cached.dirty {
                Some(cached.data.clone())
            } else {
                None
            }
        })
    }

    fn do_evict<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        if let Some(cached) = self.cache.remove(&block_num)
            && cached.dirty
        {
            DataBlockCache::write_block_static(
                block_dev,
                cached.block_num,
                &cached.data,
                self.block_size,
            )?;
        }
        Ok(())
    }

    fn do_flush<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        if let Some(cached) = self.cache.get(&block_num)
            && cached.dirty
        {
            let data = cached.data.clone();
            DataBlockCache::write_block_static(block_dev, block_num, &data, self.block_size)?;

            if let Some(cached) = self.cache.get_mut(&block_num) {
                cached.dirty = false;
            }
        }
        Ok(())
    }

    fn do_flush_all<B: BlockDevice>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        let mut dirty_blocks: Vec<(AbsoluteBN, Vec<u8>)> = self
            .cache
            .values()
            .filter(|cached| cached.dirty)
            .map(|cached| (cached.block_num, cached.data.clone()))
            .collect();

        if dirty_blocks.is_empty() {
            return Ok(());
        }

        dirty_blocks.sort_by_key(|(block_num, _)| *block_num);

        // Batch contiguous dirty blocks into one `write_blocks` call.
        let max_part_size = BLOCK_SIZE * 100;
        let block_size = self.block_size;
        let mut idx = 0usize;
        while idx < dirty_blocks.len() {
            let (start_block, _) = dirty_blocks[idx];
            let mut run_len = 1usize;

            while idx + run_len < dirty_blocks.len() && run_len <= max_part_size {
                let expected = start_block.checked_add_usize(run_len)?;
                if dirty_blocks[idx + run_len].0 == expected {
                    run_len += 1;
                } else {
                    break;
                }
            }

            let mut buf: Vec<u8> = Vec::with_capacity(block_size * run_len);
            for off in 0..run_len {
                buf.extend_from_slice(&dirty_blocks[idx + off].1);
            }

            let run_len_u32 =
                u32::try_from(run_len).map_err(|_| Ext4Error::from(Errno::EOVERFLOW))?;
            block_dev.write_blocks(&buf, start_block, run_len_u32, false)?;

            idx += run_len;
        }

        for cached in self.cache.values_mut() {
            cached.dirty = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disknode::Ext4Timestamp;

    struct TestBlockDevice {
        data: Vec<u8>,
    }

    impl TestBlockDevice {
        fn new(blocks: usize) -> Self {
            Self {
                data: alloc::vec![0; blocks * BLOCK_SIZE],
            }
        }
    }

    impl BlockDevice for TestBlockDevice {
        fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + buffer.len();
            buffer.copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + buffer.len();
            self.data[start..end].copy_from_slice(buffer);
            Ok(())
        }

        fn open(&mut self) -> Ext4Result<()> {
            Ok(())
        }

        fn close(&mut self) -> Ext4Result<()> {
            Ok(())
        }

        fn total_blocks(&self) -> u64 {
            (self.data.len() / BLOCK_SIZE) as u64
        }

        fn block_size(&self) -> u32 {
            BLOCK_SIZE as u32
        }

        fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
            Ok(Ext4Timestamp::new(0, 0))
        }
    }

    #[test]
    fn test_datablock_cache_basic() {
        let cache = DataBlockCache::new(8, BLOCK_SIZE);
        let stats = cache.stats();

        assert_eq!(stats.total_entries, 0);
        assert_eq!(stats.max_entries, 8);
        assert_eq!(stats.total_size_bytes, 0);
    }

    #[test]
    fn test_create_new_block() {
        let cache = DataBlockCache::new(8, BLOCK_SIZE);

        let device = TestBlockDevice::new(1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);

        let block = cache
            .create_new(&mut jbd2_dev, AbsoluteBN::new(100))
            .expect("create new block");
        assert_eq!(block.block_num, AbsoluteBN::new(100));
        assert_eq!(block.data.len(), BLOCK_SIZE);
        assert!(block.dirty);
    }
}
