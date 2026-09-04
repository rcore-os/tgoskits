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
        add_memory_descriptor(MemoryDescriptor::new_aligned(
            region.start,
            region.end - region.start,
            MemoryType::Reserved,
            PAGE_SIZE,
        ))
        .unwrap();
    }

    for reserved in fdt.reserved_memory() {
        if let Some(mut itr) = reserved.reg()
            && let Some(reg) = itr.next()
            && let Some(size) = reg.size
            && let Some(region) = normalize_region(reg.address, size)
        {
            add_memory_descriptor(MemoryDescriptor {
                physical_start: region.start,
                size_in_bytes: region.end - region.start,
                memory_type: MemoryType::Reserved,
            })
            .unwrap();
        }
    }

    Some(())
}

/// Registers device `reg` ranges from the firmware DTB as MMIO descriptors.
///
/// Dynamic guests under a hypervisor still probe interrupt controllers and other
/// platform devices through FDT. Those MMIO windows must be present in the
/// memory map before `ax-mm` rebuilds the kernel page table, otherwise runtime
/// `iomap()` may fail for addresses such as the GIC distributor at
/// `0x0800_0000` even though the direct-map UART window was added separately.
pub fn init_device_mmio_map() -> Option<()> {
    let fdt = fdt_base()?;

    for node in fdt.all_nodes() {
        if !is_device_mmio_node(&node) {
            continue;
        }

        let Some(regs) = node.reg() else {
            continue;
        };

        for reg in regs {
            let Some(size) = reg.size else {
                continue;
            };
            if reg.address == 0 {
                continue;
            }
            let Some(region) = normalize_region(reg.address, size) else {
                continue;
            };

            let _ = add_memory_descriptor(MemoryDescriptor::new_aligned(
                region.start,
                region.end - region.start,
                MemoryType::Mmio,
                PAGE_SIZE,
            ));
        }
    }

    Some(())
}

fn is_device_mmio_node(node: &fdt_raw::Node<'_>) -> bool {
    let name = node.name();
    if name.starts_with("memory") || name == "cpus" || name.contains("reserved-memory") {
        return false;
    }

    !matches!(
        node.find_property_str("status"),
        Some("disabled") | Some("fail") | Some("fail-safest")
    )
}

pub fn memories() -> impl Iterator<Item = Range<usize>> {
    let mut res = Vec::<_, 128>::new();
    if let Some(fdt) = fdt_base() {
        for memory in fdt.memory() {
            for region in memory.regions() {
                if let Some(region) = normalize_region(region.address, region.size) {
                    res.push(region).ok();
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
