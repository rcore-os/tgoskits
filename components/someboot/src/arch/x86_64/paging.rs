use core::arch::asm;

use num_align::NumAlign;
use page_table_generic::{MapConfig, MemAttributes, PteConfig, TableMeta, VirtAddr};
use x86::{
    controlregs::{self, Cr0, Cr4},
    msr::{rdmsr, wrmsr},
    tlb,
};

use crate::{
    arch::addrspace::{KERNEL_BASE, PERCPU_BASE},
    console::print_mapping,
    mem::{__kimage_va, __percpu, PageTableInfo, page_size},
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
    fn from_config(config: PteConfig) -> Self {
        let mut bits = (config.paddr.raw() as u64) & PTE_ADDR_MASK;
        if config.valid {
            bits |= PTE_PRESENT;
        }
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
        if config.is_dir && config.huge {
            bits |= PTE_HUGE;
        }
        match config.mem_attr {
            MemAttributes::Device | MemAttributes::Uncached => {
                bits |= PTE_CACHE_DISABLE | PTE_WRITE_THROUGH;
            }
            _ => {}
        }
        // For x86_64, NX on non-leaf entries blocks execution for the whole
        // covered range. Only apply NX on leaf mappings (PTE or huge page).
        let is_leaf = !config.is_dir || config.huge;
        if is_leaf && !config.executable {
            bits |= PTE_NO_EXECUTE;
        }
        if config.valid {
            bits |= PTE_ACCESSED;
        }
        Self(bits)
    }

    fn to_config(&self, is_dir: bool) -> PteConfig {
        let huge = is_dir && (self.0 & PTE_HUGE) != 0;
        let mem_attr = if (self.0 & (PTE_CACHE_DISABLE | PTE_WRITE_THROUGH)) != 0 {
            MemAttributes::Device
        } else {
            MemAttributes::Normal
        };
        PteConfig {
            paddr: ((self.0 & PTE_ADDR_MASK) as usize).into(),
            valid: (self.0 & PTE_PRESENT) != 0,
            read: (self.0 & PTE_PRESENT) != 0,
            writable: (self.0 & PTE_WRITABLE) != 0,
            executable: (self.0 & PTE_NO_EXECUTE) == 0,
            lower: (self.0 & PTE_USER) != 0,
            dirty: (self.0 & PTE_DIRTY) != 0,
            global: (self.0 & PTE_GLOBAL) != 0,
            is_dir,
            huge,
            mem_attr,
        }
    }

    fn valid(&self) -> bool {
        (self.0 & PTE_PRESENT) != 0
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
                tlb::flush(vaddr.raw());
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

    let meta = crate::smp::cpu_meta(crate::smp::cpu_idx()).unwrap();
    let v_sp = meta.stack_top_virt;
    let v_entry = __kimage_va(super::entry::mmu_entry as *const () as usize) as usize;

    crate::mem::mmu::set_mmu_enabled();

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
            valid: true,
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
    }

    let lapic_base = (unsafe { rdmsr(x86::msr::IA32_APIC_BASE) } as usize) & !(page_size() - 1);
    let lapic_mapped = crate::mem::memory_map().iter().any(|region| {
        let start = region.physical_start;
        let end = start.saturating_add(region.size_in_bytes);
        (start..end).contains(&lapic_base)
    });
    if !lapic_mapped {
        print_mapping("LAPIC", lapic_base, lapic_base, page_size());
        table.map(&MapConfig {
            vaddr: lapic_base.into(),
            paddr: lapic_base.into(),
            size: page_size(),
            pte: PteConfig {
                valid: true,
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
                valid: true,
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
        vaddr: kimage_vaddr.into(),
        paddr: kimage.start.into(),
        size: kimage_size,
        pte: PteConfig {
            valid: true,
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

    let percpu = crate::smp::percpu_range();
    print_mapping(
        "PerCpu",
        __percpu(percpu.start) as _,
        percpu.start,
        percpu.len(),
    );
    table.map(&MapConfig {
        vaddr: __percpu(percpu.start).into(),
        paddr: percpu.start.into(),
        size: percpu.len(),
        pte: PteConfig {
            valid: true,
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
    enable_page_features();
    super::trap::set_cr3(root);
    Ok(())
}

fn enable_page_features() {
    unsafe {
        let cr0 = controlregs::cr0() | Cr0::CR0_WRITE_PROTECT;
        controlregs::cr0_write(cr0);

        let cr4 = controlregs::cr4() | Cr4::CR4_ENABLE_GLOBAL_PAGES;
        controlregs::cr4_write(cr4);

        let efer = rdmsr(IA32_EFER) | IA32_EFER_NXE;
        wrmsr(IA32_EFER, efer);
    }
}

pub fn current_table() -> PageTableInfo {
    PageTableInfo {
        asid: 0,
        addr: super::trap::current_cr3().raw(),
    }
}

pub fn set_table(info: PageTableInfo) {
    super::trap::set_cr3(info.addr.into());
}

pub fn virt_to_phys(vaddr: *const u8) -> usize {
    let vaddr = vaddr as usize;
    if crate::smp::percpu_va_range().contains(&vaddr) {
        vaddr - PERCPU_BASE
    } else if vaddr >= KERNEL_BASE {
        crate::mem::__kimage_va_to_pa(vaddr as *const u8)
    } else {
        vaddr
    }
}
