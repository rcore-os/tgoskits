use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use dma_api::{
    DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaDomainId, DmaError, DmaMapHandle,
    DmaOp,
};
use mbarrier::mb;

/// Physical memory zone that supplied a DMA page allocation.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaPageZone {
    /// General-purpose physical memory.
    Normal,
    /// Physical memory addressable with a 32-bit DMA mask.
    Dma32,
}

impl DmaPageZone {
    const fn backend_token(self) -> usize {
        match self {
            Self::Normal => 0,
            Self::Dma32 => 1,
        }
    }

    fn from_backend_token(token: usize) -> Self {
        match token {
            0 => Self::Normal,
            1 => Self::Dma32,
            _ => unreachable!("KlibDma emitted an invalid DMA page zone token"),
        }
    }
}

/// Move-only metadata required to release DMA pages to their source zone.
///
/// ```compile_fail
/// use axklib::dma::DmaPageAllocation;
///
/// fn require_copy<T: Copy>() {}
/// require_copy::<DmaPageAllocation>();
/// ```
#[repr(C)]
#[derive(Debug)]
pub struct DmaPageAllocation {
    addr: VirtAddr,
    num_pages: usize,
    zone: DmaPageZone,
}

impl DmaPageAllocation {
    /// Creates release metadata for pages returned by the platform allocator.
    pub const fn new(addr: VirtAddr, num_pages: usize, zone: DmaPageZone) -> Self {
        Self {
            addr,
            num_pages,
            zone,
        }
    }

    /// Consumes the allocation metadata and returns its release parameters.
    pub const fn into_parts(self) -> (VirtAddr, usize, DmaPageZone) {
        (self.addr, self.num_pages, self.zone)
    }
}

pub struct KlibDma;

static DMA: KlibDma = KlibDma;

pub fn op() -> &'static KlibDma {
    &DMA
}

pub const fn domain_id() -> DmaDomainId {
    DmaDomainId::identity()
}

pub fn device_with_mask(dma_mask: u64) -> DeviceDma {
    DeviceDma::new(domain_id(), dma_mask, op())
}

struct DmaPages {
    cpu_addr: NonNull<u8>,
    dma_addr: u64,
    num_pages: usize,
    zone: DmaPageZone,
}

impl DmaPages {
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
            return Ok(Self {
                cpu_addr: NonNull::dangling(),
                dma_addr: 0,
                num_pages: 0,
                zone: if constraints.addr_mask <= u32::MAX as u64 {
                    DmaPageZone::Dma32
                } else {
                    DmaPageZone::Normal
                },
            });
        }

        let num_pages = Self::layout_pages(layout);
        let align = Self::layout_align(layout, constraints);
        let allocation = crate::klib::dma_alloc_pages(constraints.addr_mask, num_pages, align)
            .map_err(|_| DmaError::NoMemory)?;
        let (cpu_vaddr, allocated_pages, zone) = allocation.into_parts();
        if allocated_pages != num_pages {
            crate::klib::dma_dealloc_pages(DmaPageAllocation::new(
                cpu_vaddr,
                allocated_pages,
                zone,
            ));
            return Err(DmaError::NoMemory);
        }
        let cpu_addr = NonNull::new(cpu_vaddr.as_mut_ptr()).ok_or(DmaError::NoMemory)?;
        let dma_addr = dma_addr_from_vaddr(cpu_vaddr);

        if !dma_range_fits_mask(dma_addr, layout.size(), constraints.addr_mask) {
            Self::dealloc_pages(cpu_addr, num_pages, zone);
            return Err(DmaError::DmaMaskNotMatch {
                addr: dma_addr.into(),
                mask: constraints.addr_mask,
            });
        }
        if !dma_addr_is_aligned(dma_addr, constraints.align.max(layout.align())) {
            Self::dealloc_pages(cpu_addr, num_pages, zone);
            return Err(DmaError::AlignMismatch {
                required: constraints.align.max(layout.align()),
                address: dma_addr.into(),
            });
        }

        Ok(Self {
            cpu_addr,
            dma_addr,
            num_pages,
            zone,
        })
    }

    fn dealloc_pages(cpu_addr: NonNull<u8>, num_pages: usize, zone: DmaPageZone) {
        if num_pages == 0 {
            return;
        }
        crate::klib::dma_dealloc_pages(DmaPageAllocation::new(
            VirtAddr::from_usize(cpu_addr.as_ptr() as usize),
            num_pages,
            zone,
        ));
    }
}

struct CoherentDmaPolicy;

impl CoherentDmaPolicy {
    fn make_uncached(pages: &DmaPages, layout: Layout) -> Result<(), DmaError> {
        if pages.num_pages == 0 {
            return Ok(());
        }

        let range_size = pages.num_pages * PAGE_SIZE_4K;
        let start = VirtAddr::from_usize(pages.cpu_addr.as_ptr() as usize).align_down_4k();
        crate::klib::mem_make_dma_coherent_uncached(start, range_size)
            .map_err(|_| DmaError::NoMemory)?;
        // SAFETY: `pages` owns a writable allocation covering `layout`, and
        // cacheability was changed before the bytes become device-visible.
        unsafe {
            pages.cpu_addr.as_ptr().write_bytes(0, layout.size());
        }
        Ok(())
    }

    fn restore_cached(pages: NonNull<u8>, num_pages: usize) -> Result<(), DmaError> {
        if num_pages == 0 {
            return Ok(());
        }

        let start = VirtAddr::from_usize(pages.as_ptr() as usize).align_down_4k();
        crate::klib::mem_restore_dma_cached(start, num_pages * PAGE_SIZE_4K)
            .map_err(|_| DmaError::NoMemory)
    }
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
        // SAFETY: `pages` owns the live allocation and preserves its source
        // zone in the opaque token consumed by this backend.
        Some(unsafe {
            DmaAllocHandle::new_with_backend_token(
                pages.cpu_addr,
                pages.dma_addr.into(),
                layout,
                pages.zone.backend_token(),
            )
        })
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        let num_pages = DmaPages::layout_pages(handle.layout());
        DmaPages::dealloc_pages(
            handle.as_ptr(),
            num_pages,
            DmaPageZone::from_backend_token(handle.backend_token()),
        );
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        let pages = DmaPages::alloc_for_layout(constraints, layout).ok()?;
        if CoherentDmaPolicy::make_uncached(&pages, layout).is_err() {
            DmaPages::dealloc_pages(pages.cpu_addr, pages.num_pages, pages.zone);
            return None;
        }

        // SAFETY: `pages` owns the live uncached allocation and preserves its
        // source zone in the opaque token consumed by this backend.
        Some(unsafe {
            DmaAllocHandle::new_with_backend_token(
                pages.cpu_addr,
                pages.dma_addr.into(),
                layout,
                pages.zone.backend_token(),
            )
        })
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        let num_pages = DmaPages::layout_pages(handle.layout());
        CoherentDmaPolicy::restore_cached(handle.as_ptr(), num_pages)
            .expect("DMA pages must regain their cached mapping before release");
        DmaPages::dealloc_pages(
            handle.as_ptr(),
            num_pages,
            DmaPageZone::from_backend_token(handle.backend_token()),
        );
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

        if dma_range_fits_mask(dma_addr, size.get(), constraints.addr_mask)
            && dma_addr_is_aligned(dma_addr, align)
        {
            // SAFETY: the caller keeps `addr` live for the mapping lifetime;
            // the checked identity address satisfies the device constraints.
            return Ok(unsafe { DmaMapHandle::new(addr, dma_addr.into(), layout, None) });
        }

        let map_pages = DmaPages::alloc_for_layout(constraints, layout)?;
        // SAFETY: the caller-owned address stays live, and `map_pages` owns the
        // bounce allocation recorded in this consume-on-unmap token.
        Ok(unsafe {
            DmaMapHandle::new_with_backend_token(
                addr,
                map_pages.dma_addr.into(),
                layout,
                Some(map_pages.cpu_addr),
                map_pages.zone.backend_token(),
            )
        })
    }

    unsafe fn unmap_streaming(&self, handle: DmaMapHandle) {
        if let Some(map_virt) = handle.bounce_ptr() {
            let num_pages = DmaPages::layout_pages(handle.layout());
            DmaPages::dealloc_pages(
                map_virt,
                num_pages,
                DmaPageZone::from_backend_token(handle.backend_token()),
            );
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
    dma_addr_from_vaddr(VirtAddr::from_usize(ptr.as_ptr() as usize))
}

fn dma_addr_from_vaddr(vaddr: VirtAddr) -> u64 {
    crate::klib::mem_virt_to_phys(vaddr).as_usize() as u64
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
