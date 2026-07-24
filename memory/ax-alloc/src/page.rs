use ax_errno::AxResult;
use ax_memory_addr::{PhysAddr, VirtAddr};

use crate::{MemoryZone, PAGE_SIZE, PageRelease, PageRequest, UsageKind, global_allocator};

/// A RAII wrapper of contiguous 4K-sized pages.
///
/// It will automatically deallocate the pages when dropped.
#[derive(Debug)]
pub struct GlobalPage {
    start_vaddr: VirtAddr,
    request: PageRequest,
    usage: UsageKind,
}

impl GlobalPage {
    pub(crate) fn allocate(request: PageRequest, usage: UsageKind) -> crate::AllocResult<Self> {
        let vaddr = global_allocator().allocate_pages_raw(request, usage)?;
        Ok(Self {
            start_vaddr: vaddr.into(),
            request,
            usage,
        })
    }

    /// Allocate one 4K-sized page.
    pub fn alloc() -> AxResult<Self> {
        Self::allocate(
            PageRequest {
                count: 1,
                align: PAGE_SIZE,
                zone: MemoryZone::Normal,
            },
            UsageKind::Global,
        )
        .map_err(Into::into)
    }

    /// Allocate one 4K-sized page and fill with zero.
    pub fn alloc_zero() -> AxResult<Self> {
        let mut p = Self::alloc()?;
        p.zero();
        Ok(p)
    }

    /// Allocate contiguous 4K-sized pages.
    pub fn alloc_contiguous(num_pages: usize, alignment: usize) -> AxResult<Self> {
        Self::allocate(
            PageRequest {
                count: num_pages,
                align: alignment,
                zone: MemoryZone::Normal,
            },
            UsageKind::Global,
        )
        .map_err(Into::into)
    }

    /// Get the start virtual address of this page.
    pub fn start_vaddr(&self) -> VirtAddr {
        self.start_vaddr
    }

    /// Get the start physical address of this page.
    pub fn start_paddr<F>(&self, virt_to_phys: F) -> PhysAddr
    where
        F: FnOnce(VirtAddr) -> PhysAddr,
    {
        virt_to_phys(self.start_vaddr)
    }

    /// Get the total size (in bytes) of these page(s).
    pub fn size(&self) -> usize {
        self.request.count * PAGE_SIZE
    }

    /// Returns the source zone of this allocation.
    pub const fn zone(&self) -> MemoryZone {
        self.request.zone
    }

    /// Returns the allocation usage classification.
    pub const fn usage(&self) -> UsageKind {
        self.usage
    }

    /// Convert to a raw pointer.
    pub fn as_ptr(&self) -> *const u8 {
        self.start_vaddr.as_ptr()
    }

    /// Convert to a mutable raw pointer.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.start_vaddr.as_mut_ptr()
    }

    /// Fill `self` with `byte`.
    pub fn fill(&mut self, byte: u8) {
        // SAFETY: `GlobalPage` exclusively owns the live allocation, and the
        // validated request records its complete byte extent.
        unsafe { core::ptr::write_bytes(self.as_mut_ptr(), byte, self.size()) }
    }

    /// Fill `self` with zero.
    pub fn zero(&mut self) {
        self.fill(0)
    }

    /// Forms a slice that can read data.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: the allocation remains live for `self`, and immutable access
        // cannot overlap mutable access through this owner.
        unsafe { core::slice::from_raw_parts(self.as_ptr(), self.size()) }
    }

    /// Forms a mutable slice that can write data.
    pub fn as_slice_mut(&mut self) -> &mut [u8] {
        // SAFETY: `&mut self` proves exclusive CPU access through this owner,
        // and the slice is bounded by the validated allocation size.
        unsafe { core::slice::from_raw_parts_mut(self.as_mut_ptr(), self.size()) }
    }
}

impl Drop for GlobalPage {
    fn drop(&mut self) {
        // SAFETY: this owner stores the unchanged request and usage associated
        // with the live allocation, and Drop runs exactly once.
        unsafe {
            global_allocator().deallocate_pages_raw(
                self.start_vaddr.into(),
                PageRelease::from(self.request),
                self.usage,
            );
        }
    }
}
