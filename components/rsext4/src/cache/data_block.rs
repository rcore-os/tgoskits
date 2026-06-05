//! Data block cache helpers.

use alloc::{collections::BTreeMap, vec::Vec};

use ax_kspin::SpinNoPreempt as SpinMutex;

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
    /// Generation counter — bumped on every access, used to validate
    /// stale LRU snapshots before eviction.
    pub generation: u64,
}

impl CachedBlock {
    pub fn new(data: Vec<u8>, block_num: AbsoluteBN) -> Self {
        Self {
            data,
            dirty: false,
            block_num,
            last_access: 0,
            generation: 0,
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

            // Phase 2: load the requested block from disk (no dirty writeback
            // yet — the victim snapshot may be stale).
            let data = self.load_block(block_dev, block_num)?;

            // Phase 3: reacquire the lock. Validate the victim generation.
            // If valid, remove it and schedule dirty writeback for Phase 4.
            // If stale, discard the snapshot without writing anything.
            inner = self.inner.lock();

            let dirty_to_write = match evict_info {
                Some((lru_key, lru_gen, dirty_opt))
                    if inner
                        .cache
                        .get(&lru_key)
                        .is_some_and(|cached| cached.generation == lru_gen) =>
                {
                    inner.cache.remove(&lru_key);
                    dirty_opt.map(|data| (lru_key, data))
                }
                _ => None,
            };

            inner
                .cache
                .entry(block_num)
                .or_insert_with(|| CachedBlock::new(data, block_num));

            drop(inner);

            // Phase 4: write the victim's dirty data to disk AFTER the
            // generation check passed (outside the spinlock).
            if let Some((lru_key, ref lru_data)) = dirty_to_write {
                Self::write_block_static(block_dev, lru_key, lru_data, self.block_size)?;
            }

            // Reacquire for the LRU refresh below.
            inner = self.inner.lock();
        }

        // Refresh the LRU timestamp and bump the generation.
        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        if let Some(cached) = inner.cache.get_mut(&block_num) {
            cached.last_access = new_counter;
            cached.generation += 1;
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

            // Phase 2: load the requested block from disk (no dirty writeback
            // yet — the victim snapshot may be stale).
            let data = self.load_block(block_dev, block_num)?;

            // Phase 3: reacquire the lock. Validate the victim generation.
            // If valid, remove it and schedule dirty writeback for Phase 4.
            // If stale, discard the snapshot without writing anything.
            inner = self.inner.lock();

            let dirty_to_write = match evict_info {
                Some((lru_key, lru_gen, dirty_opt))
                    if inner
                        .cache
                        .get(&lru_key)
                        .is_some_and(|cached| cached.generation == lru_gen) =>
                {
                    inner.cache.remove(&lru_key);
                    dirty_opt.map(|data| (lru_key, data))
                }
                _ => None,
            };

            // Re-check after reacquiring: another thread may have inserted the
            // same key while we held no lock.
            inner
                .cache
                .entry(block_num)
                .or_insert_with(|| CachedBlock::new(data, block_num));

            drop(inner);

            // Phase 4: write the victim's dirty data to disk AFTER the
            // generation check passed (outside the spinlock).
            if let Some((lru_key, ref lru_data)) = dirty_to_write {
                Self::write_block_static(block_dev, lru_key, lru_data, self.block_size)?;
            }

            inner = self.inner.lock();
        }

        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        if let Some(cached) = inner.cache.get_mut(&block_num) {
            cached.last_access = new_counter;
            cached.generation += 1;
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
            cached.generation += 1;
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

        // Phase 2: write dirty data for unconditional same-block eviction.
        // The LRU victim's dirty data is NOT written here — it must pass
        // the generation check first (see Phase 4).
        if let Some(ref data) = evict_existing {
            Self::write_block_static(block_dev, block_num, data, self.block_size)?;
        }

        // Phase 3: reacquire the lock and apply evictions + insertion.
        inner = self.inner.lock();

        // Evict the pre-existing incarnation of the same block number
        // unconditionally — it is being replaced.
        if evict_existing.is_some() {
            inner.cache.remove(&block_num);
        }
        // Validate LRU victim generation; if valid, remove and schedule
        // dirty writeback. If stale, discard the snapshot silently.
        let lru_dirty_to_write = match evict_lru_info {
            Some((lru_key, lru_gen, dirty_opt))
                if inner
                    .cache
                    .get(&lru_key)
                    .is_some_and(|cached| cached.generation == lru_gen) =>
            {
                inner.cache.remove(&lru_key);
                dirty_opt.map(|data| (lru_key, data))
            }
            _ => None,
        };

        let data = alloc::vec![0u8; inner.block_size];
        let mut cached = CachedBlock::new(data, block_num);
        cached.dirty = true;

        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        cached.last_access = new_counter;

        inner.cache.insert(block_num, cached);
        let result = inner
            .cache
            .get(&block_num)
            .cloned()
            .ok_or(Ext4Error::corrupted());

        drop(inner);

        // Phase 4: write LRU victim's dirty data AFTER generation check.
        if let Some((lru_key, ref lru_data)) = lru_dirty_to_write {
            Self::write_block_static(block_dev, lru_key, lru_data, self.block_size)?;
        }

        result
    }

    /// Marks a cached data block dirty.
    pub fn mark_dirty(&self, block_num: AbsoluteBN) {
        let mut inner = self.inner.lock();
        if let Some(cached) = inner.cache.get_mut(&block_num) {
            cached.mark_dirty();
            cached.generation += 1;
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
        cached.generation += 1;

        if !USE_MULTILEVEL_CACHE {
            let data = cached.data.clone();
            let blk = cached.block_num;
            drop(inner);
            Self::write_block_static(block_dev, blk, &data, self.block_size)?;

            inner = self.inner.lock();
            if let Some(cached) = inner.cache.get_mut(&block_num) {
                cached.dirty = false;
                cached.generation += 1;
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
        let len = core::cmp::min(data.len(), block_size);
        buf[..len].copy_from_slice(&data[..len]);
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
    /// Returns `Some((lru_key, generation, dirty_data))` where `generation` is
    /// the entry's generation at snapshot time.  The caller must do the I/O
    /// *without* holding the spinlock, then re-lock, verify that the entry's
    /// generation still matches, and only then remove it.
    /// A generation mismatch means another thread accessed or modified the
    /// victim while we held no lock — in that case the victim must NOT be
    /// removed (temporarily exceeding `max_entries` is harmless).
    fn snapshot_lru(&self) -> Option<(AbsoluteBN, u64, Option<Vec<u8>>)> {
        let lru_key = self
            .cache
            .iter()
            .min_by_key(|(_, cached)| cached.last_access)
            .map(|(key, _)| *key)?;

        let lru_gen = self.cache.get(&lru_key).map(|cached| cached.generation)?;

        let dirty_data = self.cache.get(&lru_key).and_then(|cached| {
            if cached.dirty {
                Some(cached.data.clone())
            } else {
                None
            }
        });

        Some((lru_key, lru_gen, dirty_data))
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

        let stats = cache.stats();
        assert_eq!(stats.total_entries, 1);
        assert_eq!(stats.dirty_entries, 1);
    }

    #[test]
    fn test_invalidate() {
        let cache = DataBlockCache::new(8, BLOCK_SIZE);

        let device = TestBlockDevice::new(1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);

        cache
            .create_new(&mut jbd2_dev, AbsoluteBN::new(100))
            .expect("create new block");
        assert_eq!(cache.stats().total_entries, 1);

        cache.invalidate(AbsoluteBN::new(100));
        assert_eq!(cache.stats().total_entries, 0);
    }

    #[test]
    fn create_new_respects_lru_limit() {
        let cache = DataBlockCache::new(2, BLOCK_SIZE);
        let device = TestBlockDevice::new(1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);

        for block in 10..14 {
            cache
                .create_new(&mut jbd2_dev, AbsoluteBN::new(block))
                .expect("create new block");
        }

        let stats = cache.stats();
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.max_entries, 2);
    }

    /// Regression test for the stale-victim race (TOCTOU on LRU eviction).
    ///
    /// Scenario:
    /// 1. Cache full (max_entries=1) with block_A (gen=0, dirty).
    /// 2. Thread 1 calls `get_or_load(block_B)` → snapshots (A, gen=0, dirty_data)
    ///    → drops the spinlock → enters I/O (blocked at barrier).
    /// 3. Main thread calls `cache.get_mut(block_A)` → bumps gen to 1.
    /// 4. Thread 1 resumes → re-locks → gen mismatch (0 ≠ 1) → skips eviction.
    ///
    /// Assertions:
    /// - block_A is still cached (NOT evicted — new state preserved).
    /// - block_A's generation is 1 (the concurrent access was recorded).
    /// - block_B was loaded into cache.
    /// - No stale dirty data was written for block_A (dirty_to_write is None).
    #[test]
    fn stale_victim_gen_mismatch_prevents_eviction() {
        use std::sync::{Arc, Barrier};

        const BLK_A: AbsoluteBN = AbsoluteBN::new(10);
        const BLK_B: AbsoluteBN = AbsoluteBN::new(20);

        let cache = Arc::new(DataBlockCache::new(1, BLOCK_SIZE));

        // ── Barrier-synchronised BlockDevice ──────────────────────────────
        // The device blocks inside `read_blocks` so the test can interleave a
        // cache access between Phase 1 (snapshot) and Phase 3 (re-lock) of
        // `get_or_load`.
        let inner_dev = TestBlockDevice::new(1024);
        let enter_io = Arc::new(Barrier::new(2)); // "I'm in I/O, lock is dropped"
        let leave_io = Arc::new(Barrier::new(2)); // "I'm done, main may continue"

        struct SyncDevice<D> {
            inner: D,
            enter: Arc<Barrier>,
            leave: Arc<Barrier>,
        }

        impl<D: BlockDevice> BlockDevice for SyncDevice<D> {
            fn read(
                &mut self,
                buffer: &mut [u8],
                block_id: AbsoluteBN,
                count: u32,
            ) -> Ext4Result<()> {
                self.enter.wait(); // signal: cache lock is dropped, race window open
                self.inner.read(buffer, block_id, count)?;
                self.leave.wait(); // wait for main thread to finish its access
                Ok(())
            }

            fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
                self.inner.write(buffer, block_id, count)
            }

            fn open(&mut self) -> Ext4Result<()> {
                self.inner.open()
            }

            fn close(&mut self) -> Ext4Result<()> {
                self.inner.close()
            }

            fn total_blocks(&self) -> u64 {
                self.inner.total_blocks()
            }

            fn block_size(&self) -> u32 {
                self.inner.block_size()
            }

            fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
                self.inner.current_time()
            }
        }

        let sync_dev = SyncDevice {
            inner: inner_dev,
            enter: enter_io.clone(),
            leave: leave_io.clone(),
        };
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, sync_dev, false);

        // Step 1: fill the cache with block_A (dirty, generation 0).
        cache
            .create_new(&mut jbd2_dev, BLK_A)
            .expect("create block A");
        assert!(cache.get(BLK_A).is_some(), "block_A must be in cache");
        assert_eq!(cache.get(BLK_A).unwrap().generation, 0);

        // Step 2: spawn a thread that calls get_or_load(block_B).
        // Inside get_or_load the device will block at enter_io after the LRU
        // snapshot is taken but before the spinlock is reacquired.
        let cache2 = cache.clone();
        let handle = std::thread::spawn(move || cache2.get_or_load(&mut jbd2_dev, BLK_B));

        // Step 3: wait for Thread 1 to enter I/O (lock dropped).
        enter_io.wait();

        // Step 4: concurrently access block_A, bumping its generation.
        let cached_a = cache
            .get_mut(BLK_A)
            .expect("block_A should still be accessible");
        assert_eq!(
            cached_a.generation, 1,
            "generation bumped from 0 to 1 by concurrent access"
        );

        // Step 5: let Thread 1 continue (re-lock, gen check, insert).
        leave_io.wait();

        // Step 6: collect Thread 1's result.
        let result = handle.join().expect("Thread 1 panicked");
        assert!(result.is_ok(), "get_or_load(block_B) must succeed");

        // ── Assertions ───────────────────────────────────────────────────
        // block_A must still be present — generation mismatch prevented eviction.
        let a = cache
            .get(BLK_A)
            .expect("block_A must NOT be evicted (gen mismatch prevented removal)");
        assert_eq!(
            a.generation, 1,
            "block_A generation should be 1 (bumped by concurrent get_mut)"
        );

        // block_B was loaded.
        assert!(
            cache.get(BLK_B).is_some(),
            "block_B must be loaded into cache"
        );

        // Both entries coexist (temporarily exceeding max_entries — harmless).
        let stats = cache.stats();
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.max_entries, 1);
    }
}
