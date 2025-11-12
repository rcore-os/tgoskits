use os_helper::memory::MemoryDescriptor;

use crate::os::mem::address::{PhysAddr, VirtAddr};

mod address;
mod allocator;

pub(crate) fn init_heap(regions: &[MemoryDescriptor]) {
    for region in regions {
        if region.memory_type == os_helper::memory::MemoryType::Usable {
            let start = PhysAddr::new(region.physical_start);
            let size = region.size_in_bytes;
            let start: VirtAddr = start.into();

            let memory = unsafe { core::slice::from_raw_parts_mut(start.into(), size) };

            allocator::ALLOCATOR.add_to_frame(memory);
        }
    }
}
