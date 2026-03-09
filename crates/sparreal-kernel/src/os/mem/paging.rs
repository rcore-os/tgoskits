use alloc::boxed::Box;
use byte_unit::{Byte, UnitType};
use kernutil::memory::MemoryType;
use num_align::NumAlign;
use spin::Mutex;

use crate::{
    hal::al::*,
    os::mem::{__kimage_va, __percpu, __va},
};

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
    memory::set_kernel_page_table(pt_addr);
}

fn map_regions(pt: &mut Box<dyn PageTable>) {
    for region in memory::memory_map() {
        let phys = PhysAddr::from(region.physical_start);
        let fmt = Byte::from(region.size_in_bytes).get_appropriate_unit(UnitType::Binary);
        let virt;
        let access;
        let attrs;
        let mut size = region.size_in_bytes;
        match region.memory_type {
            MemoryType::KImage => {
                virt = __kimage_va(phys);
                access = AccessFlags::READ | AccessFlags::WRITE | AccessFlags::EXECUTE;
                attrs = MemAttributes::Normal;
                size = size.align_up(2 * 1024 * 1024);
            }
            MemoryType::Mmio => {
                virt = __va(phys);
                access = AccessFlags::READ | AccessFlags::WRITE;
                attrs = MemAttributes::Device;
            }
            MemoryType::PerCpuData => {
                virt = __percpu(phys);
                access = AccessFlags::READ | AccessFlags::WRITE | AccessFlags::EXECUTE;
                attrs = MemAttributes::PerCpu;
            }
            _ => {
                virt = __va(phys);
                access = AccessFlags::READ | AccessFlags::WRITE | AccessFlags::EXECUTE;
                attrs = MemAttributes::Normal;
            }
        }
        let config = MemConfig { access, attrs };
        debug!(
            "Mapping `{}`: [0x{:>016x}, 0x{:>016x}) -> [0x{:>016x}, 0x{:>016x}) {} ({:#.2})",
            region.memory_type,
            virt.raw(),
            (virt.raw() + size),
            phys.raw(),
            (phys.raw() + size),
            config,
            fmt
        );
        pt.map(virt.raw().into(), phys.raw().into(), size, config, false)
            .expect("Failed to map memory region");
    }
}

pub fn ioremap(phys_start: PhysAddr, size: usize) -> Result<IoMemAddr, PagingError> {
    let mut g = KERNEL_TABLE.lock();
    let pt = g.as_mut().expect("Kernel page table not initialized");
    pt.ioremap(phys_start, size, true)
}

pub fn iounmap(mmio: &mmio_api::MmioRaw) -> Result<(), PagingError> {
    let phys_start = PhysAddr::from(mmio.phys_addr().as_usize());
    let virt = crate::os::mem::__io(phys_start);
    let end = virt + mmio.size();
    let virt_start = virt.align_down(memory::page_size());
    let virt_end = end.align_up(memory::page_size());
    let size = virt_end - virt_start;

    let mut g = KERNEL_TABLE.lock();
    let pt = g.as_mut().expect("Kernel page table not initialized");
    pt.unmap(virt_start.raw().into(), size)
}
