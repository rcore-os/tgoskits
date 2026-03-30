//! Data block cache helpers.

use alloc::{collections::BTreeMap, vec::Vec};

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

/// Data block cache manager.
pub struct DataBlockCache {
    /// Cached blocks.
    cache: BTreeMap<BlockCacheKey, CachedBlock>,
    /// Maximum number of cache entries.
    max_entries: usize,
    /// Access counter used by the LRU policy.
    access_counter: u64,
    /// Filesystem block size.
    block_size: usize,
}

impl DataBlockCache {
    /// Creates a data block cache.
    pub fn new(max_entries: usize, block_size: usize) -> Self {
        Self {
            cache: BTreeMap::new(),
            max_entries,
            access_counter: 0,
            block_size,
        }
    }

    /// Creates a data block cache with default settings.
    pub fn create_default() -> Self {
        Self::new(64, BLOCK_SIZE)
    }

    /// Loads one block from disk.
    fn load_block<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<Vec<u8>> {
        block_dev.read_block(block_num)?;
        let buffer = block_dev.buffer();
        Ok(buffer.to_vec())
    }

    /// Returns a cached block, loading it from disk on demand.
    pub fn get_or_load<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<&CachedBlock> {
        // Load from disk on the first cache miss.
        if !self.cache.contains_key(&block_num) {
            if self.cache.len() >= self.max_entries {
                self.evict_lru(block_dev)?;
            }

            let data = self.load_block(block_dev, block_num)?;
            let cached = CachedBlock::new(data, block_num);
            self.cache.insert(block_num, cached);
        }

        // Refresh the LRU timestamp on every access.
        self.access_counter += 1;
        if let Some(cached) = self.cache.get_mut(&block_num) {
            cached.last_access = self.access_counter;
        }

        self.cache.get(&block_num).ok_or(Ext4Error::corrupted())
    }

    /// Returns a mutable cached block, loading it from disk on demand.
    fn get_or_load_mut<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<&mut CachedBlock> {
        if !self.cache.contains_key(&block_num) {
            if self.cache.len() >= self.max_entries {
                self.evict_lru(block_dev)?;
            }

            let data = self.load_block(block_dev, block_num)?;
            let cached = CachedBlock::new(data, block_num);
            self.cache.insert(block_num, cached);
        }

        self.access_counter += 1;
        if let Some(cached) = self.cache.get_mut(&block_num) {
            cached.last_access = self.access_counter;
            Ok(cached)
        } else {
            Err(Ext4Error::corrupted())
        }
    }

    /// Returns a cached block without loading from disk.
    pub fn get(&self, block_num: AbsoluteBN) -> Option<&CachedBlock> {
        self.cache.get(&block_num)
    }

    /// Returns a mutable cached block without loading from disk.
    pub fn get_mut(&mut self, block_num: AbsoluteBN) -> Option<&mut CachedBlock> {
        if let Some(cached) = self.cache.get_mut(&block_num) {
            self.access_counter += 1;
            cached.last_access = self.access_counter;
            Some(cached)
        } else {
            None
        }
    }

    /// Creates a brand-new cached block and marks it dirty.
    pub fn create_new(&mut self, block_num: AbsoluteBN) -> &mut CachedBlock {
        if self.cache.len() >= self.max_entries {
            // Eviction for newly created entries is left to the caller because
            // no block-device handle is available here.
        }

        let data = alloc::vec![0u8; self.block_size];
        let mut cached = CachedBlock::new(data, block_num);
        cached.dirty = true;

        self.access_counter += 1;
        cached.last_access = self.access_counter;

        self.cache.insert(block_num, cached);
        self.cache.get_mut(&block_num).unwrap()
    }

    /// Marks a cached data block dirty.
    pub fn mark_dirty(&mut self, block_num: AbsoluteBN) {
        if let Some(cached) = self.cache.get_mut(&block_num) {
            cached.mark_dirty();
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
        B: BlockDevice,
        F: FnOnce(&mut [u8]),
    {
        let cached = self.get_or_load_mut(block_dev, block_num)?;
        f(&mut cached.data);
        cached.mark_dirty();

        if !USE_MULTILEVEL_CACHE {
            Self::write_block_static(block_dev, cached.block_num, &cached.data)?;
            cached.dirty = false;
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
        B: BlockDevice,
        F: FnOnce(&mut [u8]),
    {
        let cached = self.create_new(block_num);
        f(&mut cached.data);
        cached.mark_dirty();

        if !USE_MULTILEVEL_CACHE {
            Self::write_block_static(block_dev, cached.block_num, &cached.data)?;
            cached.dirty = false;
        }

        Ok(())
    }

    /// Evicts the least recently used block, flushing it first if needed.
    fn evict_lru<B: BlockDevice>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        // Evict the least recently accessed block.
        let lru_key = self
            .cache
            .iter()
            .min_by_key(|(_, cached)| cached.last_access)
            .map(|(key, _)| *key);

        if let Some(key) = lru_key {
            self.evict(block_dev, key)?;
        }

        Ok(())
    }

    /// Evicts one cached block.
    pub fn evict<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        if let Some(cached) = self.cache.remove(&block_num)
            && cached.dirty
        {
            Self::write_block_static(block_dev, cached.block_num, &cached.data)?;
        }
        Ok(())
    }

    /// Flushes all dirty cached blocks to disk.
    pub fn flush_all<B: BlockDevice>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
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

            // Count the length of one contiguous run.
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

        // All flushed entries are now clean.
        for cached in self.cache.values_mut() {
            cached.dirty = false;
        }

        Ok(())
    }

    /// Flushes one cached block to disk.
    pub fn flush<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Ext4Result<()> {
        if let Some(cached) = self.cache.get(&block_num)
            && cached.dirty
        {
            let data = cached.data.clone();
            Self::write_block_static(block_dev, block_num, &data)?;

            if let Some(cached) = self.cache.get_mut(&block_num) {
                cached.dirty = false;
            }
        }
        Ok(())
    }

    /// Writes one block to disk.
    fn write_block_static<B: BlockDevice>(
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        data: &[u8],
    ) -> Ext4Result<()> {
        block_dev.read_block(block_num)?;
        let buffer = block_dev.buffer_mut();
        buffer[..data.len()].copy_from_slice(data);
        block_dev.write_block(block_num, false)?;
        Ok(())
    }

    /// Invalidates one cached block without flushing it.
    pub fn invalidate(&mut self, block_num: AbsoluteBN) {
        self.cache.remove(&block_num);
    }

    /// Clears the cache without flushing.
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Returns cache statistics.
    pub fn stats(&self) -> DataBlockCacheStats {
        let dirty_count = self.cache.values().filter(|c| c.dirty).count();

        let total_size = self.cache.len() * self.block_size;

        DataBlockCacheStats {
            total_entries: self.cache.len(),
            dirty_entries: dirty_count,
            max_entries: self.max_entries,
            total_size_bytes: total_size,
        }
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

        let block = cache.create_new(AbsoluteBN::new(100));
        assert_eq!(block.block_num, AbsoluteBN::new(100));
        assert_eq!(block.data.len(), BLOCK_SIZE);
        assert!(block.dirty); // New cache entries should start dirty.

        let stats = cache.stats();
        assert_eq!(stats.total_entries, 1);
        assert_eq!(stats.dirty_entries, 1);
    }

    #[test]
    fn test_invalidate() {
        let mut cache = DataBlockCache::new(8, BLOCK_SIZE);

        cache.create_new(AbsoluteBN::new(100));
        assert_eq!(cache.cache.len(), 1);

        cache.invalidate(AbsoluteBN::new(100));
        assert_eq!(cache.cache.len(), 0);
    }
}
