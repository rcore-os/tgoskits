use core::arch::naked_asm;

use super::switch_to_elx;

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kernel_entry(_fdt_addr: usize) -> ! {
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
    crate::fdt::setup_earlycon();
    if let Some(cmdline) = crate::cmdline::cmdline() {
        println!("{cmdline}");
    }
    println!("Hello, Somehal on AArch64!");

    loop {}
}
