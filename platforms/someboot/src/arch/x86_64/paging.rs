use ax_cpu::paging::{DescriptorFlags, Pte};
use num_align::NumAlign;
use page_table_generic::{MapConfig, TableMeta, VirtAddr};
use x86::msr::rdmsr;

use crate::{
    arch::addrspace::{KERNEL_BASE, PERCPU_BASE, PHYS_VIRT_OFFSET},
    console::print_mapping,
    mem::{__kimage_va, MemAttributes, PageTableInfo, PteConfig, cpu_area_phys_to_virt, page_size},
};

/// Boot mapping policy over the CPU-owned native descriptor.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Entry(Pte);

impl page_table_generic::PageTableEntry for Entry {
    type PteConfig = PteConfig;

    fn new_page(
        paddr: page_table_generic::PhysAddr,
        config: Self::PteConfig,
        is_huge: bool,
    ) -> Self {
        let mut flags = DescriptorFlags::PRESENT | DescriptorFlags::ACCESSED;
        flags.set(DescriptorFlags::WRITABLE, config.writable);
        flags.set(DescriptorFlags::USER, config.lower);
        flags.set(DescriptorFlags::DIRTY, config.dirty);
        flags.set(DescriptorFlags::GLOBAL, config.global);
        flags.set(DescriptorFlags::HUGE_PAGE, is_huge);
        if matches!(
            config.mem_attr,
            MemAttributes::Device | MemAttributes::Uncached
        ) {
            flags |= DescriptorFlags::NO_CACHE | DescriptorFlags::WRITE_THROUGH;
        }
        flags.set(DescriptorFlags::NO_EXECUTE, !config.executable);
        Self(Pte::from_parts(paddr, flags))
    }

    fn new_table(paddr: page_table_generic::PhysAddr) -> Self {
        Self(Pte::from_parts(
            paddr,
            DescriptorFlags::PRESENT | DescriptorFlags::WRITABLE | DescriptorFlags::ACCESSED,
        ))
    }

    fn paddr(&self, is_dir: bool) -> page_table_generic::PhysAddr {
        self.0.paddr(is_dir)
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let flags = self.0.flags();
        let mem_attr =
            if flags.intersects(DescriptorFlags::NO_CACHE | DescriptorFlags::WRITE_THROUGH) {
                MemAttributes::Device
            } else {
                MemAttributes::Normal
            };
        PteConfig {
            read: flags.contains(DescriptorFlags::PRESENT),
            writable: flags.contains(DescriptorFlags::WRITABLE),
            executable: !flags.contains(DescriptorFlags::NO_EXECUTE),
            lower: flags.contains(DescriptorFlags::USER),
            dirty: flags.contains(DescriptorFlags::DIRTY),
            global: flags.contains(DescriptorFlags::GLOBAL),
            mem_attr,
        }
    }

    fn present(&self) -> bool {
        self.0.present()
    }

    fn huge(&self, is_dir: bool) -> bool {
        self.0.huge(is_dir)
    }

    fn unused(&self) -> bool {
        self.0.unused()
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

#[derive(Clone, Copy)]
pub struct Generic;

impl TableMeta for Generic {
    type P = Entry;

    const PAGE_SIZE: usize = 0x1000;
    const LEVEL_BITS: &'static [usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 2;

    fn flush(vaddr: Option<VirtAddr>) {
        ax_cpu::mmu::flush_tlb(vaddr);
    }
}

pub fn enable_mmu() -> ! {
    if let Err(err) = setup_page_table() {
        panic!("failed to setup x86_64 page table: {err:?}");
    }

    let v_sp = crate::smp::primary_stack_top_virtual(crate::smp::early_current_cpu_idx())
        .expect("primary reserved stack must be addressable before final per-CPU initialization");
    let v_entry = __kimage_va(super::entry::mmu_entry as *const () as usize) as usize;
    println!("x86_64 switching CR3 and resetting relocations before high-half jump");

    super::relocate::reset();

    // SAFETY: the final high-half mapping and reserved primary stack are live.
    unsafe { ax_cpu::boot::jump_to(v_entry, v_sp) }
}

fn setup_page_table() -> anyhow::Result<()> {
    let mut table = crate::mem::mmu::new_boot_table();

    for region in crate::mem::memory_map() {
        let size = region.size_in_bytes.align_up(page_size());
        if size == 0 {
            continue;
        }
        let name = match region.memory_type {
            crate::mem::MemoryType::Free => "Free",
            crate::mem::MemoryType::Ram => "Ram",
            crate::mem::MemoryType::Reserved => "Reserved",
            crate::mem::MemoryType::Mmio => "Mmio",
            crate::mem::MemoryType::KImage => "KImage",
            crate::mem::MemoryType::PerCpuData => "PerCpu",
        };

        let pte = PteConfig {
            read: true,
            writable: true,
            executable: region.memory_type != crate::mem::MemoryType::Mmio,
            global: true,
            mem_attr: match region.memory_type {
                crate::mem::MemoryType::Mmio => MemAttributes::Device,
                _ => MemAttributes::Normal,
            },
            ..Default::default()
        };

        print_mapping(name, region.physical_start, region.physical_start, size);

        table.map(&MapConfig {
            vaddr: region.physical_start.into(),
            paddr: region.physical_start.into(),
            size,
            pte,
            allow_huge: true,
            flush: false,
        })?;

        let direct_vaddr = region.physical_start.wrapping_add(PHYS_VIRT_OFFSET);
        print_mapping(name, direct_vaddr, region.physical_start, size);
        table.map(&MapConfig {
            vaddr: direct_vaddr.into(),
            paddr: region.physical_start.into(),
            size,
            pte,
            allow_huge: true,
            flush: false,
        })?;
    }

    let lapic_base = (unsafe { rdmsr(x86::msr::IA32_APIC_BASE) } as usize) & !(page_size() - 1);
    let lapic_mapped = crate::mem::memory_map().iter().any(|region| {
        let start = region.physical_start;
        let end = start.saturating_add(region.size_in_bytes);
        (start..end).contains(&lapic_base)
    });
    if !lapic_mapped {
        let lapic_vaddr = lapic_base.wrapping_add(PHYS_VIRT_OFFSET);
        print_mapping("LAPIC", lapic_base, lapic_base, page_size());
        table.map(&MapConfig {
            vaddr: lapic_base.into(),
            paddr: lapic_base.into(),
            size: page_size(),
            pte: PteConfig {
                read: true,
                writable: true,
                executable: false,
                global: true,
                mem_attr: MemAttributes::Device,
                ..Default::default()
            },
            allow_huge: false,
            flush: false,
        })?;

        print_mapping("LAPIC", lapic_vaddr, lapic_base, page_size());
        table.map(&MapConfig {
            vaddr: lapic_vaddr.into(),
            paddr: lapic_base.into(),
            size: page_size(),
            pte: PteConfig {
                read: true,
                writable: true,
                executable: false,
                global: true,
                mem_attr: MemAttributes::Device,
                ..Default::default()
            },
            allow_huge: false,
            flush: false,
        })?;
    }

    let ap_trampoline = super::power::AP_TRAMPOLINE_PADDR;
    let ap_trampoline_mapped = crate::mem::memory_map().iter().any(|region| {
        let start = region.physical_start;
        let end = start.saturating_add(region.size_in_bytes);
        (start..end).contains(&ap_trampoline)
    });
    if !ap_trampoline_mapped {
        print_mapping("APTrampoline", ap_trampoline, ap_trampoline, page_size());
        table.map(&MapConfig {
            vaddr: ap_trampoline.into(),
            paddr: ap_trampoline.into(),
            size: page_size(),
            pte: PteConfig {
                read: true,
                writable: true,
                executable: true,
                global: true,
                mem_attr: MemAttributes::Normal,
                ..Default::default()
            },
            allow_huge: false,
            flush: false,
        })?;
    }

    let kimage = crate::mem::kimage_range();
    let kimage_size = kimage.len().align_up(2 * 1024 * 1024);
    let kimage_vaddr = __kimage_va(kimage.start);
    print_mapping("KImage", kimage_vaddr as _, kimage.start, kimage_size);
    table.map(&MapConfig {
        vaddr: VirtAddr::from_usize(kimage_vaddr as usize),
        paddr: kimage.start.into(),
        size: kimage_size,
        pte: PteConfig {
            read: true,
            writable: true,
            executable: true,
            global: true,
            mem_attr: MemAttributes::Normal,
            ..Default::default()
        },
        allow_huge: true,
        flush: false,
    })?;

    let cpu_area_region = crate::smp::cpu_area_region();
    print_mapping(
        "PerCpu",
        cpu_area_phys_to_virt(cpu_area_region.start) as _,
        cpu_area_region.start,
        cpu_area_region.len(),
    );
    table.map(&MapConfig {
        vaddr: VirtAddr::from_usize(cpu_area_phys_to_virt(cpu_area_region.start) as usize),
        paddr: cpu_area_region.start.into(),
        size: cpu_area_region.len(),
        pte: PteConfig {
            read: true,
            writable: true,
            executable: true,
            global: true,
            mem_attr: MemAttributes::PerCpu,
            ..Default::default()
        },
        allow_huge: true,
        flush: false,
    })?;

    let root = table.root_paddr();
    crate::mem::mmu::set_boot_table(table);
    // The boot page tables contain NX leaf mappings. Enable NXE before
    // loading them, otherwise x86_64 treats the NX bit as reserved.
    // SAFETY: early CPL0 boot owns NX-capable page tables.
    unsafe { ax_cpu::boot::enable_execute_disable() };
    super::trap::set_cr3(root);
    // SAFETY: the new boot tables are installed before tasks or IRQs exist.
    unsafe { ax_cpu::boot::configure_paging() };
    Ok(())
}

pub fn current_table() -> PageTableInfo {
    PageTableInfo {
        asid: 0,
        addr: super::trap::current_cr3().as_usize(),
    }
}

pub fn set_table(info: PageTableInfo) {
    super::trap::set_cr3(info.addr.into());
}

pub fn virt_to_phys(vaddr: *const u8) -> usize {
    let vaddr = vaddr as usize;
    if crate::smp::cpu_area_virtual_region().contains(&vaddr) {
        vaddr - PERCPU_BASE
    } else if vaddr >= KERNEL_BASE {
        crate::mem::__kimage_va_to_pa(vaddr as *const u8)
    } else if vaddr >= PHYS_VIRT_OFFSET {
        vaddr - PHYS_VIRT_OFFSET
    } else {
        vaddr
    }
}
