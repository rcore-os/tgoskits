//! Canonical inode-table state, shared only through a read-only capability.

mod demand;
mod reader;
mod writeback;
use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::AtomicBool;

pub(crate) use demand::InodeLoadVersion;
pub use reader::InodeCacheReader;
use reader::SharedInodes;

use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, BGIndex, InodeNumber},
    config::*,
    disknode::*,
    error::*,
};

/// Cache key for one global inode number.
pub type InodeCacheKey = InodeNumber;

/// Cached inode payload.
#[derive(Debug, Clone)]
pub struct CachedInode {
    pub inode: Ext4Inode,
    raw_inode: Arc<Vec<u8>>,
    pub dirty: bool,
    pub block_num: AbsoluteBN,
    pub offset_in_block: usize,
    pub inode_num: InodeNumber,
    pub last_access: u64,
    pub generation: u64,
}

impl CachedInode {
    pub fn new(
        inode: Ext4Inode,
        raw_inode: Vec<u8>,
        inode_num: InodeNumber,
        block_num: AbsoluteBN,
        offset_in_block: usize,
    ) -> Self {
        Self {
            inode,
            raw_inode: Arc::new(raw_inode),
            dirty: false,
            block_num,
            offset_in_block,
            inode_num,
            last_access: 0,
            generation: 0,
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn handle(&self) -> InodeHandle {
        InodeHandle {
            inode_num: self.inode_num,
        }
    }

    pub(crate) fn raw_inode(&self) -> &[u8] {
        self.raw_inode.as_slice()
    }
}

/// Lightweight cached inode handle.
#[derive(Debug, Clone, Copy)]
pub struct InodeHandle {
    pub inode_num: InodeNumber,
}

/// Inode cache owned exclusively by one mounted filesystem.
pub struct InodeCache {
    cache: Arc<SharedInodes>,
    max_entries: usize,
    access_counter: u64,
    inode_size: usize,
    pending_reads: BTreeMap<InodeNumber, Weak<AtomicBool>>,
}

impl InodeCache {
    pub fn new(max_entries: usize, inode_size: usize) -> Self {
        Self {
            cache: Arc::new(SharedInodes::new(BTreeMap::new())),
            max_entries,
            access_counter: 0,
            inode_size,
            pending_reads: BTreeMap::new(),
        }
    }

    pub fn default(inode_size: u16) -> Self {
        Self::new(INODE_CACHE_MAX, inode_size as usize)
    }

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
        Ok((
            inode_table_start.checked_add_usize(block_offset)?,
            offset_in_block,
            group_idx,
        ))
    }

    fn load_inode<B: BlockIo>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        offset: usize,
    ) -> Ext4Result<(Ext4Inode, Vec<u8>)> {
        let mut buffer = alloc::vec![0u8; block_dev.block_size() as usize];
        block_dev.read_blocks(&mut buffer, block_num, 1)?;
        let end = offset
            .checked_add(self.inode_size)
            .ok_or(Ext4Error::corrupted())?;
        let bytes = buffer.get(offset..end).ok_or(Ext4Error::corrupted())?;
        let raw_inode = bytes.to_vec();
        Ok((Ext4Inode::decode_checked(&raw_inode)?, raw_inode))
    }

    pub fn get_or_load<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        block_num: AbsoluteBN,
        offset: usize,
    ) -> Ext4Result<CachedInode> {
        self.ensure_loaded(block_dev, inode_num, block_num, offset)?;
        self.touch(inode_num);
        self.cache
            .entries
            .lock()
            .get(&inode_num)
            .cloned()
            .ok_or(Ext4Error::corrupted())
    }

    fn ensure_loaded<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        block_num: AbsoluteBN,
        offset: usize,
    ) -> Ext4Result<()> {
        if self.cache.entries.lock().contains_key(&inode_num) {
            return Ok(());
        }

        // A failed load leaves the previous cache contents untouched.
        let (inode, raw_inode) = self.load_inode(block_dev, block_num, offset)?;

        self.make_room(block_dev)?;

        let cached = CachedInode::new(inode, raw_inode, inode_num, block_num, offset);
        self.cache.entries.lock().insert(inode_num, cached);
        Ok(())
    }

    fn make_room<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        let full = self.cache.entries.lock().len() >= self.max_entries;
        if full && let Some(victim_num) = self.lru_inode() {
            let victim = self
                .cache
                .entries
                .lock()
                .get(&victim_num)
                .cloned()
                .ok_or(Ext4Error::corrupted())?;
            if victim.dirty {
                Self::write_inode_bytes_static(
                    block_dev,
                    victim.block_num,
                    victim.offset_in_block,
                    &victim.raw_inode,
                )?;
            }
            self.cache.entries.lock().remove(&victim_num);
        }

        Ok(())
    }

    /// Installs an all-zero record for a newly allocated inode without reading
    /// stale bytes from an inode table that Linux has not initialized yet.
    pub(crate) fn initialize_zeroed<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        block_num: AbsoluteBN,
        offset: usize,
    ) -> Ext4Result<()> {
        self.invalidate_read(inode_num);
        if self
            .cache
            .entries
            .lock()
            .get(&inode_num)
            .is_some_and(|cached| cached.dirty)
        {
            return Err(Ext4Error::corrupted().with_operation("inode_cache:initialize_dirty"));
        }
        let end = offset
            .checked_add(self.inode_size)
            .ok_or_else(Ext4Error::overflow)?;
        if end > block_dev.block_size() as usize {
            return Err(Ext4Error::corrupted().with_operation("inode_cache:initialize_range"));
        }
        self.cache.entries.lock().remove(&inode_num);
        self.make_room(block_dev)?;

        let cached = CachedInode::new(
            Ext4Inode::default(),
            alloc::vec![0; self.inode_size],
            inode_num,
            block_num,
            offset,
        );
        self.cache.entries.lock().insert(inode_num, cached);
        Ok(())
    }

    fn lru_inode(&self) -> Option<InodeNumber> {
        self.cache
            .entries
            .lock()
            .iter()
            .min_by_key(|(_, cached)| cached.last_access)
            .map(|(inode_num, _)| *inode_num)
    }

    fn touch(&mut self, inode_num: InodeNumber) {
        self.access_counter = self.access_counter.saturating_add(1);
        if let Some(cached) = self.cache.entries.lock().get_mut(&inode_num) {
            cached.last_access = self.access_counter;
            cached.generation = cached.generation.saturating_add(1);
        }
    }

    pub fn get(&self, inode_num: InodeNumber) -> Option<CachedInode> {
        self.cache.entries.lock().get(&inode_num).cloned()
    }

    pub fn get_mut(&mut self, inode_num: InodeNumber) -> Option<CachedInode> {
        self.touch(inode_num);
        self.cache.entries.lock().get(&inode_num).cloned()
    }

    pub fn mark_dirty(&mut self, inode_num: InodeNumber) {
        self.invalidate_read(inode_num);
        if let Some(cached) = self.cache.entries.lock().get_mut(&inode_num) {
            cached.mark_dirty();
            cached.generation = cached.generation.saturating_add(1);
        }
    }

    pub fn modify<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        block_num: AbsoluteBN,
        offset: usize,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockIo,
        F: FnOnce(&mut Ext4Inode, &mut [u8]) -> Ext4Result<()>,
    {
        self.invalidate_read(inode_num);
        self.ensure_loaded(block_dev, inode_num, block_num, offset)?;
        self.touch(inode_num);

        let mut cached = self
            .cache
            .entries
            .lock()
            .get(&inode_num)
            .cloned()
            .ok_or(Ext4Error::corrupted())?;
        f(
            &mut cached.inode,
            Arc::make_mut(&mut cached.raw_inode).as_mut_slice(),
        )?;
        cached.mark_dirty();
        cached.generation = cached.generation.saturating_add(1);
        let previous = self.cache.entries.lock().insert(inode_num, cached.clone());
        drop(previous);

        if !USE_MULTILEVEL_CACHE {
            let block_num = cached.block_num;
            let offset = cached.offset_in_block;
            let data = cached.raw_inode.clone();
            Self::write_inode_bytes_static(block_dev, block_num, offset, &data)?;
            let mut entries = self.cache.entries.lock();
            let cached = entries.get_mut(&inode_num).ok_or(Ext4Error::corrupted())?;
            cached.dirty = false;
            cached.generation = cached.generation.saturating_add(1);
        }
        Ok(())
    }

    pub fn modify_by_handle<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        handle: InodeHandle,
        block_num: AbsoluteBN,
        offset: usize,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockIo,
        F: FnOnce(&mut Ext4Inode, &mut [u8]) -> Ext4Result<()>,
    {
        self.modify(block_dev, handle.inode_num, block_num, offset, f)
    }

    pub fn evict<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<()> {
        self.invalidate_read(inode_num);
        let Some(cached) = self.cache.entries.lock().get(&inode_num).cloned() else {
            return Ok(());
        };
        if cached.dirty {
            Self::write_inode_bytes_static(
                block_dev,
                cached.block_num,
                cached.offset_in_block,
                &cached.raw_inode,
            )?;
        }
        self.cache.entries.lock().remove(&inode_num);
        Ok(())
    }

    pub fn flush_all<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        self.flush_selected(block_dev, None)
    }

    pub(crate) fn dirty_blocks(&self) -> Vec<AbsoluteBN> {
        let mut blocks: Vec<_> = self
            .cache
            .entries
            .lock()
            .values()
            .filter(|cached| cached.dirty)
            .map(|cached| cached.block_num)
            .collect();
        blocks.sort_unstable();
        blocks.dedup();
        blocks
    }

    /// The caller validates the read epoch before allowing these home bytes
    /// to participate in journal assembly. Only current dirty records merge.
    pub(crate) fn flush_pre_read<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        blocks: &[(AbsoluteBN, Vec<u8>)],
    ) -> Ext4Result<()> {
        self.flush_selected(block_dev, Some(blocks))
    }

    fn flush_selected<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        blocks: Option<&[(AbsoluteBN, Vec<u8>)]>,
    ) -> Ext4Result<()> {
        let mut dirty = self
            .cache
            .entries
            .lock()
            .iter()
            .filter(|(_, cached)| {
                cached.dirty
                    && blocks.is_none_or(|blocks| {
                        blocks
                            .binary_search_by_key(&cached.block_num, |(block, _)| *block)
                            .is_ok()
                    })
            })
            .map(|(inode_num, cached)| {
                (
                    *inode_num,
                    cached.block_num,
                    cached.offset_in_block,
                    cached.raw_inode.clone(),
                )
            })
            .collect::<Vec<_>>();
        dirty.sort_by_key(|(_, block_num, offset, _)| (*block_num, *offset));
        Self::write_dirty_inode_blocks(block_dev, &dirty, blocks)?;

        for (inode_num, ..) in dirty {
            if let Some(cached) = self.cache.entries.lock().get_mut(&inode_num) {
                cached.dirty = false;
                cached.generation = cached.generation.saturating_add(1);
            }
        }
        Ok(())
    }

    pub fn flush<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<()> {
        let Some(cached) = self.cache.entries.lock().get(&inode_num).cloned() else {
            return Ok(());
        };
        if cached.dirty {
            Self::write_inode_bytes_static(
                block_dev,
                cached.block_num,
                cached.offset_in_block,
                &cached.raw_inode,
            )?;
            let mut entries = self.cache.entries.lock();
            let cached = entries.get_mut(&inode_num).ok_or(Ext4Error::corrupted())?;
            cached.dirty = false;
            cached.generation = cached.generation.saturating_add(1);
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        self.invalidate_all_reads();
        let previous = core::mem::take(&mut *self.cache.entries.lock());
        drop(previous);
    }

    pub fn stats(&self) -> InodeCacheStats {
        let entries = self.cache.entries.lock();
        InodeCacheStats {
            total_entries: entries.len(),
            dirty_entries: entries.values().filter(|cached| cached.dirty).count(),
            max_entries: self.max_entries,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct InodeCacheStats {
    pub total_entries: usize,
    pub dirty_entries: usize,
    pub max_entries: usize,
}

#[cfg(test)]
mod tests;
