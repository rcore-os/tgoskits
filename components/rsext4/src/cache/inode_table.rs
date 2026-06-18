//! Inode table cache helpers.

use alloc::{collections::BTreeMap, vec::Vec};

use ax_kspin::SpinNoPreempt as SpinMutex;

use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, BGIndex, InodeNumber},
    config::*,
    disknode::*,
    endian::*,
    error::*,
};

/// Snapshot type for lock-free LRU eviction.
/// `(lru_key, generation, optional dirty data: (block_num, offset, data))`
type InodeLruSnapshot = Option<(InodeNumber, u64, Option<(AbsoluteBN, usize, Vec<u8>)>)>;

/// Cache key for one global inode number.
pub type InodeCacheKey = InodeNumber;

/// Cached inode payload.
#[derive(Debug, Clone)]
pub struct CachedInode {
    /// Decoded inode value.
    pub inode: Ext4Inode,
    /// Whether the cache entry is dirty.
    pub dirty: bool,
    /// Physical inode-table block number.
    pub block_num: AbsoluteBN,
    /// Offset inside the containing block.
    pub offset_in_block: usize,
    /// Global inode number.
    pub inode_num: InodeNumber,
    /// Access timestamp used for LRU eviction.
    pub last_access: u64,
    /// Generation counter — bumped on every access, used to validate
    /// stale LRU snapshots before eviction.
    pub generation: u64,
}

impl CachedInode {
    pub fn new(
        inode: Ext4Inode,
        inode_num: InodeNumber,
        block_num: AbsoluteBN,
        offset: usize,
    ) -> Self {
        Self {
            inode,
            dirty: false,
            block_num,
            offset_in_block: offset,
            inode_num,
            last_access: 0,
            generation: 0,
        }
    }

    /// Marks the inode dirty.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Creates a lightweight inode handle for callers that only need identity.
    pub fn handle(&self) -> InodeHandle {
        InodeHandle {
            inode_num: self.inode_num,
        }
    }
}

/// Lightweight cached inode handle.
#[derive(Debug, Clone, Copy)]
pub struct InodeHandle {
    pub inode_num: InodeNumber,
}

/// Inode cache internal state — protected by `SpinMutex`.
struct InodeCacheInner {
    /// Cached inodes.
    cache: BTreeMap<InodeCacheKey, CachedInode>,
    /// Maximum number of cache entries.
    max_entries: usize,
    /// Access counter used by the LRU policy.
    access_counter: u64,
    /// On-disk inode size in bytes (immutable after construction;
    /// mirrored in `InodeCache` for lock-free access).
    inode_size: usize,
}

/// Inode cache manager with internal spinlock for SMP-safe concurrent access.
///
/// All methods take `&self`; the internal `SpinMutex` provides interior
/// mutability.  Callers must ensure the block device (`Jbd2Dev`) is
/// externally synchronized (e.g. via the VFS-layer inode lock).
///
/// `inode_size` is stored outside the spinlock because it is immutable after
/// construction and is needed by lock-free paths (`calc_inode_location`) and
/// by `load_inode` which is called from code paths that already hold the lock.
pub struct InodeCache {
    inner: SpinMutex<InodeCacheInner>,
    /// On-disk inode size in bytes (immutable after construction).
    inode_size: usize,
}

impl InodeCache {
    /// Creates an inode cache.
    pub fn new(max_entries: usize, inode_size: usize) -> Self {
        Self {
            inner: SpinMutex::new(InodeCacheInner {
                cache: BTreeMap::new(),
                max_entries,
                access_counter: 0,
                inode_size,
            }),
            inode_size,
        }
    }

    /// Creates an inode cache with the default size.
    pub fn default(inode_size: u16) -> Self {
        Self::new(INODE_CACHE_MAX, inode_size as usize)
    }

    /// Calculates the physical location of one inode table entry.
    ///
    /// Lock-free: reads immutable configuration only.
    pub fn calc_inode_location(
        &self,
        inode_num: InodeNumber,
        inodes_per_group: u32,
        inode_table_start: AbsoluteBN,
        block_size: usize,
    ) -> Ext4Result<(AbsoluteBN, usize, BGIndex)> {
        let (group_idx, idx_in_group) = inode_num.to_group(inodes_per_group)?;
        let byte_offset = idx_in_group.as_usize()? * self.inode_size;

        let block_offset = byte_offset / block_size;
        let offset_in_block = byte_offset % block_size;

        let block_num = inode_table_start.checked_add_usize(block_offset)?;

        Ok((block_num, offset_in_block, group_idx))
    }

    /// Loads one inode from disk using a caller-provided buffer.
    ///
    /// Lock-free: uses `self.inode_size` (immutable after construction)
    /// so it is safe to call from code paths that already hold the lock.
    fn load_inode<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        offset: usize,
    ) -> Ext4Result<Ext4Inode> {
        let inode_size = self.inode_size;
        // Use a local buffer to avoid the BlockDev single-block buffer,
        // which is shared mutable state that would serialize concurrent reads.
        let mut buf = alloc::vec![0u8; crate::config::BLOCK_SIZE];
        block_dev.read_blocks(&mut buf, block_num, 1)?;

        if offset + inode_size > buf.len() {
            return Err(Ext4Error::corrupted());
        }

        let inode = Ext4Inode::from_disk_bytes(&buf[offset..offset + inode_size]);
        Ok(inode)
    }

    /// Returns a cached inode, loading it from disk on demand.
    pub fn get_or_load<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        block_num: AbsoluteBN,
        offset: usize,
    ) -> Ext4Result<CachedInode> {
        let mut inner = self.inner.lock();

        // Load the inode from disk on the first cache miss.
        if !inner.cache.contains_key(&inode_num) {
            // Phase 1: snapshot LRU eviction info while holding the lock.
            let evict_info = if inner.cache.len() >= inner.max_entries {
                inner.snapshot_lru()
            } else {
                None
            };

            drop(inner);

            // Phase 2: load the requested inode from disk (no dirty writeback
            // yet — the victim snapshot may be stale).
            let inode = self.load_inode(block_dev, block_num, offset)?;

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
                    dirty_opt
                }
                _ => None,
            };

            // Use or_insert_with to avoid TOCTOU: another thread may have
            // inserted the same key while we had no lock.
            inner
                .cache
                .entry(inode_num)
                .or_insert_with(|| CachedInode::new(inode, inode_num, block_num, offset));

            drop(inner);

            // Phase 4: write the victim's dirty data to disk AFTER the
            // generation check passed (outside the spinlock).
            if let Some((lru_bn, lru_off, ref lru_data)) = dirty_to_write {
                Self::write_inode_bytes_static(block_dev, lru_bn, lru_off, lru_data)?;
            }

            inner = self.inner.lock();
        }

        // Refresh the LRU timestamp on every access.
        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        if let Some(cached) = inner.cache.get_mut(&inode_num) {
            cached.last_access = new_counter;
            cached.generation += 1;
        }

        inner
            .cache
            .get(&inode_num)
            .cloned()
            .ok_or(Ext4Error::corrupted())
    }

    /// Returns a mutable cached inode, loading it from disk on demand.
    fn get_or_load_mut<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        block_num: AbsoluteBN,
        offset: usize,
    ) -> Ext4Result<()> {
        let mut inner = self.inner.lock();

        // Load the inode from disk on the first mutable cache miss.
        if !inner.cache.contains_key(&inode_num) {
            // Phase 1: snapshot LRU eviction info while holding the lock.
            let evict_info = if inner.cache.len() >= inner.max_entries {
                inner.snapshot_lru()
            } else {
                None
            };

            drop(inner);

            // Phase 2: load the requested inode from disk (no dirty writeback
            // yet — the victim snapshot may be stale).
            let inode = self.load_inode(block_dev, block_num, offset)?;

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
                    dirty_opt
                }
                _ => None,
            };

            // Re-check after reacquiring: another thread may have inserted the
            // same key while we held no lock.
            inner
                .cache
                .entry(inode_num)
                .or_insert_with(|| CachedInode::new(inode, inode_num, block_num, offset));

            drop(inner);

            // Phase 4: write the victim's dirty data to disk AFTER the
            // generation check passed (outside the spinlock).
            if let Some((lru_bn, lru_off, ref lru_data)) = dirty_to_write {
                Self::write_inode_bytes_static(block_dev, lru_bn, lru_off, lru_data)?;
            }

            inner = self.inner.lock();
        }

        // Refresh the LRU timestamp before returning the mutable handle.
        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        if let Some(cached) = inner.cache.get_mut(&inode_num) {
            cached.last_access = new_counter;
            cached.generation += 1;
        }

        Ok(())
    }

    /// Returns a cached inode without loading from disk.
    pub fn get(&self, inode_num: InodeNumber) -> Option<CachedInode> {
        self.inner.lock().cache.get(&inode_num).cloned()
    }

    /// Returns a mutable cached inode without loading from disk.
    pub fn get_mut(&self, inode_num: InodeNumber) -> Option<CachedInode> {
        let mut inner = self.inner.lock();
        let new_counter = inner.access_counter + 1;
        inner.access_counter = new_counter;
        // Counter updated outside the get_mut borrow scope to avoid conflicts.
        inner.cache.get_mut(&inode_num).map(|cached| {
            cached.last_access = new_counter;
            cached.generation += 1;
            cached.clone()
        })
    }

    /// Marks a cached inode dirty.
    pub fn mark_dirty(&self, inode_num: InodeNumber) {
        let mut inner = self.inner.lock();
        if let Some(cached) = inner.cache.get_mut(&inode_num) {
            cached.mark_dirty();
            cached.generation += 1;
        }
    }

    /// Modifies one cached inode and marks it dirty.
    pub fn modify<B, F>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        block_num: AbsoluteBN,
        offset: usize,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockDevice,
        F: FnOnce(&mut Ext4Inode),
    {
        self.get_or_load_mut(block_dev, inode_num, block_num, offset)?;

        let mut inner = self.inner.lock();
        let inode_size = inner.inode_size;
        // get_or_load_mut succeeded above, so the entry must exist unless
        // concurrently evicted — treat that as filesystem corruption.
        let cached = inner
            .cache
            .get_mut(&inode_num)
            .ok_or(Ext4Error::corrupted())?;
        f(&mut cached.inode);
        cached.mark_dirty();
        cached.generation += 1;

        if !USE_MULTILEVEL_CACHE {
            // Drop lock during synchronous I/O
            let block_num = cached.block_num;
            let offset = cached.offset_in_block;
            let mut buf = alloc::vec![0u8; inode_size];
            cached.inode.to_disk_bytes(&mut buf);
            drop(inner);

            Self::write_inode_bytes_static(block_dev, block_num, offset, &buf)?;

            inner = self.inner.lock();
            if let Some(cached) = inner.cache.get_mut(&inode_num) {
                cached.dirty = false;
                cached.generation += 1;
            }
        }
        Ok(())
    }

    /// Convenience wrapper that modifies an inode by handle.
    pub fn modify_by_handle<B, F>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        handle: InodeHandle,
        block_num: AbsoluteBN,
        offset: usize,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockDevice,
        F: FnOnce(&mut Ext4Inode),
    {
        self.modify(block_dev, handle.inode_num, block_num, offset, f)
    }

    /// Evicts one cached inode.
    pub fn evict<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<()> {
        let dirty_inode = self.inner.lock().inode_for_evict(inode_num);
        if let Some((generation, block_num, offset, data)) = dirty_inode {
            Self::write_inode_bytes_static(block_dev, block_num, offset, &data)?;
            self.inner
                .lock()
                .remove_if_generation(inode_num, generation);
        } else {
            self.inner.lock().remove_clean(inode_num);
        }
        Ok(())
    }

    /// Flushes all dirty inodes to disk.
    pub fn flush_all<B: BlockDevice>(&self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        let dirty_inodes = self.inner.lock().dirty_inodes_for_flush();
        if dirty_inodes.is_empty() {
            return Ok(());
        }

        Self::write_dirty_inode_blocks(block_dev, &dirty_inodes)?;

        let flushed_inodes = dirty_inodes
            .into_iter()
            .map(|(inode_num, generation, ..)| (inode_num, generation))
            .collect::<Vec<_>>();
        self.inner.lock().mark_flushed(&flushed_inodes);
        Ok(())
    }

    /// Flushes one inode to disk.
    pub fn flush<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<()> {
        let dirty_inode = self.inner.lock().inode_for_flush(inode_num);
        if let Some((generation, block_num, offset, data)) = dirty_inode {
            Self::write_inode_bytes_static(block_dev, block_num, offset, &data)?;
            self.inner.lock().mark_flushed(&[(inode_num, generation)]);
        }
        Ok(())
    }

    /// Clears the cache without flushing.
    pub fn clear(&self) {
        self.inner.lock().cache.clear();
    }

    /// Returns cache statistics.
    pub fn stats(&self) -> InodeCacheStats {
        let inner = self.inner.lock();
        let dirty_count = inner.cache.values().filter(|c| c.dirty).count();

        InodeCacheStats {
            total_entries: inner.cache.len(),
            dirty_entries: dirty_count,
            max_entries: inner.max_entries,
        }
    }

    /// Writes encoded inode bytes to disk (static, lock-free helper).
    fn write_inode_bytes_static<B: BlockDevice>(
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        offset: usize,
        data: &[u8],
    ) -> Ext4Result<()> {
        let mut buf = alloc::vec![0u8; crate::config::BLOCK_SIZE];
        block_dev.read_blocks(&mut buf, block_num, 1)?;
        let end = offset
            .checked_add(data.len())
            .ok_or(Ext4Error::corrupted())?;
        if end > buf.len() {
            return Err(Ext4Error::corrupted());
        }
        buf[offset..end].copy_from_slice(data);
        block_dev.write_blocks(&buf, block_num, 1, true)?; // is_metadata: inode table blocks are filesystem metadata
        Ok(())
    }

    fn write_dirty_inode_blocks<B: BlockDevice>(
        block_dev: &mut Jbd2Dev<B>,
        dirty_inodes: &[(InodeNumber, u64, AbsoluteBN, usize, Vec<u8>)],
    ) -> Ext4Result<()> {
        let mut idx = 0usize;
        while idx < dirty_inodes.len() {
            let (_, _, block_num, ..) = dirty_inodes[idx];

            let mut buf = alloc::vec![0u8; crate::config::BLOCK_SIZE];
            block_dev.read_blocks(&mut buf, block_num, 1)?;

            while idx < dirty_inodes.len() && dirty_inodes[idx].2 == block_num {
                let (_, _, _b, offset, ref data) = dirty_inodes[idx];
                let end = offset + data.len();
                if end > buf.len() {
                    return Err(Ext4Error::corrupted());
                }
                buf[offset..end].copy_from_slice(data);
                idx += 1;
            }

            block_dev.write_blocks(&buf, block_num, 1, true)?; // is_metadata: inode table blocks are filesystem metadata
        }
        Ok(())
    }
}

/// Inode cache statistics.
#[derive(Debug, Clone, Copy)]
pub struct InodeCacheStats {
    pub total_entries: usize,
    pub dirty_entries: usize,
    pub max_entries: usize,
}

// ── Inner methods (caller holds `self.inner.lock()`) ─────────────────────────

impl InodeCacheInner {
    /// Snapshots the LRU entry for lock-free eviction.
    ///
    /// Returns `(lru_key, generation, dirty_write_info)` where `generation`
    /// is the entry's generation at snapshot time.  The caller must do the
    /// I/O *without* holding the spinlock, then re-lock, verify the entry's
    /// generation still matches, and only then remove it.
    fn snapshot_lru(&self) -> InodeLruSnapshot {
        let lru_key = self
            .cache
            .iter()
            .min_by_key(|(_, cached)| cached.last_access)
            .map(|(key, _)| *key)?;

        let lru_gen = self.cache.get(&lru_key).map(|cached| cached.generation)?;

        let dirty_info = self.cache.get(&lru_key).and_then(|cached| {
            if cached.dirty {
                let mut buf = alloc::vec![0u8; self.inode_size];
                cached.inode.to_disk_bytes(&mut buf);
                Some((cached.block_num, cached.offset_in_block, buf))
            } else {
                None
            }
        });

        Some((lru_key, lru_gen, dirty_info))
    }

    fn inode_for_evict(&self, inode_num: InodeNumber) -> Option<(u64, AbsoluteBN, usize, Vec<u8>)> {
        self.cache.get(&inode_num).and_then(|cached| {
            if cached.dirty {
                let generation = cached.generation;
                let mut buf = alloc::vec![0u8; self.inode_size];
                cached.inode.to_disk_bytes(&mut buf);
                Some((generation, cached.block_num, cached.offset_in_block, buf))
            } else {
                None
            }
        })
    }

    fn remove_if_generation(&mut self, inode_num: InodeNumber, generation: u64) {
        if self
            .cache
            .get(&inode_num)
            .is_some_and(|cached| cached.generation == generation)
        {
            self.cache.remove(&inode_num);
        }
    }

    fn remove_clean(&mut self, inode_num: InodeNumber) {
        if self
            .cache
            .get(&inode_num)
            .is_some_and(|cached| !cached.dirty)
        {
            self.cache.remove(&inode_num);
        }
    }

    fn inode_for_flush(&self, inode_num: InodeNumber) -> Option<(u64, AbsoluteBN, usize, Vec<u8>)> {
        self.cache.get(&inode_num).and_then(|cached| {
            if cached.dirty {
                let generation = cached.generation;
                let block_num = cached.block_num;
                let offset = cached.offset_in_block;
                let mut buf = alloc::vec![0u8; self.inode_size];
                cached.inode.to_disk_bytes(&mut buf);
                Some((generation, block_num, offset, buf))
            } else {
                None
            }
        })
    }

    fn dirty_inodes_for_flush(&self) -> Vec<(InodeNumber, u64, AbsoluteBN, usize, Vec<u8>)> {
        let mut dirty_inodes: Vec<(InodeNumber, u64, AbsoluteBN, usize, Vec<u8>)> = self
            .cache
            .iter()
            .filter(|(_, cached)| cached.dirty)
            .map(|(inode_num, cached)| {
                let mut buf = alloc::vec![0u8; self.inode_size];
                cached.inode.to_disk_bytes(&mut buf);
                (
                    *inode_num,
                    cached.generation,
                    cached.block_num,
                    cached.offset_in_block,
                    buf,
                )
            })
            .collect();

        dirty_inodes.sort_by_key(|(_, _, block_num, offset, _)| (*block_num, *offset));
        dirty_inodes
    }

    fn mark_flushed(&mut self, inodes: &[(InodeNumber, u64)]) {
        for (inode_num, generation) in inodes {
            if let Some(cached) = self.cache.get_mut(inode_num)
                && cached.generation == *generation
            {
                cached.dirty = false;
                cached.generation += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inode_location_calc() {
        let cache = InodeCache::default(DEFAULT_INODE_SIZE);

        let inodes_per_group = 128;
        let inode_table_start = 100;
        let block_size = BLOCK_SIZE;

        let (block, offset, group) = cache
            .calc_inode_location(
                InodeNumber::new(1).unwrap(),
                inodes_per_group,
                AbsoluteBN::new(inode_table_start),
                block_size,
            )
            .unwrap();
        assert_eq!(block, AbsoluteBN::new(100));
        assert_eq!(offset, 0);
        assert_eq!(group, BGIndex::new(0));

        let inodes_per_block = (block_size / DEFAULT_INODE_SIZE as usize) as u32;
        let (block, offset, group) = cache
            .calc_inode_location(
                InodeNumber::new(inodes_per_block + 1).unwrap(),
                inodes_per_group,
                AbsoluteBN::new(inode_table_start),
                block_size,
            )
            .unwrap();
        assert_eq!(block, AbsoluteBN::new(inode_table_start + 1));
        assert_eq!(offset, 0);
        assert_eq!(group, BGIndex::new(0));
    }

    #[test]
    fn test_inode_cache_basic() {
        let cache = InodeCache::new(4, 256);
        let stats = cache.stats();

        assert_eq!(stats.total_entries, 0);
        assert_eq!(stats.max_entries, 4);
    }
}
