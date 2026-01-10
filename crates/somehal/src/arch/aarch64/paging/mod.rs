use core::arch::asm;

use num_align::NumAlign;
use page_table_generic::{MapConfig, MemAttributes, PageTableEntry};

use crate::{
    arch::elx::{flush_tlb, set_kernal_table, set_user_table, setup_sctlr, setup_table_regs},
    console::print_mapping,
    mem::{__kimage_va, __va, MB, PageTableInfo, page_size, vm_load_offset},
};

mod pte;

pub use pte::Entry;
pub use pte::Generic;

pub(crate) fn _pa(vaddr: *const u8) -> usize {
    (vaddr as usize as isize + vm_load_offset()) as usize
}

pub fn enable_mmu() -> ! {
    println!("Mapping early memory regions...");

    let k_start = crate::mem::kimage_range().start;

    let mut table = crate::mem::mmu::new_boot_table();

    let mut pte = Entry::new_valid();
    pte.set_writable(true);
    pte.set_executable(true);
    pte.set_mem_attr(MemAttributes::Normal);

    for memory in crate::fdt::memories() {
        let start = memory.start;
        let size = memory.len();

        print_mapping("Ram", __va(start) as _, start, size);

        table
            .map(&MapConfig {
                vaddr: start.into(),
                paddr: start.into(),
                size,
                pte,
                allow_huge: true,
                flush: false,
            })
            .unwrap();
    }
    let v_start = __kimage_va(k_start);
    let size = crate::mem::kimage_range().len().align_up(2 * MB);

    print_mapping("KImage", v_start as _, k_start, size);

    table
        .map(&MapConfig {
            vaddr: v_start.into(),
            paddr: k_start.into(),
            size,
            pte,
            allow_huge: true,
            flush: false,
        })
        .unwrap();

    let debug_base = unsafe { crate::console::DEBUG_BASE };
    if debug_base != 0 {
        let start = debug_base.align_down(page_size());
        let size = page_size();
        let mut pte = Entry::new_valid();
        pte.set_writable(true);
        pte.set_executable(false);
        pte.set_mem_attr(MemAttributes::Device);

        print_mapping("Debug serial", __va(start) as _, start, size);

        table
            .map(&MapConfig {
                vaddr: start.into(),
                paddr: start.into(),
                size,
                pte,
                allow_huge: true,
                flush: false,
            })
            .unwrap();
    }

    let tb_addr = table.root_paddr();
    crate::mem::mmu::set_boot_table(table);

    println!("Boot page table at physical address: {:#x}", tb_addr);

    // Use physical address to avoid virtual address mapping issues
    let mmu_entry_phys = super::entry::mmu_entry as *const () as usize;
    println!("MMU Entry point at physical address: {:#x}", mmu_entry_phys);
    setup_table_regs();
    let tb = PageTableInfo {
        asid: 0,
        addr: tb_addr.into(),
    };
    set_kernal_table(tb);
    set_user_table(tb);
    flush_tlb(None);

    let v_sp = __kimage_va(ext_sym_addr!(__cpu0_stack_top)) as usize;
    let v_entry = __kimage_va(mmu_entry_phys) as usize;

    println!("Enabling MMU...");
    setup_sctlr();
    println!("MMU enabled, jumping to {v_entry:#x}, sp={v_sp:#x}");
    crate::mem::mmu::set_mmu_enabled();
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
