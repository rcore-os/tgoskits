use byte_unit::{Byte, UnitType};
use kernutil::memory::MemoryDescriptor;

use crate::os::mem::address::{PhysAddr, VirtAddr};

pub use allocator::{KernelAllocator, KernelMemoryAllocator, kernel_memory_allocator};

mod address;
mod allocator;
pub mod dma;
pub mod mmio;
pub(crate) mod paging;

pub use paging::{ioremap, iounmap};

pub fn page_size() -> usize {
    crate::hal::al::memory::page_size()
}

pub(crate) fn __va(addr: PhysAddr) -> VirtAddr {
    crate::hal::al::memory::_va(addr)
}

pub(crate) fn __io(addr: PhysAddr) -> VirtAddr {
    crate::hal::al::memory::_io(addr)
}

pub(crate) fn __percpu(addr: PhysAddr) -> VirtAddr {
    crate::hal::al::memory::_percpu(addr)
}

pub(crate) fn __kimage_va(addr: PhysAddr) -> VirtAddr {
    let offset = crate::hal::al::memory::kimage_offset();
    VirtAddr::new((addr.raw() as isize - offset) as usize)
}

pub(crate) fn init_heap(regions: &[MemoryDescriptor]) {
    for region in regions {
        if region.memory_type == kernutil::memory::MemoryType::Free {
            let start = PhysAddr::new(region.physical_start).align_up(page_size());
            let end =
                PhysAddr::new(region.physical_start + region.size_in_bytes).align_down(page_size());
            let size = end - start;
            if size == 0 {
                continue;
            }
            let byte_count = Byte::from(size);
            let adjusted_byte = byte_count.get_appropriate_unit(UnitType::Binary);
            let start: VirtAddr = __va(start);
            debug!(
                "Alloc add: {} - {} ({:.2})",
                start,
                start + size,
                adjusted_byte
            );

            #[cfg(target_os = "none")]
            {
                let memory = unsafe { core::slice::from_raw_parts_mut(start.into(), size) };

                allocator::kernel_memory_allocator().add_memory_region(memory);
            }
        }
    }
}
