use core::ops::Range;

use heapless::Vec;

use crate::{
    consts::PAGE_SIZE,
    fdt::fdt_base,
    mem::{MemoryDescriptor, MemoryType, add_memory_descriptor},
};

pub fn init_memory_map() -> Option<()> {
    let fdt = super::fdt_base()?;

    for memory in fdt.memory() {
        for region in memory.regions() {
            if region.size == 0 {
                continue;
            }

            add_memory_descriptor(MemoryDescriptor {
                physical_start: region.address as usize,
                size_in_bytes: region.size as _,
                memory_type: MemoryType::Free,
            })
            .unwrap();
        }
    }

    for reserved in fdt.memory_reservations() {
        add_memory_descriptor(MemoryDescriptor::new_aligned(
            reserved.address as usize,
            reserved.size as usize,
            MemoryType::Reserved,
            PAGE_SIZE,
        ))
        .unwrap();
    }

    for reserved in fdt.reserved_memory() {
        if let Some(mut itr) = reserved.reg()
            && let Some(reg) = itr.next()
            && let Some(size) = reg.size
            && size > 0
        {
            add_memory_descriptor(MemoryDescriptor {
                physical_start: reg.address as usize,
                size_in_bytes: size as usize,
                memory_type: MemoryType::Reserved,
            })
            .unwrap();
        }
    }

    Some(())
}

pub fn memories() -> impl Iterator<Item = Range<usize>> {
    let mut res = Vec::<_, 128>::new();
    if let Some(fdt) = fdt_base() {
        for memory in fdt.memory() {
            for region in memory.regions() {
                res.push(region.address as usize..(region.address + region.size) as usize)
                    .ok();
            }
        }
    }
    res.into_iter()
}
