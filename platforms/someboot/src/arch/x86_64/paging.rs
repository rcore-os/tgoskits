use core::arch::asm;

use num_align::NumAlign;
use page_table_generic::{MapConfig, TableMeta, VirtAddr};
use x86::{
    controlregs::{self, Cr0, Cr4},
    msr::{rdmsr, wrmsr},
    tlb,
};

use crate::{
    arch::addrspace::{KERNEL_BASE, PERCPU_BASE, PHYS_VIRT_OFFSET},
    console::print_mapping,
    mem::{__kimage_va, MemAttributes, PageTableInfo, PteConfig, cpu_area_phys_to_virt, page_size},
};

const IA32_EFER: u32 = 0xc000_0080;
const IA32_EFER_NXE: u64 = 1 << 11;

const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITABLE: u64 = 1 << 1;
const PTE_USER: u64 = 1 << 2;
const PTE_WRITE_THROUGH: u64 = 1 << 3;
const PTE_CACHE_DISABLE: u64 = 1 << 4;
const PTE_ACCESSED: u64 = 1 << 5;
const PTE_DIRTY: u64 = 1 << 6;
const PTE_HUGE: u64 = 1 << 7;
const PTE_GLOBAL: u64 = 1 << 8;
const PTE_NO_EXECUTE: u64 = 1 << 63;
const PTE_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

#[derive(Clone, Copy, Debug, Default)]
pub struct Entry(u64);

impl page_table_generic::PageTableEntry for Entry {
    type PteConfig = PteConfig;

    fn new_page(
        paddr: page_table_generic::PhysAddr,
        config: Self::PteConfig,
        is_huge: bool,
    ) -> Self {
        let mut bits = (paddr.as_usize() as u64) & PTE_ADDR_MASK;
        bits |= PTE_PRESENT;
        if config.writable {
            bits |= PTE_WRITABLE;
        }
        if config.lower {
            bits |= PTE_USER;
        }
        if config.dirty {
            bits |= PTE_DIRTY;
        }
        if config.global {
            bits |= PTE_GLOBAL;
        }
        if is_huge {
            bits |= PTE_HUGE;
        }
        match config.mem_attr {
            MemAttributes::Device | MemAttributes::Uncached => {
                bits |= PTE_CACHE_DISABLE | PTE_WRITE_THROUGH;
            }
            _ => {}
        }
        if !config.executable {
            bits |= PTE_NO_EXECUTE;
        }
        bits |= PTE_ACCESSED;
        Self(bits)
    }

    fn new_table(paddr: page_table_generic::PhysAddr) -> Self {
        Self((paddr.as_usize() as u64 & PTE_ADDR_MASK) | PTE_PRESENT | PTE_WRITABLE | PTE_ACCESSED)
    }

    fn paddr(&self, _is_dir: bool) -> page_table_generic::PhysAddr {
        ((self.0 & PTE_ADDR_MASK) as usize).into()
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let mem_attr = if (self.0 & (PTE_CACHE_DISABLE | PTE_WRITE_THROUGH)) != 0 {
            MemAttributes::Device
        } else {
            MemAttributes::Normal
        };
        PteConfig {
            read: (self.0 & PTE_PRESENT) != 0,
            writable: (self.0 & PTE_WRITABLE) != 0,
            executable: (self.0 & PTE_NO_EXECUTE) == 0,
            lower: (self.0 & PTE_USER) != 0,
            dirty: (self.0 & PTE_DIRTY) != 0,
            global: (self.0 & PTE_GLOBAL) != 0,
            mem_attr,
        }
    }

    fn present(&self) -> bool {
        (self.0 & PTE_PRESENT) != 0
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir && (self.0 & PTE_HUGE) != 0
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
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
        unsafe {
            if let Some(vaddr) = vaddr {
                tlb::flush(vaddr.as_usize());
            } else {
                tlb::flush_all();
            }
        }
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

    unsafe {
        asm!(
            "mov rsp, {sp}",
            "jmp {entry}",
            sp = in(reg) v_sp,
            entry = in(reg) v_entry,
            options(noreturn)
        );
    }
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
    enable_no_execute();
    super::trap::set_cr3(root);
    enable_page_features();
    Ok(())
}

fn enable_no_execute() {
    unsafe {
        let efer = rdmsr(IA32_EFER) | IA32_EFER_NXE;
        wrmsr(IA32_EFER, efer);
    }
}

fn enable_page_features() {
    unsafe {
        let cr0 = controlregs::cr0() | Cr0::CR0_WRITE_PROTECT;
        controlregs::cr0_write(cr0);

        let cr4 = controlregs::cr4() | Cr4::CR4_ENABLE_GLOBAL_PAGES;
        controlregs::cr4_write(cr4);
    }
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
