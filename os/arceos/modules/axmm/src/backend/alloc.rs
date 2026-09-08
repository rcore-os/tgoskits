use alloc::{sync::Arc, vec::Vec};
use core::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use ax_alloc::{UsageKind, global_allocator};
use ax_hal::{
    mem::{phys_to_virt, virt_to_phys},
    paging::{MappingFlags, PageTable, PageTableEntry, PagingError},
};
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PageIter4K, PhysAddr, VirtAddr};

use super::Backend;
use crate::tlb::TlbGather;

static NEXT_KERNEL_VIRTUAL_ALLOCATION_ID: AtomicU64 = AtomicU64::new(1);

/// Stable identity of one kernel virtual allocation.
///
/// The identity prevents a stale quarantine retry from acting on a newly
/// allocated mapping that happens to reuse the same virtual range.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct KernelVirtualAllocationId(u64);

impl KernelVirtualAllocationId {
    fn allocate() -> Option<Self> {
        NEXT_KERNEL_VIRTUAL_ALLOCATION_ID
            .try_update(Ordering::AcqRel, Ordering::Acquire, |next| {
                next.checked_add(1)
            })
            .ok()
            .map(Self)
    }

    #[cfg(test)]
    pub(crate) const fn for_test(raw: u64) -> Self {
        Self(raw)
    }
}

struct KernelVirtualFrameSet {
    frames: Vec<PhysAddr>,
    usage: UsageKind,
}

impl Drop for KernelVirtualFrameSet {
    fn drop(&mut self) {
        for frame in self.frames.drain(..) {
            dealloc_frame_with_usage(frame, self.usage);
        }
    }
}

struct KernelVirtualFrameBuilder {
    frames: Vec<PhysAddr>,
    usage: UsageKind,
}

impl KernelVirtualFrameBuilder {
    fn allocate(page_count: usize, usage: UsageKind) -> Option<Self> {
        if page_count == 0 {
            return None;
        }
        let mut frames = Vec::new();
        frames.try_reserve_exact(page_count).ok()?;
        let mut builder = Self { frames, usage };
        for _ in 0..page_count {
            builder.frames.push(alloc_frame(true, usage)?);
        }
        Some(builder)
    }

    fn finish(mut self) -> Vec<PhysAddr> {
        core::mem::take(&mut self.frames)
    }
}

impl Drop for KernelVirtualFrameBuilder {
    fn drop(&mut self) {
        for frame in self.frames.drain(..) {
            dealloc_frame_with_usage(frame, self.usage);
        }
    }
}

/// Cloneable metadata owner for one page-backed kernel virtual allocation.
///
/// The page table is only a materialized view. Backing frames remain owned by
/// this object across partial PTE detach, TLB quarantine, and retry; the final
/// metadata reference releases them after the range is retired.
#[derive(Clone)]
pub struct KernelVirtualAllocationBackend {
    id: KernelVirtualAllocationId,
    frames: Arc<KernelVirtualFrameSet>,
    leading_guard_pages: usize,
    state: super::KernelVirtualAllocationState,
}

impl fmt::Debug for KernelVirtualAllocationBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KernelVirtualAllocationBackend")
            .field("id", &self.id)
            .field("frame_count", &self.frames.frames.len())
            .field("usage", &self.frames.usage)
            .field("leading_guard_pages", &self.leading_guard_pages)
            .field("state", &self.state)
            .finish()
    }
}

impl KernelVirtualAllocationBackend {
    pub(crate) fn allocate(
        usage: UsageKind,
        leading_guard_pages: usize,
        page_count: usize,
    ) -> Option<Self> {
        let id = KernelVirtualAllocationId::allocate()?;
        let builder = KernelVirtualFrameBuilder::allocate(page_count, usage)?;
        Some(Self {
            id,
            frames: Arc::new(KernelVirtualFrameSet {
                frames: builder.finish(),
                usage,
            }),
            leading_guard_pages,
            state: super::KernelVirtualAllocationState::Live,
        })
    }

    pub(crate) const fn id(&self) -> KernelVirtualAllocationId {
        self.id
    }

    pub(crate) const fn state(&self) -> super::KernelVirtualAllocationState {
        self.state
    }

    pub(crate) const fn leading_guard_pages(&self) -> usize {
        self.leading_guard_pages
    }

    pub(crate) fn with_state(&self, state: super::KernelVirtualAllocationState) -> Self {
        Self {
            id: self.id,
            frames: self.frames.clone(),
            leading_guard_pages: self.leading_guard_pages,
            state,
        }
    }

    pub(crate) fn expected_frame(&self, page_index: usize) -> Option<PhysAddr> {
        self.frames.frames.get(page_index).copied()
    }

    pub(crate) fn frame_count(&self) -> usize {
        self.frames.frames.len()
    }
}

fn alloc_frame(zeroed: bool, usage: UsageKind) -> Option<PhysAddr> {
    let vaddr = VirtAddr::from(
        global_allocator()
            .alloc_pages(1, PAGE_SIZE_4K, usage)
            .ok()?,
    );
    if zeroed {
        unsafe { core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, PAGE_SIZE_4K) };
    }
    let paddr = virt_to_phys(vaddr);
    Some(paddr)
}

pub(crate) fn dealloc_frame(frame: PhysAddr) {
    dealloc_frame_with_usage(frame, UsageKind::VirtMem);
}

fn dealloc_frame_with_usage(frame: PhysAddr, usage: UsageKind) {
    let vaddr = phys_to_virt(frame);
    global_allocator().dealloc_pages(vaddr.as_usize(), 1, usage);
}

impl Backend {
    /// Creates a new allocation mapping backend.
    pub const fn new_alloc(populate: bool) -> Self {
        Self::Alloc { populate }
    }

    pub(crate) fn map_alloc(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        gather: &mut TlbGather,
        pt: &mut PageTable,
        populate: bool,
    ) -> bool {
        debug!(
            "map_alloc: [{:#x}, {:#x}) {:?} (populate={})",
            start,
            start + size,
            flags,
            populate
        );
        if populate {
            let page_count = size / PAGE_SIZE_4K;
            if gather.prepare_deferred_frames(page_count).is_err()
                || gather.prepare_page_table_reclaims(page_count).is_err()
            {
                return false;
            }
            let mut populate = PageTablePopulate {
                page_table: pt,
                flags,
                gather,
                usage: UsageKind::VirtMem,
            };
            populate_pages(&mut populate, start, size)
        } else {
            // Map to a empty entry for on-demand mapping.
            let flags = MappingFlags::empty();
            pt.map_region(start, |_| 0.into(), size, flags).is_ok()
        }
    }

    pub(crate) fn unmap_alloc(
        &self,
        start: VirtAddr,
        size: usize,
        gather: &mut TlbGather,
        pt: &mut PageTable,
        populate: bool,
    ) -> bool {
        debug!("unmap_alloc: [{:#x}, {:#x})", start, start + size);
        let mut mapped = alloc::vec::Vec::new();
        for addr in PageIter4K::new(start, start + size).unwrap() {
            match pt.query_occupied(addr) {
                Ok((entry, 1)) => {
                    let frame = entry.paddr(false);
                    let owns_frame = populate || frame.as_usize() != 0;
                    mapped.push((addr, frame, owns_frame));
                }
                Ok(_) => return false,
                Err(PagingError::NotMapped) => {}
                Err(_) => return false,
            }
        }
        let owned_frames = mapped
            .iter()
            .filter(|(_, _, owns_frame)| *owns_frame)
            .count();
        if gather.prepare_deferred_frames(owned_frames).is_err()
            || gather.prepare_page_table_reclaims(mapped.len()).is_err()
        {
            return false;
        }
        for (addr, frame, owns_frame) in mapped {
            let (_, _, _, deferred_page_tables) = pt
                .unmap_page_deferred(addr)
                .expect("a preflighted allocated page must remain mapped under the aspace lock");
            gather.defer_page_tables(deferred_page_tables);
            if owns_frame {
                gather.defer_frame(frame);
            }
        }
        gather.invalidate(start, size);
        true
    }

    pub(crate) fn handle_page_fault_alloc(
        &self,
        vaddr: VirtAddr,
        orig_flags: MappingFlags,
        gather: &mut TlbGather,
        pt: &mut PageTable,
        populate: bool,
    ) -> bool {
        if populate {
            false // Populated mappings should not trigger page faults.
        } else {
            // Allocate a physical frame lazily and map it to the fault address.
            remap_frame_or_dealloc(
                alloc_frame(true, UsageKind::VirtMem),
                |frame| pt.remap_page(vaddr, frame, orig_flags).is_ok(),
                dealloc_frame,
                |_| gather.invalidate(vaddr.align_down_4k(), PAGE_SIZE_4K),
            )
        }
    }

    pub(crate) fn unmap_kernel_virtual_allocation(
        &self,
        start: VirtAddr,
        size: usize,
        pt: &mut PageTable,
    ) -> bool {
        let Some(allocation) = self.kernel_virtual_allocation() else {
            return false;
        };
        let Some((mapped_start, mapped_size)) =
            kernel_virtual_mapped_range(start, size, allocation.leading_guard_pages())
        else {
            return false;
        };
        detach_kernel_virtual_frames(allocation, mapped_start, mapped_size, pt)
    }

    pub(crate) fn validate_kernel_virtual_allocation(
        &self,
        start: VirtAddr,
        size: usize,
        pt: &PageTable,
    ) -> bool {
        let Some(allocation) = self.kernel_virtual_allocation() else {
            return false;
        };
        let Some((mapped_start, mapped_size)) =
            kernel_virtual_mapped_range(start, size, allocation.leading_guard_pages())
        else {
            return false;
        };
        let Some(mapped_end) = mapped_start.checked_add(mapped_size) else {
            return false;
        };
        let Some(pages) = PageIter4K::new(mapped_start, mapped_end) else {
            return false;
        };
        if mapped_size / PAGE_SIZE_4K != allocation.frame_count() {
            return false;
        }
        let allow_detached = allocation.state() == super::KernelVirtualAllocationState::Quarantined;
        pages.into_iter().enumerate().all(|(index, addr)| {
            let Some(expected) = allocation.expected_frame(index) else {
                return false;
            };
            match pt.query_occupied(addr) {
                Ok((pte, level)) => level == 1 && pte.paddr(false) == expected,
                Err(PagingError::NotMapped) => allow_detached,
                Err(_) => false,
            }
        })
    }
}

pub(crate) fn kernel_virtual_mapped_range(
    start: VirtAddr,
    size: usize,
    leading_guard_pages: usize,
) -> Option<(VirtAddr, usize)> {
    let guard_size = leading_guard_pages.checked_mul(PAGE_SIZE_4K)?;
    let mapped_size = size.checked_sub(guard_size)?;
    if mapped_size == 0 {
        return None;
    }
    Some((start.checked_add(guard_size)?, mapped_size))
}

fn detach_kernel_virtual_frames(
    allocation: &KernelVirtualAllocationBackend,
    start: VirtAddr,
    size: usize,
    pt: &mut PageTable,
) -> bool {
    let Some(end) = start.checked_add(size) else {
        return false;
    };
    let Some(pages) = PageIter4K::new(start, end) else {
        return false;
    };
    if size / PAGE_SIZE_4K != allocation.frame_count() {
        return false;
    }

    let mut complete = true;
    for (index, addr) in pages.into_iter().enumerate() {
        let Some(expected) = allocation.expected_frame(index) else {
            complete = false;
            continue;
        };
        match pt.query_occupied(addr) {
            Ok((pte, level)) if level == 1 && pte.paddr(false) == expected => {
                match pt
                    .plan_unmap_page(addr)
                    .and_then(|plan| pt.try_unmap_page_with(plan))
                {
                    Ok((detached, _, page_size))
                        if detached == expected && page_size == PAGE_SIZE_4K => {}
                    _ => complete = false,
                }
            }
            Err(PagingError::NotMapped) => {}
            _ => complete = false,
        }
    }
    complete
}

fn remap_frame_or_dealloc(
    frame: Option<PhysAddr>,
    remap_frame: impl FnOnce(PhysAddr) -> bool,
    dealloc_frame: impl FnOnce(PhysAddr),
    publish_frame: impl FnOnce(PhysAddr),
) -> bool {
    let Some(frame) = frame else {
        return false;
    };
    if remap_frame(frame) {
        publish_frame(frame);
        true
    } else {
        dealloc_frame(frame);
        false
    }
}

trait PopulatePageOps {
    fn alloc_frame(&mut self) -> Option<PhysAddr>;

    fn map_frame(&mut self, addr: VirtAddr, frame: PhysAddr) -> bool;

    fn unmap_frame(&mut self, addr: VirtAddr) -> Option<PhysAddr>;

    /// Releases a frame that was never reachable through a published PTE.
    fn release_unmapped_frame(&mut self, frame: PhysAddr);

    /// Retains a frame removed from a PTE until shootdown confirmation.
    fn defer_unmapped_frame(&mut self, frame: PhysAddr);

    /// Records the published prefix removed by a failed populate operation.
    fn invalidate_published_range(&mut self, _start: VirtAddr, _size: usize) {}
}

struct PageTablePopulate<'a> {
    page_table: &'a mut PageTable,
    flags: MappingFlags,
    gather: &'a mut TlbGather,
    usage: UsageKind,
}

impl PopulatePageOps for PageTablePopulate<'_> {
    fn alloc_frame(&mut self) -> Option<PhysAddr> {
        alloc_frame(true, self.usage)
    }

    fn map_frame(&mut self, addr: VirtAddr, frame: PhysAddr) -> bool {
        self.page_table
            .map_page(addr, frame, PAGE_SIZE_4K, self.flags)
            .is_ok()
    }

    fn unmap_frame(&mut self, addr: VirtAddr) -> Option<PhysAddr> {
        let (frame, _, page_size, deferred_page_tables) =
            self.page_table.unmap_page_deferred(addr).ok()?;
        self.gather.defer_page_tables(deferred_page_tables);
        assert_eq!(
            page_size, PAGE_SIZE_4K,
            "a populated 4K mapping must be rolled back as a 4K page"
        );
        Some(frame)
    }

    fn release_unmapped_frame(&mut self, frame: PhysAddr) {
        dealloc_frame(frame);
    }

    fn defer_unmapped_frame(&mut self, frame: PhysAddr) {
        self.gather.defer_frame(frame);
    }

    fn invalidate_published_range(&mut self, start: VirtAddr, size: usize) {
        self.gather.invalidate(start, size);
    }
}

fn populate_pages(ops: &mut impl PopulatePageOps, start: VirtAddr, size: usize) -> bool {
    let Some(end) = start.checked_add(size) else {
        return false;
    };
    let Some(pages) = PageIter4K::new(start, end) else {
        return false;
    };
    for addr in pages {
        let Some(frame) = ops.alloc_frame() else {
            rollback_populated_pages(ops, start, addr);
            return false;
        };
        if !ops.map_frame(addr, frame) {
            ops.release_unmapped_frame(frame);
            rollback_populated_pages(ops, start, addr);
            return false;
        }
    }
    true
}

fn rollback_populated_pages(
    ops: &mut impl PopulatePageOps,
    start: VirtAddr,
    mapped_end: VirtAddr,
) -> bool {
    let Some(pages) = PageIter4K::new(start, mapped_end) else {
        return false;
    };
    let mut complete = true;
    for addr in pages {
        if let Some(frame) = ops.unmap_frame(addr) {
            ops.defer_unmapped_frame(frame);
        } else {
            complete = false;
        }
    }
    if mapped_end > start {
        ops.invalidate_published_range(start, mapped_end - start);
    }
    complete
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::cell::Cell;

    use super::*;

    const START: usize = 0x4000_0000;

    #[test]
    fn rolls_back_current_and_mapped_frames_when_map_fails() {
        let start = VirtAddr::from(START);
        let mut ops = MockPopulatePageOps::with_map_failure(start + PAGE_SIZE_4K);

        assert!(!populate_pages(&mut ops, start, 3 * PAGE_SIZE_4K));
        assert!(ops.mapped.is_empty());
        assert_eq!(ops.deallocated.len(), 2);
        assert!(ops.deallocated.contains(&PhysAddr::from(PAGE_SIZE_4K)));
        assert!(ops.deallocated.contains(&PhysAddr::from(2 * PAGE_SIZE_4K)));
        assert_eq!(ops.invalidated, [(start, PAGE_SIZE_4K)]);
    }

    #[test]
    fn rolls_back_mapped_frames_when_allocation_fails() {
        let start = VirtAddr::from(START);
        let mut ops = MockPopulatePageOps::with_allocation_limit(1);

        assert!(!populate_pages(&mut ops, start, 3 * PAGE_SIZE_4K));
        assert!(ops.mapped.is_empty());
        assert_eq!(ops.deallocated, [PhysAddr::from(PAGE_SIZE_4K)]);
        assert_eq!(ops.invalidated, [(start, PAGE_SIZE_4K)]);
    }

    #[test]
    fn rollback_does_not_panic_when_a_mapped_leaf_cannot_be_detached() {
        let start = VirtAddr::from(START);
        let mut ops = MockPopulatePageOps::with_unmap_failure(start + PAGE_SIZE_4K);
        assert!(ops.map_frame(start, PhysAddr::from(PAGE_SIZE_4K)));
        assert!(ops.map_frame(start + PAGE_SIZE_4K, PhysAddr::from(2 * PAGE_SIZE_4K)));

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rollback_populated_pages(&mut ops, start, start + 2 * PAGE_SIZE_4K)
        }));

        assert!(
            matches!(outcome, Ok(false)),
            "a recoverable rollback failure must not panic or claim success"
        );
    }

    #[test]
    fn keeps_lazy_frame_when_remap_succeeds() {
        let frame = PhysAddr::from(PAGE_SIZE_4K);
        let deallocated = Cell::new(None);
        let published = Cell::new(None);

        assert!(remap_frame_or_dealloc(
            Some(frame),
            |_| true,
            |frame| deallocated.set(Some(frame)),
            |frame| published.set(Some(frame)),
        ));
        assert_eq!(deallocated.get(), None);
        assert_eq!(published.get(), Some(frame));
    }

    #[test]
    fn deallocates_lazy_frame_when_remap_fails() {
        let frame = PhysAddr::from(PAGE_SIZE_4K);
        let deallocated = Cell::new(None);

        assert!(!remap_frame_or_dealloc(
            Some(frame),
            |_| false,
            |frame| deallocated.set(Some(frame)),
            |_| unreachable!("failed remap must not publish its frame"),
        ));
        assert_eq!(deallocated.get(), Some(frame));
    }

    struct MockPopulatePageOps {
        allocations: usize,
        allocation_limit: Option<usize>,
        map_failure: Option<VirtAddr>,
        unmap_failure: Option<VirtAddr>,
        mapped: Vec<(VirtAddr, PhysAddr)>,
        deallocated: Vec<PhysAddr>,
        invalidated: Vec<(VirtAddr, usize)>,
    }

    impl MockPopulatePageOps {
        fn with_map_failure(addr: VirtAddr) -> Self {
            Self {
                allocations: 0,
                allocation_limit: None,
                map_failure: Some(addr),
                unmap_failure: None,
                mapped: Vec::new(),
                deallocated: Vec::new(),
                invalidated: Vec::new(),
            }
        }

        fn with_allocation_limit(limit: usize) -> Self {
            Self {
                allocations: 0,
                allocation_limit: Some(limit),
                map_failure: None,
                unmap_failure: None,
                mapped: Vec::new(),
                deallocated: Vec::new(),
            }
        }

        fn with_unmap_failure(addr: VirtAddr) -> Self {
            Self {
                allocations: 0,
                allocation_limit: None,
                map_failure: None,
                unmap_failure: Some(addr),
                mapped: Vec::new(),
                deallocated: Vec::new(),
                invalidated: Vec::new(),
            }
        }
    }

    impl PopulatePageOps for MockPopulatePageOps {
        fn alloc_frame(&mut self) -> Option<PhysAddr> {
            if self.allocation_limit == Some(self.allocations) {
                return None;
            }
            self.allocations += 1;
            Some(PhysAddr::from(self.allocations * PAGE_SIZE_4K))
        }

        fn map_frame(&mut self, addr: VirtAddr, frame: PhysAddr) -> bool {
            if self.map_failure == Some(addr) {
                return false;
            }
            self.mapped.push((addr, frame));
            true
        }

        fn unmap_frame(&mut self, addr: VirtAddr) -> Option<PhysAddr> {
            if self.unmap_failure == Some(addr) {
                return None;
            }
            let index = self
                .mapped
                .iter()
                .position(|(mapped_addr, _)| *mapped_addr == addr)?;
            Some(self.mapped.remove(index).1)
        }

        fn release_unmapped_frame(&mut self, frame: PhysAddr) {
            self.deallocated.push(frame);
        }

        fn defer_unmapped_frame(&mut self, frame: PhysAddr) {
            self.deallocated.push(frame);
        }

        fn invalidate_published_range(&mut self, start: VirtAddr, size: usize) {
            self.invalidated.push((start, size));
        }
    }
}
