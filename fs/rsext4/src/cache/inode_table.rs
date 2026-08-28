//! Inode table cache helpers.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

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
#[derive(Clone)]
pub struct InodeCache {
    cache: BTreeMap<InodeCacheKey, CachedInode>,
    max_entries: usize,
    access_counter: u64,
    inode_size: usize,
}

impl InodeCache {
    pub fn new(max_entries: usize, inode_size: usize) -> Self {
        Self {
            cache: BTreeMap::new(),
            max_entries,
            access_counter: 0,
            inode_size,
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
        if self.cache.contains_key(&inode_num) {
            return Ok(());
        }

        // A failed load leaves the previous cache contents untouched.
        let (inode, raw_inode) = self.load_inode(block_dev, block_num, offset)?;

        self.make_room(block_dev)?;

        self.cache.insert(
            inode_num,
            CachedInode::new(inode, raw_inode, inode_num, block_num, offset),
        );
        Ok(())
    }

    fn make_room<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        if self.cache.len() >= self.max_entries
            && let Some(victim_num) = self.lru_inode()
        {
            let victim = self
                .cache
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
            self.cache.remove(&victim_num);
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
        if self
            .cache
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
        self.cache.remove(&inode_num);
        self.make_room(block_dev)?;

        self.cache.insert(
            inode_num,
            CachedInode::new(
                Ext4Inode::default(),
                alloc::vec![0; self.inode_size],
                inode_num,
                block_num,
                offset,
            ),
        );
        Ok(())
    }

    fn lru_inode(&self) -> Option<InodeNumber> {
        self.cache
            .iter()
            .min_by_key(|(_, cached)| cached.last_access)
            .map(|(inode_num, _)| *inode_num)
    }

    fn touch(&mut self, inode_num: InodeNumber) {
        self.access_counter = self.access_counter.saturating_add(1);
        if let Some(cached) = self.cache.get_mut(&inode_num) {
            cached.last_access = self.access_counter;
            cached.generation = cached.generation.saturating_add(1);
        }
    }

    pub fn get(&self, inode_num: InodeNumber) -> Option<CachedInode> {
        self.cache.get(&inode_num).cloned()
    }

    pub fn get_mut(&mut self, inode_num: InodeNumber) -> Option<CachedInode> {
        self.touch(inode_num);
        self.cache.get(&inode_num).cloned()
    }

    pub fn mark_dirty(&mut self, inode_num: InodeNumber) {
        if let Some(cached) = self.cache.get_mut(&inode_num) {
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
        self.ensure_loaded(block_dev, inode_num, block_num, offset)?;
        self.touch(inode_num);

        let cached = self
            .cache
            .get_mut(&inode_num)
            .ok_or(Ext4Error::corrupted())?;
        let previous_inode = cached.inode;
        let previous_raw_inode = cached.raw_inode.clone();
        if let Err(error) = f(
            &mut cached.inode,
            Arc::make_mut(&mut cached.raw_inode).as_mut_slice(),
        ) {
            cached.inode = previous_inode;
            cached.raw_inode = previous_raw_inode;
            return Err(error);
        }
        cached.mark_dirty();
        cached.generation = cached.generation.saturating_add(1);

        if !USE_MULTILEVEL_CACHE {
            let block_num = cached.block_num;
            let offset = cached.offset_in_block;
            let data = cached.raw_inode.clone();
            Self::write_inode_bytes_static(block_dev, block_num, offset, &data)?;
            let cached = self
                .cache
                .get_mut(&inode_num)
                .ok_or(Ext4Error::corrupted())?;
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
        let Some(cached) = self.cache.get(&inode_num).cloned() else {
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
        self.cache.remove(&inode_num);
        Ok(())
    }

    pub fn flush_all<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        let mut dirty = self
            .cache
            .iter()
            .filter(|(_, cached)| cached.dirty)
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
        Self::write_dirty_inode_blocks(block_dev, &dirty)?;

        for (inode_num, ..) in dirty {
            if let Some(cached) = self.cache.get_mut(&inode_num) {
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
        let Some(cached) = self.cache.get(&inode_num).cloned() else {
            return Ok(());
        };
        if cached.dirty {
            Self::write_inode_bytes_static(
                block_dev,
                cached.block_num,
                cached.offset_in_block,
                &cached.raw_inode,
            )?;
            let cached = self
                .cache
                .get_mut(&inode_num)
                .ok_or(Ext4Error::corrupted())?;
            cached.dirty = false;
            cached.generation = cached.generation.saturating_add(1);
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }

    pub fn stats(&self) -> InodeCacheStats {
        InodeCacheStats {
            total_entries: self.cache.len(),
            dirty_entries: self.cache.values().filter(|cached| cached.dirty).count(),
            max_entries: self.max_entries,
        }
    }

    fn write_inode_bytes_static<B: BlockIo>(
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        offset: usize,
        data: &[u8],
    ) -> Ext4Result<()> {
        let mut buffer = alloc::vec![0u8; block_dev.block_size() as usize];
        block_dev.read_blocks(&mut buffer, block_num, 1)?;
        let end = offset
            .checked_add(data.len())
            .ok_or(Ext4Error::corrupted())?;
        let dst = buffer.get_mut(offset..end).ok_or(Ext4Error::corrupted())?;
        dst.copy_from_slice(data);
        block_dev.write_blocks(&buffer, block_num, 1, true)
    }

    fn write_dirty_inode_blocks<B: BlockIo>(
        block_dev: &mut Jbd2Dev<B>,
        dirty: &[(InodeNumber, AbsoluteBN, usize, Arc<Vec<u8>>)],
    ) -> Ext4Result<()> {
        let mut index = 0;
        while index < dirty.len() {
            let block_num = dirty[index].1;
            let mut buffer = alloc::vec![0u8; block_dev.block_size() as usize];
            block_dev.read_blocks(&mut buffer, block_num, 1)?;

            while index < dirty.len() && dirty[index].1 == block_num {
                let (_, _, offset, data) = &dirty[index];
                let end = offset
                    .checked_add(data.len())
                    .ok_or(Ext4Error::corrupted())?;
                let dst = buffer.get_mut(*offset..end).ok_or(Ext4Error::corrupted())?;
                dst.copy_from_slice(data);
                index += 1;
            }
            block_dev.write_blocks(&buffer, block_num, 1, true)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct InodeCacheStats {
    pub total_entries: usize,
    pub dirty_entries: usize,
    pub max_entries: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inode_location_calc() {
        let cache = InodeCache::default(DEFAULT_INODE_SIZE);
        let inodes_per_group = 128;
        let inode_table_start = AbsoluteBN::new(100);

        let (block, offset, group) = cache
            .calc_inode_location(
                InodeNumber::new(1).unwrap(),
                inodes_per_group,
                inode_table_start,
                BLOCK_SIZE,
            )
            .unwrap();
        assert_eq!(block, inode_table_start);
        assert_eq!(offset, 0);
        assert_eq!(group, BGIndex::new(0));

        let inodes_per_block = (BLOCK_SIZE / DEFAULT_INODE_SIZE as usize) as u32;
        let (block, offset, group) = cache
            .calc_inode_location(
                InodeNumber::new(inodes_per_block + 1).unwrap(),
                inodes_per_group,
                inode_table_start,
                BLOCK_SIZE,
            )
            .unwrap();
        assert_eq!(block, AbsoluteBN::new(101));
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
