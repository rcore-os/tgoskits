//! Growth of an exclusively owned mount, before any filesystem user is published.
//!
//! Preflight validates the complete target before the OS changes its partition
//! table. Growth then advances by bounded groups per durable transaction;
//! recovery can resume from the last committed geometry after interruption.

mod layout;
mod resize_inode;
mod step;

use ::alloc::collections::BTreeMap;
use layout::GrowthLayout;

use super::*;
use crate::runtime::{Event, GrowthEvent};

/// Complete candidate block images, keyed by absolute filesystem block number.
/// Aliasing updates (such as inode 7 and a GDT entry) merge into one image before
/// journal credits are counted; preparing them does not modify live metadata.
type Images = BTreeMap<AbsoluteBN, Vec<u8>>;

/// Returns the staged image of absolute filesystem `block` in `images`, loading
/// its visible bytes from `device` only on first access. The returned mutable
/// reference changes the candidate, not the device's mounted metadata.
fn image<'a, B: BlockIo>(
    images: &'a mut Images,
    device: &mut Jbd2Dev<B>,
    block: u64,
) -> Ext4Result<&'a mut Vec<u8>> {
    let block = AbsoluteBN::new(block);
    match images.entry(block) {
        ::alloc::collections::btree_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
        ::alloc::collections::btree_map::Entry::Vacant(entry) => {
            device.read_block(block)?;
            Ok(entry.insert(device.buffer().to_vec()))
        }
    }
}

/// Stores the filesystem block count `blocks` in both halves of `superblock`.
/// Callers separately validate 32-bit feature limits and update the checksum.
fn set_blocks(superblock: &mut Ext4Superblock, blocks: u64) {
    superblock.s_blocks_count_lo = blocks as u32;
    superblock.s_blocks_count_hi = (blocks >> 32) as u32;
}

impl Ext4FileSystem {
    /// Checks a target partition capacity without performing growth writes.
    /// `target_bytes` may exceed the current device view; the caller must
    /// recreate that view after committing its partition table.
    /// Returns the usable filesystem capacity in bytes, possibly leaving a tiny
    /// final group unused. Unsupported layouts and invalid metadata are errors.
    /// `device` must belong to this exclusive mount and use its filesystem block
    /// size. The check may read/cache metadata but does not publish new geometry.
    pub fn growth_capacity<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        target_bytes: u64,
    ) -> Ext4Result<u64> {
        let layout = GrowthLayout::check(self, device, target_bytes)?;
        resize_inode::validate(self, device, &layout)?;
        if layout.blocks > self.superblock.blocks_count() {
            let group = self
                .group_count
                .checked_sub(1)
                .ok_or_else(Ext4Error::corrupted)?;
            let descriptor = self
                .group_descs
                .get(group as usize)
                .ok_or_else(Ext4Error::corrupted)?;
            if !descriptor.is_block_bitmap_uninit()
                && ext4_superblock_has_metadata_csum(&self.superblock)
            {
                device.read_block(AbsoluteBN::new(descriptor.block_bitmap()))?;
                if !descriptor.block_bitmap_csum_matches(
                    &self.superblock,
                    ext4_block_bitmap_csum32(&self.superblock, device.buffer()),
                ) {
                    return Err(Ext4Error::checksum().with_operation("grow:tail_bitmap_checksum"));
                }
            }
        }
        Ok(layout.blocks * self.block_size() as u64)
    }

    /// Grows only while this mount has exclusive ownership of device geometry.
    /// A failed step requires reopening the device, not reuse of cached state.
    /// `device` supplies this mount's journal and final partition capacity;
    /// successful return means every required batch has been committed and all
    /// allocators, caches and journal protection reflect the final geometry.
    /// `observer` receives initial geometry and committed progress after at
    /// least a GiB of growth or the final batch. Callbacks must not reenter
    /// the filesystem or cancel pending I/O.
    pub(super) fn grow_before_publish<B: BlockIo, O: crate::runtime::Observer>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        observer: &mut O,
    ) -> Ext4Result<()> {
        let target = device
            .total_blocks()
            .checked_mul(self.block_size() as u64)
            .ok_or_else(Ext4Error::overflow)?;
        let capacity = self.growth_capacity(device, target)? / self.block_size() as u64;
        let block_size = self.block_size() as u64;
        let target_bytes = capacity * block_size;
        let mut reported_bytes = self.superblock.blocks_count() * block_size;
        if self.superblock.blocks_count() >= capacity {
            return Ok(());
        }
        observer.event(Event::Growth(GrowthEvent::Started {
            current_bytes: reported_bytes,
            target_bytes,
        }));
        while self.superblock.blocks_count() < capacity {
            let candidate = self.prepare_growth_step(device, capacity)?;
            candidate.publish(self, device)?;
            let current_bytes = self.superblock.blocks_count() * block_size;
            if current_bytes - reported_bytes >= 1024 * 1024 * 1024
                || self.superblock.blocks_count() == capacity
            {
                observer.event(Event::Growth(GrowthEvent::Committed { current_bytes }));
                reported_bytes = current_bytes;
            }
        }
        // Finish all home writes and release the durable log before resetting
        // runtime/journal state or returning the exclusive mounted owner.
        device.umount_commit()?;
        // No allocator or ordinary inode reader is published during growth.
        // Existing inode 7 locations and the initial partial-tail protection
        // stay valid; rebuilding the full runtime after each batch would repeat
        // all descriptor reads and journal mapping walks unnecessarily.
        self.reset_runtime_from_superblock(device, true)?;
        let journal = InodeNumber::new(self.superblock.s_journal_inum)?;
        self.protect_journal_blocks(device, journal)?;
        Ok(())
    }
}
