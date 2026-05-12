use core::{arch::naked_asm, mem::offset_of};

use aarch64_cpu::registers::{CurrentEL, Readable};

use super::{switch_to_elx, switch_to_elx_secondary};
use crate::{
    arch::{elx, paging::init_mmu_secondary},
    consts::VM_LOAD_ADDRESS,
    entry::PrimaryCpuInitInfo,
    smp::PerCpuMeta,
};

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kernel_entry(_fdt_addr: usize) {
    naked_asm!(
        "mov  x9,  x0",

        // Clear BSS section from __bss_start to __bss_stop
        asm_sym_addr!(x0, "__bss_start"),
        asm_sym_addr!(x1, "__bss_stop"),
        "mov x2, #0",        // Zero value to store
        "1:",
        "cmp x0, x1",        // Compare current address with end
        "b.eq 2f",           // If reached end, exit loop
        "str x2, [x0], #8",  // Store zero and advance by 8 bytes
        "b 1b",              // Loop back
        "2:",

        asm_sym_addr!(x8, "{fdt}"),
        "str  x9, [x8]",

        asm_sym_addr!(x8, "__cpu0_stack_top"),
        "mov sp, x8",

        "bl {switch_to_elx}",
        fdt = sym crate::fdt::FDT_ADDR,
        switch_to_elx = sym switch_to_elx,

    )
}

pub fn el_entry() -> ! {
    super::relocate::apply();
    super::trap::setup();

    let kernel_code_start_lma = ext_sym_addr!(_head);
    let kernel_code_end_lma = ext_sym_addr!(__kernel_code_end);

    crate::entry::primary_init_early(PrimaryCpuInitInfo {
        kernel_start: kernel_code_start_lma.into(),
        kernel_end: kernel_code_end_lma.into(),
        kernel_start_link: VM_LOAD_ADDRESS.into(),
    });

    println!("EL: {}", CurrentEL.read(CurrentEL::EL));

    crate::arch::paging::enable_mmu()
}

pub(crate) fn mmu_entry() -> ! {
    println!("Disable user page table");
    #[cfg(uspace)]
    elx::set_user_table(kernutil::memory::PageTableInfo::zero());
    elx::flush_tlb(None);
    super::trap::setup();

    // crate::mem::reset_memory_map();
    crate::arch::relocate::reset();
    crate::prime_entry()
}

#[unsafe(naked)]
pub(crate) unsafe extern "C" fn _secondary_entry(_arg: usize) -> ! {
    naked_asm!(
        "mov x20, x0",
        "ldr x1, [x20, {stack_top_offset}]",
        "mov sp, x1",
        "mov x0, x20",
        "bl {switch_to_elx_secondary}",
        switch_to_elx_secondary = sym switch_to_elx_secondary,
        stack_top_offset = const offset_of!(crate::smp::PerCpuMeta, stack_top),
    )
}

#[unsafe(naked)]
pub(crate) unsafe extern "C" fn secondary_el_entry(_cpu_meta_paddr: usize) -> ! {
    naked_asm!(
        "bl {init_mmu}",
        "ldr x8, [x20, {stack_top_virt_offset}]",
        "mov sp, x8",
        "ldr x8, [x20, {entry_offset}]",
        "blr x8",
        "b .",
        init_mmu = sym init_mmu_secondary,
        stack_top_virt_offset = const offset_of!(PerCpuMeta, stack_top_virt),
        entry_offset = const offset_of!(PerCpuMeta, entry_virt),
    )
}
