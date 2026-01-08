use alloc::boxed::Box;
use byte_unit::{Byte, UnitType};
use kernutil::memory::MemoryType;
use spin::Mutex;

use crate::hal::al::*;

static KERNEL_TABLE: Mutex<Option<Box<dyn PageTable>>> = Mutex::new(None);

pub fn init() {
    info!("Setting up MMU and page tables");

    let mut pt = memory::page_table_new().unwrap();
    map_regions(&mut pt);
    let pt_addr = pt.addr();
    {
        let mut g = KERNEL_TABLE.lock();
        *g = Some(pt);
    }
    debug!("Setting kernel page table to {pt_addr:?}");
    // let pt_addr = memory::kernel_page_table();
    memory::set_kernel_page_table(pt_addr);
    memory::enable_paging();
}

fn map_regions(pt: &mut Box<dyn PageTable>) {
    for region in memory::memory_map() {
        let phys = PhysAddr::from(region.physical_start);
        let fmt = Byte::from(region.size_in_bytes).get_appropriate_unit(UnitType::Binary);
        match region.memory_type {
            MemoryType::Mmio => {
                debug!(
                    "Mapping `{:<16}`: [0x{:>016x}, 0x{:>016x}) {}",
                    region.name,
                    phys.raw(),
                    (phys.raw() + region.size_in_bytes),
                    fmt
                );
                pt.ioremap(phys.raw().into(), region.size_in_bytes, false)
                    .expect("Failed to map mmio");
            }
            _ => {
                let virt = VirtAddr::from(phys);
                let config = MemConfig {
                    access: AccessFlags::READ | AccessFlags::WRITE | AccessFlags::EXECUTE,
                    attrs: MemAttributes::Normal,
                };
                debug!(
                    "Mapping `{:<16}`: [0x{:>016x}, 0x{:>016x}) -> [0x{:>016x}, 0x{:>016x}) {} ({:#.2})",
                    region.name,
                    virt.raw(),
                    (virt.raw() + region.size_in_bytes),
                    phys.raw(),
                    (phys.raw() + region.size_in_bytes),
                    config,
                    fmt
                );
                pt.map(
                    virt.raw().into(),
                    phys.raw().into(),
                    region.size_in_bytes,
                    config,
                    false,
                )
                .expect("Failed to map memory region");
            }
        };
    }
}
