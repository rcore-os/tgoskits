use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use dma_api::{DeviceDma, DmaDirection, DmaError, DmaHandle, DmaMapHandle, DmaOp};

pub struct KlibDma;

static DMA: KlibDma = KlibDma;

pub fn op() -> &'static KlibDma {
    &DMA
}

pub fn device(dma_mask: u64) -> DeviceDma {
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

    fn layout_align(layout: Layout) -> usize {
        layout.align().max(PAGE_SIZE_4K)
    }

    /// Allocates DMA-coherent pages using the kernel DMA allocator.
    ///
    /// `dma_alloc_pages` is expected to honor `dma_mask` and the requested
    /// alignment. The checks below are defensive validation so a bad platform
    /// allocator fails before the buffer is handed to a device.
    unsafe fn alloc_for_layout(dma_mask: u64, layout: Layout) -> Result<Self, DmaError> {
        if layout.size() == 0 {
            return Ok(Self {
                cpu_addr: NonNull::dangling(),
                dma_addr: 0,
                num_pages: 0,
            });
        }

        let num_pages = Self::layout_pages(layout);
        let align = Self::layout_align(layout);
        let cpu_vaddr = crate::klib::dma_alloc_pages(dma_mask, num_pages, align)
            .map_err(|_| DmaError::NoMemory)?;
        let cpu_addr = NonNull::new(cpu_vaddr.as_mut_ptr()).ok_or(DmaError::NoMemory)?;
        let dma_addr = dma_addr_from_vaddr(cpu_vaddr);

        if !dma_range_fits_mask(dma_addr, layout.size(), dma_mask) {
            unsafe { Self::dealloc_pages(cpu_addr, num_pages) };
            return Err(DmaError::DmaMaskNotMatch {
                addr: dma_addr.into(),
                mask: dma_mask,
            });
        }
        if !dma_addr_is_aligned(dma_addr, layout.align()) {
            unsafe { Self::dealloc_pages(cpu_addr, num_pages) };
            return Err(DmaError::AlignMismatch {
                required: layout.align(),
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

    unsafe fn map_single(
        &self,
        dma_mask: u64,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let align = align.max(1);
        let layout = Layout::from_size_align(size.get(), align)?;
        let dma_addr = dma_addr_from_ptr(addr);

        if dma_range_fits_mask(dma_addr, size.get(), dma_mask)
            && dma_addr_is_aligned(dma_addr, align)
        {
            return Ok(unsafe { DmaMapHandle::new(addr, dma_addr.into(), layout, None) });
        }

        let map_pages = unsafe { DmaPages::alloc_for_layout(dma_mask, layout)? };
        let map_virt = map_pages.cpu_addr;

        if matches!(
            direction,
            DmaDirection::ToDevice | DmaDirection::Bidirectional
        ) {
            unsafe {
                map_virt
                    .as_ptr()
                    .copy_from_nonoverlapping(addr.as_ptr(), size.get());
            }
        }

        Ok(unsafe { DmaMapHandle::new(addr, map_pages.dma_addr.into(), layout, Some(map_virt)) })
    }

    unsafe fn unmap_single(&self, handle: DmaMapHandle) {
        if let Some(map_virt) = handle.alloc_virt() {
            let num_pages = DmaPages::layout_pages(handle.layout());
            unsafe { DmaPages::dealloc_pages(map_virt, num_pages) };
        }
    }

    fn prepare_read(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        if !matches!(
            direction,
            DmaDirection::FromDevice | DmaDirection::Bidirectional
        ) {
            return;
        }

        let target = unsafe { handle.as_ptr().add(offset) };
        if let Some(map_virt) = handle.alloc_virt()
            && map_virt != handle.as_ptr()
        {
            let source = unsafe { map_virt.add(offset) };
            self.invalidate(source, size);
            unsafe {
                target
                    .as_ptr()
                    .copy_from_nonoverlapping(source.as_ptr(), size);
            }
            return;
        }

        self.invalidate(target, size);
    }

    fn confirm_write(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        if !matches!(
            direction,
            DmaDirection::ToDevice | DmaDirection::Bidirectional
        ) {
            return;
        }

        let source = unsafe { handle.as_ptr().add(offset) };
        if let Some(map_virt) = handle.alloc_virt()
            && map_virt != handle.as_ptr()
        {
            let target = unsafe { map_virt.add(offset) };
            unsafe {
                target
                    .as_ptr()
                    .copy_from_nonoverlapping(source.as_ptr(), size);
            }
            self.flush(target, size);
            return;
        }

        self.flush(source, size);
    }

    unsafe fn alloc_coherent(&self, dma_mask: u64, layout: Layout) -> Option<DmaHandle> {
        let pages = unsafe { DmaPages::alloc_for_layout(dma_mask, layout).ok()? };
        if CoherentDmaPolicy::make_uncached(&pages, layout).is_err() {
            unsafe { DmaPages::dealloc_pages(pages.cpu_addr, pages.num_pages) };
            return None;
        }

        Some(unsafe { DmaHandle::new(pages.cpu_addr, pages.dma_addr.into(), layout) })
    }

    unsafe fn dealloc_coherent(&self, handle: DmaHandle) {
        let num_pages = DmaPages::layout_pages(handle.layout());
        if CoherentDmaPolicy::restore_cached(handle.as_ptr(), num_pages).is_err() {
            return;
        }
        unsafe { DmaPages::dealloc_pages(handle.as_ptr(), num_pages) };
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
