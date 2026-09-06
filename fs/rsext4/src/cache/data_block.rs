//! Data block cache helpers.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

use crate::{blockdev::*, bmalloc::AbsoluteBN, config::USE_MULTILEVEL_CACHE, error::*};

/// Cache key for one physical data block.
pub type BlockCacheKey = AbsoluteBN;

#[derive(Debug, Clone, Copy)]
struct DirtyBlock {
    block_num: AbsoluteBN,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WritebackCompletion {
    MarkClean,
    RetainDirty,
}

/// Cached data block.
#[derive(Debug, Clone)]
pub struct CachedBlock {
    /// Block contents.
    pub data: Arc<Vec<u8>>,
    /// Whether the cache entry is dirty.
    pub dirty: bool,
    /// Physical block number.
    pub block_num: AbsoluteBN,
    /// Access timestamp used for LRU eviction.
    pub last_access: u64,
    /// Generation counter bumped whenever the cached state changes.
    pub generation: u64,
}

impl CachedBlock {
    pub fn new(data: Vec<u8>, block_num: AbsoluteBN) -> Self {
        Self {
            data: Arc::new(data),
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

/// Data block cache owned exclusively by one mounted filesystem.
///
/// The portable core deliberately provides no internal synchronization. The
/// OS adapter owns the filesystem lock and every cache mutation is visible
/// through an exclusive borrow.
#[derive(Clone)]
pub struct DataBlockCache {
    cache: BTreeMap<BlockCacheKey, CachedBlock>,
    /// Unique block numbers ordered from least to most recently used.
    lru_order: Vec<AbsoluteBN>,
    max_entries: usize,
    access_counter: u64,
    block_size: usize,
}

impl DataBlockCache {
    /// Creates a data block cache.
    pub fn new(max_entries: usize, block_size: usize) -> Self {
        Self {
            cache: BTreeMap::new(),
            lru_order: Vec::with_capacity(max_entries),
            max_entries,
            access_counter: 0,
            block_size,
        }
    }

    /// Loads one block from disk using a caller-provided buffer.
    fn load_block<B: BlockIo>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<Vec<u8>> {
        let mut buf = alloc::vec![0u8; self.block_size];
        block_dev.read_blocks(&mut buf, block_num, 1)?;
        Ok(buf)
    }

    /// Returns a cached block, loading it from disk on demand.
    pub fn get_or_load<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<CachedBlock> {
        self.ensure_loaded(block_dev, block_num)?;
        self.touch(block_num);
        self.cache
            .get(&block_num)
            .cloned()
            .ok_or(Ext4Error::corrupted())
    }

    fn ensure_loaded<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        if self.cache.contains_key(&block_num) {
            return Ok(());
        }

        // Finish the potentially failing read before changing cache contents.
        let data = self.load_block(block_dev, block_num)?;
        self.evict_lru_if_full(block_dev)?;
        self.cache
            .insert(block_num, CachedBlock::new(data, block_num));
        self.lru_order.push(block_num);
        Ok(())
    }

    fn evict_lru_if_full<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        if self.cache.len() < self.max_entries {
            return Ok(());
        }

        if self.lru_order.is_empty() {
            return Ok(());
        }

        // Reclaim one quarter of the cache at a time. Small, one-entry caches
        // retain the old behavior, while the normal 128-entry cache can merge
        // physically contiguous dirty blocks into a bounded device request.
        let victim_count = self.max_entries.div_ceil(4).max(1);
        let victim_count = core::cmp::min(victim_count, self.lru_order.len());
        let mut victims = Vec::new();
        victims
            .try_reserve_exact(victim_count)
            .map_err(|_| Ext4Error::no_memory())?;
        victims.extend_from_slice(&self.lru_order[..victim_count]);
        let dirty_blocks = self.dirty_blocks_for_keys(&victims)?;

        // Keep every selected entry dirty and resident unless all writeback
        // requests succeed. Retrying a partially persisted batch is safe.
        self.write_dirty_runs(block_dev, &dirty_blocks, WritebackCompletion::RetainDirty)?;
        for block_num in &victims {
            self.cache.remove(block_num);
        }
        self.lru_order.drain(..victim_count);
        Ok(())
    }

    fn touch(&mut self, block_num: AbsoluteBN) {
        self.access_counter = self.access_counter.saturating_add(1);
        if let Some(cached) = self.cache.get_mut(&block_num) {
            cached.last_access = self.access_counter;
            cached.generation = cached.generation.saturating_add(1);
            self.record_mru(block_num);
        }
    }

    fn record_mru(&mut self, block_num: AbsoluteBN) {
        if let Some(index) = self
            .lru_order
            .iter()
            .position(|cached_num| *cached_num == block_num)
        {
            self.lru_order.remove(index);
        }
        self.lru_order.push(block_num);
    }

    fn remove_from_lru(&mut self, block_num: AbsoluteBN) {
        if let Some(index) = self
            .lru_order
            .iter()
            .position(|cached_num| *cached_num == block_num)
        {
            self.lru_order.remove(index);
        }
    }

    fn write_back_if_dirty<B: BlockIo>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        let Some(cached) = self.cache.get(&block_num) else {
            return Ok(());
        };
        if cached.dirty {
            Self::write_block_static(block_dev, block_num, &cached.data, self.block_size, false)?;
        }
        Ok(())
    }

    /// Returns a cached block without loading from disk.
    pub fn get(&self, block_num: AbsoluteBN) -> Option<CachedBlock> {
        self.cache.get(&block_num).cloned()
    }

    /// Returns a cached block and records mutable access to it.
    pub fn get_mut(&mut self, block_num: AbsoluteBN) -> Option<CachedBlock> {
        self.touch(block_num);
        self.cache.get(&block_num).cloned()
    }

    /// Creates a brand-new cached block and marks it dirty.
    pub fn create_new<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<CachedBlock> {
        if self.cache.contains_key(&block_num) {
            // A failed replacement writeback leaves the old incarnation intact.
            self.write_back_if_dirty(block_dev, block_num)?;
            self.cache.remove(&block_num);
            self.remove_from_lru(block_num);
        } else {
            self.evict_lru_if_full(block_dev)?;
        }

        let mut cached = CachedBlock::new(alloc::vec![0u8; self.block_size], block_num);
        cached.dirty = true;
        self.access_counter = self.access_counter.saturating_add(1);
        cached.last_access = self.access_counter;
        self.cache.insert(block_num, cached);
        self.lru_order.push(block_num);
        self.cache
            .get(&block_num)
            .cloned()
            .ok_or(Ext4Error::corrupted())
    }

    /// Marks a cached data block dirty.
    pub fn mark_dirty(&mut self, block_num: AbsoluteBN) {
        if let Some(cached) = self.cache.get_mut(&block_num) {
            cached.mark_dirty();
            cached.generation = cached.generation.saturating_add(1);
        }
    }

    /// Modifies one cached block and marks it dirty.
    pub fn modify<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockIo,
        F: FnOnce(&mut [u8]),
    {
        self.modify_with_kind(block_dev, block_num, false, f)
    }

    /// Modifies one filesystem metadata block and routes write-through through
    /// the active JBD2/direct metadata handle.
    pub(crate) fn modify_metadata<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockIo,
        F: FnOnce(&mut [u8]),
    {
        self.modify_with_kind(block_dev, block_num, true, f)
    }

    fn modify_with_kind<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        is_metadata: bool,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockIo,
        F: FnOnce(&mut [u8]),
    {
        self.ensure_loaded(block_dev, block_num)?;
        self.touch(block_num);

        let cached = self
            .cache
            .get_mut(&block_num)
            .ok_or(Ext4Error::corrupted())?;
        f(Arc::make_mut(&mut cached.data).as_mut_slice());
        cached.mark_dirty();
        cached.generation = cached.generation.saturating_add(1);

        if !USE_MULTILEVEL_CACHE {
            let data = cached.data.clone();
            Self::write_block_static(block_dev, block_num, &data, self.block_size, is_metadata)?;
            let cached = self
                .cache
                .get_mut(&block_num)
                .ok_or(Ext4Error::corrupted())?;
            cached.dirty = false;
            cached.generation = cached.generation.saturating_add(1);
        }
        Ok(())
    }

    /// Initializes a newly allocated data block through a closure.
    pub fn modify_new<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockIo,
        F: FnOnce(&mut [u8]),
    {
        self.create_new(block_dev, block_num)?;
        self.modify(block_dev, block_num, f)
    }

    /// Initializes a newly allocated filesystem metadata block.
    pub(crate) fn modify_new_metadata<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockIo,
        F: FnOnce(&mut [u8]),
    {
        self.create_new(block_dev, block_num)?;
        self.modify_metadata(block_dev, block_num, f)
    }

    /// Writes a contiguous initialized data-block run directly and refreshes
    /// any cached entries that overlap the run.
    pub fn write_run<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        start_block: AbsoluteBN,
        count: u32,
        data: &[u8],
    ) -> Ext4Result<()> {
        if count == 0 {
            return Ok(());
        }

        let required = self
            .block_size
            .checked_mul(count as usize)
            .ok_or_else(Ext4Error::overflow)?;
        if data.len() < required {
            return Err(Ext4Error::buffer_too_small(data.len(), required));
        }

        block_dev.write_blocks(&data[..required], start_block, count, false)?;

        for off in 0..count {
            let block_num = start_block.checked_add(off)?;
            if self.cache.contains_key(&block_num) {
                let start = off as usize * self.block_size;
                self.access_counter = self.access_counter.saturating_add(1);
                let cached = self
                    .cache
                    .get_mut(&block_num)
                    .ok_or(Ext4Error::corrupted())?;
                Arc::make_mut(&mut cached.data)
                    .copy_from_slice(&data[start..start + self.block_size]);
                cached.dirty = false;
                cached.last_access = self.access_counter;
                cached.generation = cached.generation.saturating_add(1);
            }
        }
        Ok(())
    }

    /// Reads a contiguous initialized data-block run and overlays any cached
    /// entries that overlap the run. Dirty cache entries therefore remain the
    /// source of truth even when the disk still contains older data.
    pub fn read_run<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        start_block: AbsoluteBN,
        count: u32,
        dst: &mut [u8],
    ) -> Ext4Result<()> {
        if count == 0 {
            return Ok(());
        }

        let required = self
            .block_size
            .checked_mul(count as usize)
            .ok_or_else(Ext4Error::overflow)?;
        if dst.len() < required {
            return Err(Ext4Error::buffer_too_small(dst.len(), required));
        }

        block_dev.read_blocks(&mut dst[..required], start_block, count)?;

        for off in 0..count {
            let block_num = start_block.checked_add(off)?;
            if self.cache.contains_key(&block_num) {
                let start = off as usize * self.block_size;
                self.touch(block_num);
                let cached = self.cache.get(&block_num).ok_or(Ext4Error::corrupted())?;
                dst[start..start + self.block_size].copy_from_slice(&cached.data);
            }
        }
        Ok(())
    }

    /// Evicts one cached block. A failed dirty writeback keeps the entry dirty
    /// and resident so the caller can retry without losing data.
    pub fn evict<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        self.write_back_if_dirty(block_dev, block_num)?;
        self.cache.remove(&block_num);
        self.remove_from_lru(block_num);
        Ok(())
    }

    /// Flushes all dirty cached blocks to disk.
    pub fn flush_all<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        let dirty_blocks = self.dirty_blocks_for_flush()?;
        if dirty_blocks.is_empty() {
            return Ok(());
        }

        self.write_dirty_runs(block_dev, &dirty_blocks, WritebackCompletion::MarkClean)
    }

    /// Flushes one cached block to disk.
    pub fn flush<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        let Some(cached) = self.cache.get(&block_num) else {
            return Ok(());
        };
        if !cached.dirty {
            return Ok(());
        }

        let generation = cached.generation;
        let data = cached.data.clone();
        Self::write_block_static(block_dev, block_num, &data, self.block_size, false)?;
        if let Some(cached) = self.cache.get_mut(&block_num)
            && cached.generation == generation
        {
            cached.dirty = false;
            cached.generation = cached.generation.saturating_add(1);
        }
        Ok(())
    }

    /// Flushes one cached filesystem metadata block through the journal owner.
    pub(crate) fn flush_metadata<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        let Some(cached) = self.cache.get(&block_num) else {
            return Ok(());
        };
        if !cached.dirty {
            return Ok(());
        }

        let generation = cached.generation;
        let data = cached.data.clone();
        Self::write_block_static(block_dev, block_num, &data, self.block_size, true)?;
        if let Some(cached) = self.cache.get_mut(&block_num)
            && cached.generation == generation
        {
            cached.dirty = false;
            cached.generation = cached.generation.saturating_add(1);
        }
        Ok(())
    }

    /// Invalidates one cached block without flushing it.
    pub fn invalidate(&mut self, block_num: AbsoluteBN) {
        self.cache.remove(&block_num);
        self.remove_from_lru(block_num);
    }

    /// Clears the cache without flushing.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.lru_order.clear();
    }

    /// Returns cache statistics.
    pub fn stats(&self) -> DataBlockCacheStats {
        let dirty_count = self.cache.values().filter(|cached| cached.dirty).count();
        DataBlockCacheStats {
            total_entries: self.cache.len(),
            dirty_entries: dirty_count,
            max_entries: self.max_entries,
            total_size_bytes: self.cache.len() * self.block_size,
        }
    }

    fn dirty_blocks_for_flush(&self) -> Ext4Result<Vec<DirtyBlock>> {
        let dirty_count = self.cache.values().filter(|cached| cached.dirty).count();
        let mut dirty_blocks = Vec::new();
        dirty_blocks
            .try_reserve_exact(dirty_count)
            .map_err(|_| Ext4Error::no_memory())?;
        dirty_blocks.extend(self.cache.iter().filter_map(|(&block_num, cached)| {
            cached.dirty.then_some(DirtyBlock {
                block_num,
                generation: cached.generation,
            })
        }));
        Ok(dirty_blocks)
    }

    fn dirty_blocks_for_keys(&self, block_nums: &[AbsoluteBN]) -> Ext4Result<Vec<DirtyBlock>> {
        let mut dirty_blocks = Vec::new();
        dirty_blocks
            .try_reserve_exact(block_nums.len())
            .map_err(|_| Ext4Error::no_memory())?;
        dirty_blocks.extend(block_nums.iter().filter_map(|block_num| {
            self.cache.get(block_num).and_then(|cached| {
                cached.dirty.then_some(DirtyBlock {
                    block_num: *block_num,
                    generation: cached.generation,
                })
            })
        }));
        dirty_blocks.sort_by_key(|dirty| dirty.block_num);
        Ok(dirty_blocks)
    }

    /// Writes one block to disk.
    fn write_block_static<B: BlockIo>(
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        data: &[u8],
        block_size: usize,
        is_metadata: bool,
    ) -> Ext4Result<()> {
        let data = data
            .get(..block_size)
            .ok_or_else(|| Ext4Error::buffer_too_small(data.len(), block_size))?;
        block_dev.write_blocks(data, block_num, 1, is_metadata)?;
        Ok(())
    }

    fn write_dirty_runs<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        dirty_blocks: &[DirtyBlock],
        completion: WritebackCompletion,
    ) -> Ext4Result<()> {
        if dirty_blocks.is_empty() {
            return Ok(());
        }

        let buffered_blocks = core::cmp::min(dirty_blocks.len(), MAX_BUFFERED_WRITE_BLOCKS);
        let buffer_bytes = self
            .block_size
            .checked_mul(buffered_blocks)
            .ok_or_else(Ext4Error::overflow)?;
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(buffer_bytes)
            .map_err(|_| Ext4Error::no_memory())?;

        let mut idx = 0usize;
        while idx < dirty_blocks.len() {
            let start_block = dirty_blocks[idx].block_num;
            let mut run_len = 1usize;

            while idx + run_len < dirty_blocks.len() && run_len < MAX_BUFFERED_WRITE_BLOCKS {
                let expected = start_block.checked_add_usize(run_len)?;
                if dirty_blocks[idx + run_len].block_num != expected {
                    break;
                }
                run_len += 1;
            }

            buffer.clear();
            for dirty in &dirty_blocks[idx..idx + run_len] {
                let cached = self.cache.get(&dirty.block_num).ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("data_cache:missing_dirty_block")
                })?;
                if !cached.dirty || cached.generation != dirty.generation {
                    return Err(
                        Ext4Error::corrupted().with_operation("data_cache:stale_dirty_snapshot")
                    );
                }
                let data = cached.data.get(..self.block_size).ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("data_cache:short_dirty_block")
                })?;
                buffer.extend_from_slice(data);
            }

            let run_len_u32 = u32::try_from(run_len).map_err(|_| Ext4Error::overflow())?;
            block_dev.write_blocks(&buffer, start_block, run_len_u32, false)?;
            if completion == WritebackCompletion::MarkClean {
                for dirty in &dirty_blocks[idx..idx + run_len] {
                    let cached = self.cache.get_mut(&dirty.block_num).ok_or_else(|| {
                        Ext4Error::corrupted().with_operation("data_cache:lost_written_block")
                    })?;
                    if !cached.dirty || cached.generation != dirty.generation {
                        return Err(Ext4Error::corrupted()
                            .with_operation("data_cache:changed_written_block"));
                    }
                    cached.dirty = false;
                    cached.generation = cached.generation.saturating_add(1);
                }
            }
            idx += run_len;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::BLOCK_SIZE, disknode::Ext4Timestamp};

    struct TestBlockDevice {
        data: Vec<u8>,
        fail_writes: bool,
        fail_write_number: Option<usize>,
        write_attempts: usize,
        read_calls: Vec<(u64, u32)>,
        write_calls: Vec<(u64, u32)>,
    }

    impl TestBlockDevice {
        fn new(blocks: usize) -> Self {
            Self {
                data: alloc::vec![0; blocks * BLOCK_SIZE],
                fail_writes: false,
                fail_write_number: None,
                write_attempts: 0,
                read_calls: Vec::new(),
                write_calls: Vec::new(),
            }
        }

        fn failing_writes(blocks: usize) -> Self {
            Self {
                data: alloc::vec![0; blocks * BLOCK_SIZE],
                fail_writes: true,
                fail_write_number: None,
                write_attempts: 0,
                read_calls: Vec::new(),
                write_calls: Vec::new(),
            }
        }

        #[cfg(feature = "USE_MULTILEVEL_CACHE")]
        fn failing_write_number(blocks: usize, fail_write_number: usize) -> Self {
            Self {
                data: alloc::vec![0; blocks * BLOCK_SIZE],
                fail_writes: false,
                fail_write_number: Some(fail_write_number),
                write_attempts: 0,
                read_calls: Vec::new(),
                write_calls: Vec::new(),
            }
        }
    }

    impl BlockIo for TestBlockDevice {
        fn read(
            &mut self,
            buffer: &mut [u8],
            block_id: crate::io::SectorId,
            count: u32,
        ) -> Ext4Result<()> {
            self.read_calls.push((block_id.raw(), count));
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + buffer.len();
            buffer.copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn write(
            &mut self,
            buffer: &[u8],
            block_id: crate::io::SectorId,
            count: u32,
        ) -> Ext4Result<()> {
            self.write_attempts += 1;
            if self.fail_writes || self.fail_write_number == Some(self.write_attempts) {
                return Err(Ext4Error::io());
            }
            self.write_calls.push((block_id.raw(), count));
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + buffer.len();
            self.data[start..end].copy_from_slice(buffer);
            Ok(())
        }

        fn geometry(&self) -> crate::io::DeviceGeometry {
            crate::io::DeviceGeometry::new(BLOCK_SIZE as u32, {
                (self.data.len() / BLOCK_SIZE) as u64
            })
        }

        fn capabilities(&self) -> crate::io::DeviceCapabilities {
            crate::io::DeviceCapabilities {
                read_only: { false },

                flush: true,

                ..crate::io::DeviceCapabilities::default()
            }
        }

        fn flush(&mut self) -> crate::Ext4Result<()> {
            Ok(())
        }
    }

    impl crate::runtime::Clock for TestBlockDevice {
        fn now(&self) -> Ext4Result<Ext4Timestamp> {
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
        let mut cache = DataBlockCache::new(8, BLOCK_SIZE);
        let device = TestBlockDevice::new(1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);

        let block = cache
            .create_new(&mut jbd2_dev, AbsoluteBN::new(100))
            .expect("create new block");
        assert_eq!(block.block_num, AbsoluteBN::new(100));
        assert_eq!(block.data.len(), BLOCK_SIZE);
        assert!(block.dirty);
        assert_eq!(cache.stats().total_entries, 1);
        assert_eq!(cache.stats().dirty_entries, 1);
    }

    #[test]
    fn test_invalidate() {
        let mut cache = DataBlockCache::new(8, BLOCK_SIZE);
        let device = TestBlockDevice::new(1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);

        cache
            .create_new(&mut jbd2_dev, AbsoluteBN::new(100))
            .expect("create new block");
        cache.invalidate(AbsoluteBN::new(100));
        assert_eq!(cache.stats().total_entries, 0);
    }

    #[test]
    fn read_run_overlays_dirty_cached_blocks() {
        let mut cache = DataBlockCache::new(8, BLOCK_SIZE);
        let mut device = TestBlockDevice::new(1024);
        let disk_start = AbsoluteBN::new(100).as_usize().unwrap() * BLOCK_SIZE;
        device.data[disk_start..disk_start + BLOCK_SIZE * 2].fill(0xaa);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);

        cache
            .modify_new(&mut jbd2_dev, AbsoluteBN::new(101), |data| {
                data.fill(0xbb);
            })
            .expect("create dirty cached block");

        let mut dst = alloc::vec![0; BLOCK_SIZE * 2];
        cache
            .read_run(&mut jbd2_dev, AbsoluteBN::new(100), 2, &mut dst)
            .expect("read contiguous run");

        assert_eq!(dst[..BLOCK_SIZE], [0xaa; BLOCK_SIZE]);
        assert_eq!(dst[BLOCK_SIZE..], [0xbb; BLOCK_SIZE]);
        assert_eq!(
            cache.stats().dirty_entries,
            usize::from(USE_MULTILEVEL_CACHE)
        );

        if !USE_MULTILEVEL_CACHE {
            cache.clear();
            let mut persisted = alloc::vec![0; BLOCK_SIZE];
            cache
                .read_run(&mut jbd2_dev, AbsoluteBN::new(101), 1, &mut persisted)
                .expect("write-through data remains after cache clear");
            assert_eq!(persisted, [0xbb; BLOCK_SIZE]);
        }
    }

    #[test]
    fn create_new_respects_lru_limit() {
        let mut cache = DataBlockCache::new(2, BLOCK_SIZE);
        let device = TestBlockDevice::new(1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);

        for block in 10..14 {
            cache
                .create_new(&mut jbd2_dev, AbsoluteBN::new(block))
                .expect("create new block");
        }

        assert_eq!(cache.stats().total_entries, 2);
        assert_eq!(cache.stats().max_entries, 2);
    }

    #[cfg(feature = "USE_MULTILEVEL_CACHE")]
    #[test]
    fn dirty_lru_eviction_batches_contiguous_blocks() {
        let mut cache = DataBlockCache::new(8, BLOCK_SIZE);
        let device = TestBlockDevice::new(1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);

        for block in 10..18 {
            cache
                .modify_new(&mut jbd2_dev, AbsoluteBN::new(block), |data| {
                    data.fill(block as u8);
                })
                .expect("fill dirty cache");
        }
        cache
            .modify_new(&mut jbd2_dev, AbsoluteBN::new(18), |data| data.fill(18))
            .expect("batch dirty eviction");

        assert_eq!(cache.stats().total_entries, 7);
        let device = jbd2_dev.into_inner();
        assert_eq!(device.write_calls, [(10, 2)]);
    }

    #[test]
    fn full_cached_block_writeback_does_not_reread_home_block() {
        let mut cache = DataBlockCache::new(8, BLOCK_SIZE);
        let device = TestBlockDevice::new(1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);

        cache
            .modify_new(&mut jbd2_dev, AbsoluteBN::new(10), |data| data.fill(0xa5))
            .expect("create full cached block");
        cache
            .flush(&mut jbd2_dev, AbsoluteBN::new(10))
            .expect("flush full cached block");

        let device = jbd2_dev.into_inner();
        assert!(device.read_calls.is_empty());
        assert_eq!(device.write_calls, [(10, 1)]);
    }

    #[cfg(feature = "USE_MULTILEVEL_CACHE")]
    #[test]
    fn flush_bounds_each_contiguous_writeback_request() {
        let mut cache = DataBlockCache::new(32, BLOCK_SIZE);
        let device = TestBlockDevice::new(1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);

        for block in 10..27 {
            cache
                .modify_new(&mut jbd2_dev, AbsoluteBN::new(block), |data| {
                    data.fill(block as u8);
                })
                .expect("fill dirty cache");
        }
        cache.flush_all(&mut jbd2_dev).expect("bounded writeback");

        let device = jbd2_dev.into_inner();
        assert_eq!(device.write_calls, [(10, 16), (26, 1)]);
    }

    #[cfg(feature = "USE_MULTILEVEL_CACHE")]
    #[test]
    fn flush_commits_only_batches_acknowledged_by_the_device() {
        let mut cache = DataBlockCache::new(32, BLOCK_SIZE);
        let device = TestBlockDevice::failing_write_number(1024, 2);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);

        for block in 10..27 {
            cache
                .modify_new(&mut jbd2_dev, AbsoluteBN::new(block), |data| {
                    data.fill(block as u8);
                })
                .expect("fill dirty cache");
        }

        let error = cache
            .flush_all(&mut jbd2_dev)
            .expect_err("the second writeback batch must fail");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);
        assert_eq!(cache.stats().dirty_entries, 1);
        for block in 10..26 {
            assert!(
                !cache
                    .get(AbsoluteBN::new(block))
                    .expect("written block remains cached")
                    .dirty
            );
        }
        assert!(
            cache
                .get(AbsoluteBN::new(26))
                .expect("failed block remains cached")
                .dirty
        );

        let device = jbd2_dev.into_inner();
        assert_eq!(device.write_calls, [(10, 16)]);
    }

    #[cfg(feature = "USE_MULTILEVEL_CACHE")]
    #[test]
    fn dirty_eviction_write_failure_preserves_victim() {
        let mut cache = DataBlockCache::new(1, BLOCK_SIZE);
        let device = TestBlockDevice::failing_writes(1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let victim = AbsoluteBN::new(10);
        let replacement = AbsoluteBN::new(20);

        cache
            .modify_new(&mut jbd2_dev, victim, |data| data.fill(0xa5))
            .expect("dirty victim creation does not write through");

        let error = cache
            .create_new(&mut jbd2_dev, replacement)
            .expect_err("dirty eviction must report the device write error");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        let cached = cache.get(victim).expect("failed writeback keeps victim");
        assert!(cached.dirty);
        assert!(cached.data.iter().all(|byte| *byte == 0xa5));
        assert!(cache.get(replacement).is_none());
        assert_eq!(cache.stats().total_entries, 1);
    }

    #[cfg(not(feature = "USE_MULTILEVEL_CACHE"))]
    #[test]
    fn write_through_failure_preserves_dirty_cache_for_retry() {
        let mut cache = DataBlockCache::new(1, BLOCK_SIZE);
        let device = TestBlockDevice::failing_writes(1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let block = AbsoluteBN::new(10);

        let error = cache
            .modify_new(&mut jbd2_dev, block, |data| data.fill(0xa5))
            .expect_err("write-through must propagate the device write error");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        let cached = cache.get(block).expect("failed write keeps retry state");
        assert!(cached.dirty);
        assert!(cached.data.iter().all(|byte| *byte == 0xa5));
        assert_eq!(cache.stats().total_entries, 1);
        assert_eq!(cache.stats().dirty_entries, 1);
    }
}
