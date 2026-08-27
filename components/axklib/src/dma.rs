use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr};
use dma_api::{
    DeviceDma, DmaAllocHandle, DmaConstraints, DmaDeviceInfo, DmaDirection, DmaError, DmaMapHandle,
    DmaOp,
};
use mbarrier::mb;

use crate::DmaCoherentMappingOutcome;

pub struct KlibDma;

static DMA: KlibDma = KlibDma;

pub fn op() -> &'static KlibDma {
    &DMA
}

pub fn device(info: DmaDeviceInfo) -> DeviceDma {
    DeviceDma::new(info, op())
}

struct DmaPages {
    cpu_addr: NonNull<u8>,
    dma_addr: u64,
    num_pages: usize,
    state: DmaPagesState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DmaPagesState {
    Owned,
    Transferred,
    Quarantined,
}

impl DmaPagesState {
    const fn releases_pages_on_drop(self) -> bool {
        matches!(self, Self::Owned)
    }
}

impl DmaPages {
    fn empty() -> Self {
        let (cpu_addr, dma_addr, num_pages) = empty_dma_page_parts();
        Self {
            cpu_addr,
            dma_addr,
            num_pages,
            state: DmaPagesState::Owned,
        }
    }

    fn layout_pages(layout: Layout) -> usize {
        layout.size().div_ceil(PAGE_SIZE_4K)
    }

    fn layout_align(layout: Layout, constraints: DmaConstraints) -> usize {
        layout.align().max(constraints.align).max(PAGE_SIZE_4K)
    }

    /// Allocates DMA-visible pages using the kernel DMA allocator.
    ///
    /// `dma_alloc_pages` is expected to honor `addr_mask` and the requested
    /// alignment. The checks below are defensive validation so a bad platform
    /// allocator fails before the buffer is handed to a device.
    fn alloc_for_layout(constraints: DmaConstraints, layout: Layout) -> Result<Self, DmaError> {
        if layout.size() == 0 {
            return Ok(Self::empty());
        }

        let num_pages = Self::layout_pages(layout);
        let align = Self::layout_align(layout, constraints);
        let cpu_addr = crate::klib::dma_alloc_pages(constraints.addr_mask, num_pages, align)
            .map_err(|_| DmaError::NoMemory)?;
        let dma_addr = dma_addr_from_ptr(cpu_addr);
        let pages = Self {
            cpu_addr,
            dma_addr,
            num_pages,
            state: DmaPagesState::Owned,
        };

        if !dma_range_fits_mask(dma_addr, layout.size(), constraints.addr_mask) {
            return Err(DmaError::DmaMaskNotMatch {
                addr: dma_addr.into(),
                mask: constraints.addr_mask,
            });
        }
        if !dma_addr_is_aligned(dma_addr, constraints.align.max(layout.align())) {
            return Err(DmaError::AlignMismatch {
                required: constraints.align.max(layout.align()),
                address: dma_addr.into(),
            });
        }

        Ok(pages)
    }

    /// # Safety
    ///
    /// `cpu_addr` and `num_pages` must describe a live allocation returned by
    /// the kernel DMA page allocator, and no published handle may still own it.
    unsafe fn dealloc_pages(cpu_addr: NonNull<u8>, num_pages: usize) {
        if num_pages == 0 {
            return;
        }
        crate::klib::dma_dealloc_pages(cpu_addr, num_pages);
    }

    fn into_contiguous_handle(mut self, layout: Layout) -> DmaAllocHandle {
        self.state = DmaPagesState::Transferred;
        // SAFETY: `self` owns the live allocation and its matching DMA address
        // until this method transfers both values into the handle.
        unsafe { DmaAllocHandle::new(self.cpu_addr, self.cpu_addr, self.dma_addr.into(), layout) }
    }

    /// Transfers the allocator pages to a coherent handle using `alias` for CPU access.
    ///
    /// # Safety
    ///
    /// `alias` must map the same physical pages as `self.cpu_addr` for the
    /// complete `layout` and must remain valid until coherent deallocation.
    unsafe fn into_coherent_handle(mut self, alias: NonNull<u8>, layout: Layout) -> DmaAllocHandle {
        self.state = DmaPagesState::Transferred;
        unsafe { DmaAllocHandle::new(alias, self.cpu_addr, self.dma_addr.into(), layout) }
    }

    fn into_bounce_parts(mut self) -> (NonNull<u8>, u64) {
        self.state = DmaPagesState::Transferred;
        (self.cpu_addr, self.dma_addr)
    }

    fn quarantine(mut self) {
        self.state = DmaPagesState::Quarantined;
    }
}

fn empty_dma_page_parts() -> (NonNull<u8>, u64, usize) {
    (NonNull::dangling(), 0, 0)
}

impl Drop for DmaPages {
    fn drop(&mut self) {
        if self.state.releases_pages_on_drop() {
            // SAFETY: the guard still owns exactly the pages returned by the
            // kernel DMA allocator and has not published them in a handle.
            unsafe { Self::dealloc_pages(self.cpu_addr, self.num_pages) };
        }
    }
}

struct CoherentDmaPolicy;

impl CoherentDmaPolicy {
    fn map_uncached(pages: &DmaPages) -> DmaCoherentMappingOutcome {
        if pages.num_pages == 0 {
            return DmaCoherentMappingOutcome::Mapped(pages.cpu_addr);
        }

        let range_size = pages.num_pages * PAGE_SIZE_4K;
        crate::klib::mem_map_dma_coherent_uncached(pages.cpu_addr, range_size)
    }

    fn unmap_alias(alias: NonNull<u8>, num_pages: usize) -> Result<(), DmaError> {
        if num_pages == 0 {
            return Ok(());
        }

        crate::klib::mem_unmap_dma_coherent(alias, num_pages * PAGE_SIZE_4K)
            .map_err(|_| DmaError::NoMemory)
    }
}

fn release_coherent_pages(
    unmap_alias: impl FnOnce() -> Result<(), DmaError>,
    dealloc_pages: impl FnOnce(),
) -> Result<(), DmaError> {
    unmap_alias()?;
    dealloc_pages();
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoherentMappingFailure {
    Reclaim,
    Quarantine,
}

fn finish_coherent_mapping(
    outcome: DmaCoherentMappingOutcome,
) -> Result<NonNull<u8>, CoherentMappingFailure> {
    match outcome {
        DmaCoherentMappingOutcome::Mapped(alias) => Ok(alias),
        DmaCoherentMappingOutcome::NotStarted(_) => Err(CoherentMappingFailure::Reclaim),
        // The PTE update may already be visible on only part of the CPU set.
        // Returning these pages to the allocator could let cached and uncached
        // aliases race with a new owner, so quarantine them permanently.
        DmaCoherentMappingOutcome::StateUncertain(_) => Err(CoherentMappingFailure::Quarantine),
    }
}

/// # Safety
///
/// The handle's CPU pointer must be writable for its full layout and must not
/// yet be observable by another owner.
unsafe fn initialize_coherent_handle(handle: DmaAllocHandle) -> DmaAllocHandle {
    unsafe { handle.as_ptr().write_bytes(0, handle.size()) };
    handle
}

impl DmaOp for KlibDma {
    fn page_size(&self) -> usize {
        PAGE_SIZE_4K
    }

    unsafe fn alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        let pages = DmaPages::alloc_for_layout(constraints, layout).ok()?;
        Some(pages.into_contiguous_handle(layout))
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        let num_pages = DmaPages::layout_pages(handle.layout());
        unsafe { DmaPages::dealloc_pages(handle.as_ptr(), num_pages) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        let pages = DmaPages::alloc_for_layout(constraints, layout).ok()?;
        let alias = match finish_coherent_mapping(CoherentDmaPolicy::map_uncached(&pages)) {
            Ok(alias) => alias,
            Err(CoherentMappingFailure::Reclaim) => return None,
            Err(CoherentMappingFailure::Quarantine) => {
                pages.quarantine();
                return None;
            }
        };

        let handle = unsafe { pages.into_coherent_handle(alias, layout) };
        Some(unsafe { initialize_coherent_handle(handle) })
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        let num_pages = DmaPages::layout_pages(handle.layout());
        release_coherent_pages(
            || {
                CoherentDmaPolicy::unmap_alias(handle.as_ptr(), num_pages)
                    .map_err(|_| DmaError::CoherentReleaseFailed)
            },
            || unsafe { DmaPages::dealloc_pages(handle.allocation_ptr(), num_pages) },
        )
    }

    unsafe fn map_streaming(
        &self,
        constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let align = constraints.align.max(1);
        let layout = Layout::from_size_align(size.get(), align)?;
        let dma_addr = dma_addr_from_ptr(addr);

        if dma_mapping_can_be_direct(dma_addr, size.get(), constraints) {
            return Ok(unsafe { DmaMapHandle::new(addr, dma_addr.into(), layout, None) });
        }

        let map_pages = DmaPages::alloc_for_layout(constraints, layout)?;
        let (bounce_ptr, bounce_dma_addr) = map_pages.into_bounce_parts();
        Ok(unsafe { DmaMapHandle::new(addr, bounce_dma_addr.into(), layout, Some(bounce_ptr)) })
    }

    unsafe fn unmap_streaming(&self, handle: DmaMapHandle) {
        if let Some(map_virt) = handle.bounce_ptr() {
            let num_pages = DmaPages::layout_pages(handle.layout());
            unsafe { DmaPages::dealloc_pages(map_virt, num_pages) };
        }
    }

    fn flush(&self, addr: NonNull<u8>, size: usize) {
        mb();
        crate::klib::dma_cache_clean(VirtAddr::from_usize(addr.as_ptr() as usize), size);
    }

    fn invalidate(&self, addr: NonNull<u8>, size: usize) {
        crate::klib::dma_cache_invalidate(VirtAddr::from_usize(addr.as_ptr() as usize), size);
        mb();
    }

    fn flush_invalidate(&self, addr: NonNull<u8>, size: usize) {
        mb();
        crate::klib::dma_cache_clean_invalidate(VirtAddr::from_usize(addr.as_ptr() as usize), size);
        mb();
    }
}

fn dma_addr_from_ptr(ptr: NonNull<u8>) -> u64 {
    crate::klib::mem_virt_to_phys(VirtAddr::from_usize(ptr.as_ptr() as usize)).as_usize() as u64
}

fn dma_range_fits_mask(dma_addr: u64, size: usize, dma_mask: u64) -> bool {
    if size == 0 {
        dma_addr <= dma_mask
    } else {
        dma_addr
            .checked_add(size.saturating_sub(1) as u64)
            .map(|end| end <= dma_mask)
            .unwrap_or(false)
    }
}

fn dma_addr_is_aligned(dma_addr: u64, align: usize) -> bool {
    dma_addr.is_multiple_of(align.max(1) as u64)
}

fn dma_mapping_can_be_direct(dma_addr: u64, size: usize, constraints: DmaConstraints) -> bool {
    let align = constraints.align.max(1);
    // A direct streaming mapping transfers cache-line ownership to the device.
    // Keep both ends aligned so unrelated heap objects cannot share that range.
    dma_range_fits_mask(dma_addr, size, constraints.addr_mask)
        && dma_addr_is_aligned(dma_addr, align)
        && size.is_multiple_of(align)
}

#[cfg(test)]
mod tests {
    use alloc::{rc::Rc, vec::Vec};
    use core::cell::RefCell;

    use super::*;
    use crate::KlibError;

    #[test]
    fn coherent_release_unmaps_alias_before_freeing_original_pages() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let unmap_events = events.clone();
        let free_events = events.clone();

        let result = release_coherent_pages(
            move || {
                unmap_events.borrow_mut().push("unmap_alias");
                Ok(())
            },
            move || free_events.borrow_mut().push("free"),
        );

        assert_eq!(result, Ok(()));
        assert_eq!(*events.borrow(), ["unmap_alias", "free"]);
    }

    #[test]
    fn coherent_release_quarantines_pages_when_alias_unmap_fails() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let unmap_events = events.clone();
        let free_events = events.clone();

        let result = release_coherent_pages(
            move || {
                unmap_events.borrow_mut().push("unmap_alias");
                Err(DmaError::CoherentReleaseFailed)
            },
            move || free_events.borrow_mut().push("free"),
        );

        assert_eq!(result, Err(DmaError::CoherentReleaseFailed));
        assert_eq!(*events.borrow(), ["unmap_alias"]);
    }

    #[test]
    fn coherent_mapping_marks_not_started_pages_as_reclaimable() {
        let result = finish_coherent_mapping(DmaCoherentMappingOutcome::NotStarted(
            KlibError::Unsupported,
        ));

        assert_eq!(result, Err(CoherentMappingFailure::Reclaim));
    }

    #[test]
    fn coherent_mapping_marks_uncertain_pages_for_quarantine() {
        let result = finish_coherent_mapping(DmaCoherentMappingOutcome::StateUncertain(
            KlibError::TimedOut,
        ));

        assert_eq!(result, Err(CoherentMappingFailure::Quarantine));
    }

    #[test]
    fn dma_page_guard_only_releases_owned_pages() {
        assert!(DmaPagesState::Owned.releases_pages_on_drop());
        assert!(!DmaPagesState::Transferred.releases_pages_on_drop());
        assert!(!DmaPagesState::Quarantined.releases_pages_on_drop());
    }

    #[test]
    fn coherent_allocation_publishes_the_independent_cpu_alias() {
        let alias = NonNull::new(0x8000 as *mut u8).unwrap();
        let mapped = finish_coherent_mapping(DmaCoherentMappingOutcome::Mapped(alias));

        assert_eq!(mapped, Ok(alias));
    }

    #[test]
    fn coherent_allocation_is_zeroed_through_the_published_alias() {
        let mut allocation = [0_u8; 8];
        let mut alias_bytes = [0xa5_u8; 8];
        let allocation_ptr = NonNull::from_mut(&mut allocation[0]);
        let alias = NonNull::from_mut(&mut alias_bytes[0]);
        let layout = Layout::from_size_align(alias_bytes.len(), 1).unwrap();
        let handle =
            unsafe { DmaAllocHandle::new(alias, allocation_ptr, 0x1000_u64.into(), layout) };

        let handle = unsafe { initialize_coherent_handle(handle) };

        assert_eq!(handle.as_ptr(), alias);
        assert_eq!(alias_bytes, [0; 8]);
    }

    #[test]
    fn zero_length_dma_pages_use_a_dangling_cpu_pointer_and_zero_dma_address() {
        let layout = Layout::from_size_align(0, 1).unwrap();
        let (cpu_addr, dma_addr, num_pages) = empty_dma_page_parts();

        assert_eq!(cpu_addr, NonNull::dangling());
        assert_eq!(dma_addr, 0);
        assert_eq!(num_pages, 0);

        let handle = unsafe { DmaAllocHandle::new(cpu_addr, cpu_addr, dma_addr.into(), layout) };
        assert_eq!(handle.as_ptr(), NonNull::dangling());
        assert_eq!(handle.dma_addr().as_u64(), 0);
        assert_eq!(handle.size(), 0);
    }

    #[test]
    fn zero_device_dma_address_remains_a_valid_numerical_address() {
        assert!(dma_range_fits_mask(0, PAGE_SIZE_4K, u64::MAX));
        assert!(dma_addr_is_aligned(0, PAGE_SIZE_4K));
    }

    #[test]
    fn direct_streaming_mapping_requires_an_isolated_aligned_range() {
        let constraints = DmaConstraints::new(u32::MAX as u64).with_align(64);

        assert!(dma_mapping_can_be_direct(0x1000, 64, constraints));
        assert!(dma_mapping_can_be_direct(0x1000, 128, constraints));
        assert!(!dma_mapping_can_be_direct(0x1000, 9, constraints));
        assert!(!dma_mapping_can_be_direct(0x1001, 64, constraints));
    }
}
