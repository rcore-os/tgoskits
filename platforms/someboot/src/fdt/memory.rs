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
            let Some(region) = normalize_region(region.address, region.size) else {
                continue;
            };

            add_memory_descriptor(MemoryDescriptor {
                physical_start: region.start,
                size_in_bytes: region.end - region.start,
                memory_type: MemoryType::Free,
            })
            .unwrap();
        }
    }

    for reserved in fdt.memory_reservations() {
        let Some(region) = normalize_region(reserved.address, reserved.size) else {
            continue;
        };
        let descriptor = MemoryDescriptor::new_aligned(
            region.start,
            region.end - region.start,
            MemoryType::Reserved,
            PAGE_SIZE,
        )
        .expect("FDT reserved-memory descriptor must have a valid aligned range");
        add_memory_descriptor(descriptor).unwrap_or_else(|error| {
            panic!("failed to add FDT memory reservation {region:#x?}: {error}")
        });
    }

    for reserved in fdt.reserved_memory() {
        let Some(regions) = reserved.reg() else {
            continue;
        };
        for reg in regions {
            let Some(size) = reg.size else {
                continue;
            };
            let Some(region) = normalize_region(reg.address, size) else {
                continue;
            };
            add_memory_descriptor(MemoryDescriptor {
                physical_start: region.start,
                size_in_bytes: region.end - region.start,
                memory_type: MemoryType::Reserved,
            })
            .unwrap_or_else(|error| {
                panic!("failed to reserve FDT memory region {region:#x?}: {error}")
            });
        }
    }

    Some(())
}

pub fn memories() -> impl Iterator<Item = Range<usize>> {
    let mut res = Vec::<_, 128>::new();
    if let Some(fdt) = fdt_base() {
        for memory in fdt.memory() {
            for region in memory.regions() {
                if let Some(region) = normalize_region(region.address, region.size) {
                    res.push(region)
                        .expect("FDT contains more than 128 usable memory regions");
                }
            }
        }
    }
    res.into_iter()
}

fn normalize_region(address: u64, size: u64) -> Option<Range<usize>> {
    if size == 0 {
        return None;
    }

    let start = normalize_fdt_address(address as usize);
    let size = size as usize;
    let end = start.checked_add(size)?;
    Some(start..end)
}

fn normalize_fdt_address(address: usize) -> usize {
    <crate::arch::Arch as crate::ArchTrait>::canonicalize_paddr(address)
}
