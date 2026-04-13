use core::{alloc::Layout, ptr::NonNull};

#[cfg(not(feature = "buddy-slab"))]
use ax_alloc::DefaultByteAllocator;
use ax_alloc::{AllocError, AllocResult, UsageKind, global_allocator};
#[cfg(not(feature = "buddy-slab"))]
use ax_allocator::{BaseAllocator, ByteAllocator};
use ax_hal::{mem::virt_to_phys, paging::MappingFlags};
use ax_kspin::SpinNoIrq;
use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr, va};
#[cfg(not(feature = "buddy-slab"))]
use log::debug;
use log::error;

use crate::{BusAddr, DMAInfo, phys_to_bus};

pub(crate) static ALLOCATOR: SpinNoIrq<DmaAllocator> = SpinNoIrq::new(DmaAllocator::new());

pub(crate) struct DmaAllocator {
    #[cfg(not(feature = "buddy-slab"))]
    alloc: DefaultByteAllocator,
}

impl DmaAllocator {
    pub const fn new() -> Self {
        Self {
            #[cfg(not(feature = "buddy-slab"))]
            alloc: DefaultByteAllocator::new(),
        }
    }

    /// Allocate arbitrary number of bytes. Returns the left bound of the
    /// allocated region.
    ///
    /// It firstly tries to allocate from the coherent byte allocator. If there is no
    /// memory, it asks the global page allocator for more memory and adds it to the
    /// byte allocator.
    pub unsafe fn alloc_coherent(&mut self, layout: Layout) -> AllocResult<DMAInfo> {
        #[cfg(feature = "buddy-slab")]
        {
            self.alloc_coherent_pages(layout)
        }

        #[cfg(not(feature = "buddy-slab"))]
        if layout.size() >= PAGE_SIZE_4K {
            self.alloc_coherent_pages(layout)
        } else {
            self.alloc_coherent_bytes(layout)
        }
    }

    #[cfg(not(feature = "buddy-slab"))]
    fn alloc_coherent_bytes(&mut self, layout: Layout) -> AllocResult<DMAInfo> {
        let mut is_expanded = false;
        loop {
            if let Ok(data) = self.alloc.alloc(layout) {
                let cpu_addr = va!(data.as_ptr() as usize);
                return Ok(DMAInfo {
                    cpu_addr: data,
                    bus_addr: virt_to_bus(cpu_addr),
                });
            } else {
                if is_expanded {
                    return Err(AllocError::NoMemory);
                }
                is_expanded = true;
                let available_pages = global_allocator().available_pages();
                // 4 pages or available pages.
                let num_pages = 4.min(available_pages);
                let expand_size = num_pages * PAGE_SIZE_4K;
                let vaddr_raw =
                    global_allocator().alloc_pages(num_pages, PAGE_SIZE_4K, UsageKind::Dma)?;
                let vaddr = va!(vaddr_raw);
                self.update_flags(
                    vaddr,
                    num_pages,
                    MappingFlags::READ | MappingFlags::WRITE | MappingFlags::UNCACHED,
                )?;
                self.alloc
                    .add_memory(vaddr_raw, expand_size)
                    .map_err(AllocError::from)
                    .inspect_err(|e| error!("add memory fail: {e:?}"))?;
                debug!("expand memory @{vaddr:#X}, size: {expand_size:#X} bytes");
            }
        }
    }

    fn alloc_coherent_pages(&mut self, layout: Layout) -> AllocResult<DMAInfo> {
        let num_pages = layout_pages(&layout);
        let vaddr_raw = global_allocator().alloc_pages(
            num_pages,
            PAGE_SIZE_4K.max(layout.align()),
            UsageKind::Dma,
        )?;
        let vaddr = va!(vaddr_raw);
        self.update_flags(
            vaddr,
            num_pages,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::UNCACHED,
        )?;
        Ok(DMAInfo {
            cpu_addr: unsafe { NonNull::new_unchecked(vaddr_raw as *mut u8) },
            bus_addr: virt_to_bus(vaddr),
        })
    }

    fn update_flags(
        &mut self,
        vaddr: VirtAddr,
        num_pages: usize,
        flags: MappingFlags,
    ) -> AllocResult<()> {
        let expand_size = num_pages * PAGE_SIZE_4K;
        ax_mm::kernel_aspace()
            .lock()
            .protect(vaddr, expand_size, flags)
            .map_err(|e| {
                error!("change table flag fail: {e:?}");
                AllocError::NoMemory
            })
    }

    /// Gives back the allocated region to the byte allocator.
    pub unsafe fn dealloc_coherent(&mut self, dma: DMAInfo, layout: Layout) {
        #[cfg(feature = "buddy-slab")]
        {
            let num_pages = layout_pages(&layout);
            let virt_raw = dma.cpu_addr.as_ptr() as usize;
            global_allocator().dealloc_pages(virt_raw, num_pages, UsageKind::Dma);
            let _ = self.update_flags(
                va!(virt_raw),
                num_pages,
                MappingFlags::READ | MappingFlags::WRITE,
            );
        }

        #[cfg(not(feature = "buddy-slab"))]
        if layout.size() >= PAGE_SIZE_4K {
            let num_pages = layout_pages(&layout);
            let virt_raw = dma.cpu_addr.as_ptr() as usize;
            global_allocator().dealloc_pages(virt_raw, num_pages, UsageKind::Dma);
            let _ = self.update_flags(
                va!(virt_raw),
                num_pages,
                MappingFlags::READ | MappingFlags::WRITE,
            );
        } else {
            self.alloc.dealloc(dma.cpu_addr, layout)
        }
    }
}

fn virt_to_bus(addr: VirtAddr) -> BusAddr {
    let paddr = virt_to_phys(addr);
    phys_to_bus(paddr)
}

const fn layout_pages(layout: &Layout) -> usize {
    ax_memory_addr::align_up_4k(layout.size()) / PAGE_SIZE_4K
}
