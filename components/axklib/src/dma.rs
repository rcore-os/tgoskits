use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use dma_api::{
    DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp,
};

pub struct KlibDma;

static DMA: KlibDma = KlibDma;

pub fn op() -> &'static KlibDma {
    &DMA
}

pub fn device_with_mask(dma_mask: u64) -> DeviceDma {
    DeviceDma::new(dma_mask, op())
}

struct DmaPages {
    cpu_addr: NonNull<u8>,
    dma_addr: u64,
    num_pages: usize,
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
    unsafe fn alloc_for_layout(
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Result<Self, DmaError> {
        if layout.size() == 0 {
            return Ok(Self {
                cpu_addr: NonNull::dangling(),
                dma_addr: 0,
                num_pages: 0,
            });
        }

        let num_pages = Self::layout_pages(layout);
        let align = Self::layout_align(layout, constraints);
        let cpu_vaddr = crate::klib::dma_alloc_pages(constraints.addr_mask, num_pages, align)
            .map_err(|_| DmaError::NoMemory)?;
        let cpu_addr = NonNull::new(cpu_vaddr.as_mut_ptr()).ok_or(DmaError::NoMemory)?;
        let dma_addr = dma_addr_from_vaddr(cpu_vaddr);

        if !dma_range_fits_mask(dma_addr, layout.size(), constraints.addr_mask) {
            unsafe { Self::dealloc_pages(cpu_addr, num_pages) };
            return Err(DmaError::DmaMaskNotMatch {
                addr: dma_addr.into(),
                mask: constraints.addr_mask,
            });
        }
        if !dma_addr_is_aligned(dma_addr, constraints.align.max(layout.align())) {
            unsafe { Self::dealloc_pages(cpu_addr, num_pages) };
            return Err(DmaError::AlignMismatch {
                required: constraints.align.max(layout.align()),
                address: dma_addr.into(),
            });
        }

        Ok(Self {
            cpu_addr,
            dma_addr,
            num_pages,
        })
    }

    unsafe fn dealloc_pages(cpu_addr: NonNull<u8>, num_pages: usize) {
        if num_pages == 0 {
            return;
        }
        crate::klib::dma_dealloc_pages(VirtAddr::from_usize(cpu_addr.as_ptr() as usize), num_pages);
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
        let pages = unsafe { DmaPages::alloc_for_layout(constraints, layout).ok()? };
        Some(unsafe { DmaAllocHandle::new(pages.cpu_addr, pages.dma_addr.into(), layout) })
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
        let pages = unsafe { DmaPages::alloc_for_layout(constraints, layout).ok()? };
        if CoherentDmaPolicy::make_uncached(&pages, layout).is_err() {
            unsafe { DmaPages::dealloc_pages(pages.cpu_addr, pages.num_pages) };
            return None;
        }

        Some(unsafe { DmaAllocHandle::new(pages.cpu_addr, pages.dma_addr.into(), layout) })
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        let num_pages = DmaPages::layout_pages(handle.layout());
        if CoherentDmaPolicy::restore_cached(handle.as_ptr(), num_pages).is_err() {
            return;
        }
        unsafe { DmaPages::dealloc_pages(handle.as_ptr(), num_pages) };
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
            return Ok(unsafe { DmaMapHandle::new(addr, dma_addr.into(), layout, None) });
        }

        let map_pages = unsafe { DmaPages::alloc_for_layout(constraints, layout)? };
        Ok(unsafe {
            DmaMapHandle::new(
                addr,
                map_pages.dma_addr.into(),
                layout,
                Some(map_pages.cpu_addr),
            )
        })
    }

    unsafe fn unmap_streaming(&self, handle: DmaMapHandle) {
        if let Some(map_virt) = handle.bounce_ptr() {
            let num_pages = DmaPages::layout_pages(handle.layout());
            unsafe { DmaPages::dealloc_pages(map_virt, num_pages) };
        }
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
