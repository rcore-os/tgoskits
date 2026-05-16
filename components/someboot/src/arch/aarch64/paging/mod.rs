use core::arch::asm;

use aarch64_cpu::asm::barrier::{self, dsb, isb};
use num_align::NumAlign;
use page_table_generic::{MapConfig, MemAttributes, PteConfig};

#[cfg(not(feature = "hv"))]
use crate::arch::elx::set_user_table;
use crate::{
    arch::elx::{flush_tlb, set_kernal_table, setup_sctlr, setup_table_regs},
    console::print_mapping,
    mem::{__kimage_va, __percpu, __va, MB, PageTableInfo, page_size},
    smp::PerCpuMeta,
};

mod pte;

pub use pte::{Entry, Generic};

pub fn enable_mmu() -> ! {
    if let Err(e) = setup_page_table() {
        println!("Failed to setup page table: {:?}", e);
        panic!();
    }
    // Use physical address to avoid virtual address mapping issues
    let mmu_entry_phys = super::entry::mmu_entry as *const () as usize;
    println!("MMU Entry point at physical address: {:#x}", mmu_entry_phys);

    let meta = crate::smp::cpu_meta(crate::smp::cpu_idx()).unwrap();
    let v_sp = meta.stack_top_virt;
    let v_entry = __kimage_va(mmu_entry_phys) as usize;

    println!("Enabling MMU...");
    setup_sctlr();
    println!("MMU enabled, jumping to {v_entry:#x}, sp={v_sp:#x}");
    crate::mem::mmu::set_mmu_enabled();

    super::relocate::reset();
    dsb(barrier::SY);
    isb(barrier::SY);

    // Jump to mmu_entry using physical address
    unsafe {
        asm!(
            "
            mov x8, {0}
            mov x9, {1}
            mov sp, x9
            br x8
        ",
            in(reg) v_entry,
            in(reg) v_sp,
            options(noreturn, nostack)
        )
    }
}

pub fn init_mmu_secondary(cpu_meta_paddr: usize) -> usize {
    let meta = unsafe { &*(cpu_meta_paddr as *const PerCpuMeta) };

    setup_table_regs();
    let tb = PageTableInfo {
        asid: 0,
        addr: meta.boot_table_paddr,
    };
    set_kernal_table(tb);
    #[cfg(not(feature = "hv"))]
    set_user_table(tb);
    setup_sctlr();
    flush_tlb(None);
    dsb(barrier::SY);
    isb(barrier::SY);
    cpu_meta_paddr
}

fn setup_page_table() -> anyhow::Result<()> {
    println!("Mapping early memory regions...");

    let k_start = crate::mem::kimage_range().start;

    let mut table = crate::mem::mmu::new_boot_table();

    let pte = PteConfig {
        valid: true,
        read: true,
        writable: true,
        executable: true,
        mem_attr: MemAttributes::Normal,
        ..Default::default()
    };

    for memory in crate::fdt::memories() {
        let start = memory.start;
        let size = memory.len();
        if size == 0 {
            continue;
        }

        print_mapping("Ram", __va(start) as _, start, size);

        table.map(&MapConfig {
            vaddr: start.into(),
            paddr: start.into(),
            size,
            pte,
            allow_huge: true,
            flush: false,
        })?;
    }
    let v_start = __kimage_va(k_start);
    let size = crate::mem::kimage_range().len().align_up(2 * MB);

    print_mapping("KImage", v_start as _, k_start, size);

    table.map(&MapConfig {
        vaddr: v_start.into(),
        paddr: k_start.into(),
        size,
        pte,
        allow_huge: true,
        flush: false,
    })?;

    let percpu_range = crate::smp::percpu_range();
    print_mapping(
        "PerCpu",
        __percpu(percpu_range.start) as _,
        percpu_range.start,
        percpu_range.len(),
    );

    table
        .map(&MapConfig {
            vaddr: __percpu(percpu_range.start).into(),
            paddr: percpu_range.start.into(),
            size: percpu_range.len(),
            pte: PteConfig {
                valid: true,
                read: true,
                writable: true,
                executable: true,
                mem_attr: MemAttributes::PerCpu,
                ..Default::default()
            },
            allow_huge: true,
            flush: false,
        })
        .unwrap();

    let debug_base = unsafe { crate::console::DEBUG_BASE };
    if debug_base != 0 {
        let start = debug_base.align_down(page_size());
        let size = page_size();
        let pte = PteConfig {
            valid: true,
            read: true,
            writable: true,
            executable: false,
            mem_attr: MemAttributes::Device,
            ..Default::default()
        };

        print_mapping("Debug serial", __va(start) as _, start, size);

        table.map(&MapConfig {
            vaddr: start.into(),
            paddr: start.into(),
            size,
            pte,
            allow_huge: true,
            flush: false,
        })?;
    }

    let tb_addr = table.root_paddr();

    println!("Boot page table at physical address: {:#x}", tb_addr);
    crate::mem::mmu::set_boot_table(table);
    println!("Setting up table registers...");

    setup_table_regs();
    let tb = PageTableInfo {
        asid: 0,
        addr: tb_addr.into(),
    };
    set_kernal_table(tb);
    #[cfg(not(feature = "hv"))]
    set_user_table(tb);
    flush_tlb(None);

    Ok(())
}
