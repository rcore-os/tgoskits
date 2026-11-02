//! Bounded compressed anonymous swap, owned by each address space.
//!
//! Like Linux swap entries, these owners survive a nonresident PTE and fork.
//! Unlike a disk swap device, an incompressible page stays resident. No I/O or
//! allocator callback enters this module: reclaim runs in a sleepable task.

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr, VirtAddrRange};
use ax_runtime::hal::{mem::phys_to_virt, trap::PageFaultFlags};

use super::{
    AddrSpace, EvictMappingOutcome, FaultResult, MappingSlotKey, PageOrder, PageState, RssKind,
};
use crate::{StarryError, StarryResult};

static STORED_PAGES: AtomicUsize = AtomicUsize::new(0);
static STORED_BYTES: AtomicUsize = AtomicUsize::new(0);
static PAGE_INS: AtomicUsize = AtomicUsize::new(0);
static PAGE_OUTS: AtomicUsize = AtomicUsize::new(0);
const MAX_COMPRESSED_BYTES: usize = PAGE_SIZE_4K / 2;

/// Immutable contents; fork shares storage until each MM faults or unmaps it.
pub(super) struct SwapPage {
    compressed: [Vec<u8>; 2],
}

impl SwapPage {
    fn compress(source: &[u8]) -> StarryResult<Option<Arc<Self>>> {
        let mut scratch = [0; lz4_flex::block::get_maximum_output_size(PAGE_SIZE_4K)];
        let size =
            lz4_flex::block::compress_into(source, &mut scratch).map_err(|_| StarryError::Io)?;
        if size > MAX_COMPRESSED_BYTES {
            return Ok(None);
        }
        // A 2 KiB slab object consumes a whole 4 KiB slab after its aligned
        // header. Split the payload so each allocation fits a class that
        // packs multiple objects per slab and actually saves physical RAM.
        let mut compressed = [Vec::new(), Vec::new()];
        for (buffer, chunk) in compressed
            .iter_mut()
            .zip(scratch[..size].chunks(MAX_COMPRESSED_BYTES / 2))
        {
            buffer
                .try_reserve_exact(chunk.len())
                .map_err(|_| StarryError::NoMemory)?;
            buffer.extend_from_slice(chunk);
        }
        let bytes = compressed.iter().map(Vec::capacity).sum();
        let limit = ax_runtime::hal::mem::total_ram_size() / 8;
        if STORED_BYTES
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes).filter(|next| *next <= limit)
            })
            .is_err()
        {
            return Ok(None);
        }
        let page_limit = ax_runtime::hal::mem::total_ram_size() / 2 / PAGE_SIZE_4K;
        if STORED_PAGES
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |pages| {
                (pages < page_limit).then_some(pages + 1)
            })
            .is_err()
        {
            STORED_BYTES.fetch_sub(bytes, Ordering::Relaxed);
            return Ok(None);
        }
        Arc::try_new(Self { compressed })
            .map(Some)
            .map_err(|_| StarryError::NoMemory)
    }

    pub(super) fn read_into(&self, destination: &mut [u8]) -> StarryResult {
        if destination.len() != PAGE_SIZE_4K {
            return Err(StarryError::InvalidInput);
        }
        let mut encoded = [0; MAX_COMPRESSED_BYTES];
        let mut length = 0;
        for chunk in &self.compressed {
            encoded[length..length + chunk.len()].copy_from_slice(chunk);
            length += chunk.len();
        }
        let size = lz4_flex::block::decompress_into(&encoded[..length], destination)
            .map_err(|_| StarryError::Io)?;
        if size != PAGE_SIZE_4K {
            return Err(StarryError::Io);
        }
        Ok(())
    }

    fn available_pages() -> usize {
        let total = ax_runtime::hal::mem::total_ram_size();
        let bytes = (total / 8).saturating_sub(STORED_BYTES.load(Ordering::Relaxed));
        let pages = (total / 2 / PAGE_SIZE_4K).saturating_sub(STORED_PAGES.load(Ordering::Relaxed));
        pages.min(bytes / MAX_COMPRESSED_BYTES)
    }
}

impl Drop for SwapPage {
    fn drop(&mut self) {
        let bytes = self.compressed.iter().map(Vec::capacity).sum();
        STORED_BYTES.fetch_sub(bytes, Ordering::Relaxed);
        STORED_PAGES.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Diagnostic counters only; the MM's swap entries remain authoritative.
pub(crate) fn swap_usage() -> (usize, usize, usize, usize) {
    (
        STORED_PAGES.load(Ordering::Relaxed) * PAGE_SIZE_4K,
        STORED_BYTES.load(Ordering::Relaxed),
        PAGE_INS.load(Ordering::Relaxed),
        PAGE_OUTS.load(Ordering::Relaxed),
    )
}

impl AddrSpace {
    pub(crate) fn swapped_page_count(&self) -> usize {
        self.swapped_pages.len()
    }

    pub(super) fn forget_swap_range(&mut self, range: VirtAddrRange) {
        while let Some((&address, _)) = self.swapped_pages.range(range.start..range.end).next() {
            self.swapped_pages.remove(&address);
        }
    }

    pub(super) fn finish_swap_in(&mut self, address: VirtAddr) {
        if self.swapped_pages.remove(&address).is_some() {
            PAGE_INS.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Restore before the enclosing populate transaction takes its preimage.
    /// The MM mutex excludes reclaim/fork throughout this synchronous path.
    pub(crate) fn restore_swapped_range(&mut self, range: VirtAddrRange) -> StarryResult {
        while let Some((&address, _)) = self.swapped_pages.range(range.start..range.end).next() {
            match self.handle_page_fault_result(address, PageFaultFlags::empty()) {
                FaultResult::Handled => {}
                FaultResult::NoMemory => return Err(StarryError::NoMemory),
                FaultResult::Retry => return Err(StarryError::ResourceBusy),
                _ => return Err(StarryError::Io),
            }
        }
        Ok(())
    }

    /// Scan a bounded number of resident slots, resuming after the last visit.
    /// This first backend skips shared, pinned, huge, locked and file pages.
    pub(crate) fn swap_out_batch(&mut self, scan_limit: usize) -> StarryResult<usize> {
        let scan_limit = scan_limit.min(SwapPage::available_pages());
        if scan_limit == 0 {
            return Ok(0);
        }
        let mut keys = Vec::new();
        keys.try_reserve(scan_limit)
            .map_err(|_| StarryError::NoMemory)?;
        for _ in 0..scan_limit {
            let next = match self.swap_scan_cursor {
                Some(after) => self
                    .mapping_slots
                    .range((
                        core::ops::Bound::Excluded(after),
                        core::ops::Bound::Unbounded,
                    ))
                    .next(),
                None => self.mapping_slots.iter().next(),
            };
            let Some((&key, _)) = next else {
                self.swap_scan_cursor = None;
                break;
            };
            self.swap_scan_cursor = Some(key);
            keys.push(key);
        }
        self.swap_out_pages(&keys)
    }

    fn swap_candidate(&self, key: MappingSlotKey) -> StarryResult<Option<Arc<super::PageObject>>> {
        let Some(slot) = self.mapping_slots.get(&key) else {
            return Ok(None);
        };
        if slot.page_order != PageOrder::BASE
            || slot.resident_kind() != Some(RssKind::Anon)
            || slot.page.state() != PageState::Present
            || slot.page.mapping_refs() != 1
            || !slot.page.frame().is_exclusive_base_allocation()
            || Arc::strong_count(&slot.page) != 1
        {
            return Ok(None);
        }
        let Some(entry) = self.vma_root.lookup_entry(key.va) else {
            return Err(StarryError::BadState);
        };
        if entry.snapshot().lock_mode.is_locked() || !entry.operation().is_private_anonymous() {
            return Ok(None);
        }
        Ok(Some(slot.page.clone()))
    }

    fn swap_out_pages(&mut self, keys: &[MappingSlotKey]) -> StarryResult<usize> {
        self.swap_out_pages_with_capacity(keys, SwapPage::available_pages())
    }

    fn swap_out_pages_with_capacity(
        &mut self,
        keys: &[MappingSlotKey],
        capacity: usize,
    ) -> StarryResult<usize> {
        // Reserve enough *potential* storage for each selected candidate
        // before revoking writes. Actual atomic reservations still happen at
        // compression, since another MM can consume this observed capacity.
        // A full pool must not turn every write into another futile fault.
        let keys = &keys[..keys.len().min(capacity)];
        if keys.is_empty() {
            return Ok(0);
        }
        let mut candidates = Vec::new();
        candidates
            .try_reserve(keys.len())
            .map_err(|_| StarryError::NoMemory)?;
        for &key in keys {
            if let Some(page) = self.swap_candidate(key)? {
                candidates.push((key, page));
            }
        }
        if candidates.is_empty() {
            return Ok(0);
        }
        // Freeze the whole bounded batch with one cross-CPU acknowledgement.
        self.protect_mapping_slots(&candidates)?;
        let mut pages = Vec::new();
        let mut stored = Vec::new();
        pages
            .try_reserve(candidates.len())
            .map_err(|_| StarryError::NoMemory)?;
        stored
            .try_reserve(candidates.len())
            .map_err(|_| StarryError::NoMemory)?;
        for (key, page) in candidates {
            // SAFETY: `page` retains an exclusive owned 4 KiB frame. The MM
            // mutex excludes fork and mapping changes; the batch receipt has
            // revoked every writable translation before this immutable read.
            // Shared mappings, anchored frames and extra page references were
            // excluded before collecting any candidate owners.
            let source = unsafe {
                core::slice::from_raw_parts(
                    phys_to_virt(page.frame().paddr()).as_ptr(),
                    PAGE_SIZE_4K,
                )
            };
            if let Some(content) = SwapPage::compress(source)? {
                pages.push((key, page));
                stored.push((key.va, content));
            }
        }
        // No swap entry is visible until all fallible compression succeeds.
        // The MM mutex prevents faults from seeing resident+swap preparation.
        self.swapped_pages.extend(stored);
        match self.evict_mapping_slots(&pages) {
            Ok(EvictMappingOutcome::Complete) => {
                PAGE_OUTS.fetch_add(pages.len(), Ordering::Relaxed);
                Ok(pages.len())
            }
            Ok(EvictMappingOutcome::PublishedPendingTlb) => {
                // Source frames remain quarantined until the shared receipt
                // completes. Refault services it before restoring contents.
                PAGE_OUTS.fetch_add(pages.len(), Ordering::Relaxed);
                Ok(0)
            }
            Ok(EvictMappingOutcome::NeedsRepair) => Err(StarryError::BadState),
            Err(error) => {
                if !self.mutation_gate.needs_repair() {
                    for (key, _) in pages {
                        self.swapped_pages.remove(&key.va);
                    }
                }
                Err(error)
            }
        }
    }

    /// Best-effort PAGEOUT; return the next cursor after at most 128 slots.
    pub(crate) fn pageout_anonymous_batch(
        &mut self,
        range: VirtAddrRange,
    ) -> StarryResult<VirtAddr> {
        let start = MappingSlotKey {
            space_id: self.id,
            va: range.start,
        };
        let end = MappingSlotKey {
            space_id: self.id,
            va: range.end,
        };
        let mut keys = Vec::new();
        keys.try_reserve(128).map_err(|_| StarryError::NoMemory)?;
        keys.extend(
            self.mapping_slots
                .range(start..end)
                .take(128)
                .map(|(&key, _)| key),
        );
        let cursor = if keys.len() == 128 {
            keys.last().ok_or(StarryError::BadState)?.va + PAGE_SIZE_4K
        } else {
            range.end
        };
        self.swap_out_pages(&keys)?;
        Ok(cursor)
    }
}

#[cfg(all(test, axtest))]
mod tests {
    use ax_runtime::hal::paging::MappingFlags;

    use super::{
        super::{HugePageAdvice, MappingOperation},
        *,
    };

    #[axtest::axtest]
    fn anonymous_swap_preserves_fork_move_discard_and_replacement() {
        // A moderately compressible page must use genuinely smaller backing
        // allocations. The 2 KiB slab class fits only one object per 4 KiB
        // slab, so accepting one such buffer would release no physical page.
        let mut source = [0; PAGE_SIZE_4K];
        let mut random = 0x4a32_8731u32;
        for byte in &mut source[..1536] {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            *byte = random as u8;
        }
        for index in 1536..source.len() {
            source[index] = source[index % 1536];
        }
        let stored = SwapPage::compress(&source).unwrap().unwrap();
        assert!(stored.compressed.iter().map(Vec::len).sum::<usize>() > 1024);
        let largest_allocation = stored.compressed.iter().map(Vec::capacity).max().unwrap();
        let mut restored = [0; PAGE_SIZE_4K];
        stored.read_into(&mut restored).unwrap();
        assert_eq!(restored, source);
        drop(stored);

        let base = VirtAddr::from(0x7200_0000usize);
        let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
        let mut parent = AddrSpace::new_empty(base, 16 * PAGE_SIZE_4K).unwrap();
        parent
            .map(
                base,
                4 * PAGE_SIZE_4K,
                flags,
                true,
                MappingOperation::new_alloc(base, PAGE_SIZE_4K, "[swap-test]"),
            )
            .unwrap();
        for number in 0..4 {
            parent
                .write(
                    base + number * PAGE_SIZE_4K,
                    &[number as u8 + 1; PAGE_SIZE_4K],
                )
                .unwrap();
        }
        let range = VirtAddrRange::from_start_size(base, 4 * PAGE_SIZE_4K);
        let keys: Vec<_> = parent.mapping_slots.keys().copied().collect();
        let evicted_without_capacity = parent.swap_out_pages_with_capacity(&keys, 0).unwrap();
        let unchanged_without_capacity = evicted_without_capacity == 0
            && parent.swapped_pages.is_empty()
            && parent.resident_pages.anon == 4
            && parent
                .pt
                .query(base)
                .is_ok_and(|(_, flags, _)| flags.contains(MappingFlags::WRITE));
        assert!(
            largest_allocation <= 1024 && unchanged_without_capacity,
            "swap backing/capacity contract: largest allocation={largest_allocation}, unchanged \
             without capacity={unchanged_without_capacity}"
        );
        // Fail write-protection publication, then eviction publication. Both
        // transactions must restore all four pages without publishing swap.
        parent.mutation_gate.fail_next_commit_before_publish();
        assert!(parent.pageout_anonymous_batch(range).is_err());
        assert!(parent.swapped_pages.is_empty());
        assert_eq!(parent.resident_pages.anon, 4);
        let pages: Vec<_> = parent
            .mapping_slots
            .iter()
            .map(|(&key, slot)| (key, slot.page.clone()))
            .collect();
        parent.protect_mapping_slots(&pages).unwrap();
        drop(pages);
        parent.mutation_gate.fail_next_commit_before_publish();
        assert!(parent.pageout_anonymous_batch(range).is_err());
        assert!(parent.swapped_pages.is_empty());
        assert_eq!(parent.resident_pages.anon, 4);
        for number in 0..4 {
            let mut observed = [0; PAGE_SIZE_4K];
            parent
                .read(base + number * PAGE_SIZE_4K, &mut observed)
                .unwrap();
            assert_eq!(observed, [number as u8 + 1; PAGE_SIZE_4K]);
        }
        assert_eq!(parent.swap_out_batch(16).unwrap(), 4);
        assert_eq!(parent.resident_pages.anon, 0);
        assert!(parent.mapping_slots.is_empty());
        parent.mutation_gate.fail_next_commit_before_publish();
        assert_eq!(
            parent.handle_page_fault_result(base, PageFaultFlags::READ),
            FaultResult::Retry
        );
        assert_eq!(parent.swapped_pages.len(), 4);
        assert!(parent.mapping_slots.is_empty());

        let child = parent.try_clone().unwrap();
        let mut child = child.lock();
        let target = base + 8 * PAGE_SIZE_4K;
        let source = child.mremap_source(base).unwrap();
        child
            .mremap_move_from_source(
                &source,
                base,
                4 * PAGE_SIZE_4K,
                target,
                4 * PAGE_SIZE_4K,
                HugePageAdvice::Default,
                false,
                0,
                false,
                None,
            )
            .unwrap();
        assert!(child.swapped_pages.get(&base).is_none());
        assert_eq!(child.swapped_pages.len(), 4);
        child
            .populate_area(target, 4 * PAGE_SIZE_4K, MappingFlags::READ)
            .unwrap();
        assert!(child.swapped_pages.is_empty());
        let mut observed = [0; PAGE_SIZE_4K];
        for number in 0..4 {
            child
                .read(target + number * PAGE_SIZE_4K, &mut observed)
                .unwrap();
            assert_eq!(observed, [number as u8 + 1; PAGE_SIZE_4K]);
        }
        child.write(target, &[0x55; PAGE_SIZE_4K]).unwrap();
        assert_eq!(
            parent.handle_page_fault_result(base, PageFaultFlags::READ),
            FaultResult::Handled
        );
        parent.read(base, &mut observed).unwrap();
        assert_eq!(observed, [1; PAGE_SIZE_4K]);
        parent
            .discard_range(base + PAGE_SIZE_4K, PAGE_SIZE_4K)
            .unwrap();
        parent
            .populate_area(base + PAGE_SIZE_4K, PAGE_SIZE_4K, MappingFlags::READ)
            .unwrap();
        parent.read(base + PAGE_SIZE_4K, &mut observed).unwrap();
        assert_eq!(observed, [0; PAGE_SIZE_4K]);
        parent
            .unmap(base + 2 * PAGE_SIZE_4K, 2 * PAGE_SIZE_4K)
            .unwrap();
        parent
            .map(
                base + 2 * PAGE_SIZE_4K,
                2 * PAGE_SIZE_4K,
                flags,
                true,
                MappingOperation::new_alloc(base + 2 * PAGE_SIZE_4K, PAGE_SIZE_4K, "[swap-reuse]"),
            )
            .unwrap();
        parent.read(base + 2 * PAGE_SIZE_4K, &mut observed).unwrap();
        assert_eq!(observed, [0; PAGE_SIZE_4K]);
        assert!(parent.swapped_pages.is_empty());
        child.reset_uninstalled_for_loader().unwrap();
        parent.reset_uninstalled_for_loader().unwrap();

        // A failed rollback must keep old translations' frames alive, even
        // after every caller-owned reference and resident slot is gone.
        for eviction in [false, true] {
            let mut space = AddrSpace::new_empty(base, 4 * PAGE_SIZE_4K).unwrap();
            space
                .map(
                    base,
                    PAGE_SIZE_4K,
                    flags,
                    true,
                    MappingOperation::new_alloc(base, PAGE_SIZE_4K, "[swap-rollback]"),
                )
                .unwrap();
            let pages: Vec<_> = space
                .mapping_slots
                .iter()
                .map(|(&key, slot)| (key, slot.page.clone()))
                .collect();
            let weak = Arc::downgrade(&pages[0].1);
            space.fail_restore_after_detach = true;
            if eviction {
                space.fail_eviction_after_unmap = true;
                assert!(matches!(
                    space.evict_mapping_slots(&pages),
                    Ok(EvictMappingOutcome::NeedsRepair)
                ));
            } else {
                space.mutation_gate.fail_next_commit_before_publish();
                assert!(space.protect_mapping_slots(&pages).is_err());
            }
            drop(pages);
            assert!(space.mapping_slots.is_empty());
            assert!(space.mutation_gate.needs_repair());
            assert!(
                weak.upgrade().is_some(),
                "rollback released a frame before TLB acknowledgement"
            );
            // This test MM was never installed; acknowledge its empty target
            // set before releasing the quarantined frames for test cleanup.
            let epoch = space.mutation_gate.current_epoch().checked_next().unwrap();
            space
                .confirm_restored_mapping_owners(
                    epoch,
                    &[VirtAddrRange::from_start_size(base, PAGE_SIZE_4K)],
                )
                .unwrap();
            assert!(weak.upgrade().is_none());
            space.mutation_gate.clear_repair();
            space.reset_uninstalled_for_loader().unwrap();
        }
    }
}
