use ax_plat::mem::{Aligned4K, pa};

use crate::config::plat::{BOOT_STACK_SIZE, PHYS_VIRT_OFFSET};

#[unsafe(link_section = ".bss.stack")]
static mut BOOT_STACK: [u8; BOOT_STACK_SIZE] = [0; BOOT_STACK_SIZE];

#[unsafe(link_section = ".data")]
static mut BOOT_PT_SV39: Aligned4K<[u64; 512]> = Aligned4K::new([0; 512]);

const PTE_FLAGS: u64 = 0xef; // VRWX_GAD
const PTE_SO: u64 = 1_u64 << 63;
const PTE_CACHE: u64 = 1_u64 << 62;
const PTE_BUF: u64 = 1_u64 << 61;
const PTE_SHARE: u64 = 1_u64 << 60;

const MEMORY_FLAGS: u64 = PTE_FLAGS | PTE_CACHE | PTE_BUF | PTE_SHARE;
const IOREMAP_FLAGS: u64 = PTE_FLAGS | PTE_SO | PTE_SHARE;

const fn sv39_l1_entry(paddr: usize, flags: u64) -> u64 {
    (((paddr >> 12) as u64) << 10) | flags
}

const fn high_map_index(paddr: usize) -> usize {
    ((PHYS_VIRT_OFFSET + paddr) >> 30) & 0x1ff
}

unsafe fn init_boot_page_table() {
    unsafe {
        // Direct mappings keep early code running after enabling MMU.
        BOOT_PT_SV39[0] = sv39_l1_entry(0x0000_0000, MEMORY_FLAGS);
        BOOT_PT_SV39[1] = sv39_l1_entry(0x4000_0000, MEMORY_FLAGS);
        BOOT_PT_SV39[2] = sv39_l1_entry(0x8000_0000, IOREMAP_FLAGS);
        BOOT_PT_SV39[0x3c] = sv39_l1_entry(0xf000_00000, IOREMAP_FLAGS);

        // High-half linear mappings used by phys_to_virt().
        BOOT_PT_SV39[high_map_index(0x0000_0000)] = sv39_l1_entry(0x0000_0000, MEMORY_FLAGS);
        BOOT_PT_SV39[high_map_index(0x4000_0000)] = sv39_l1_entry(0x4000_0000, MEMORY_FLAGS);
        BOOT_PT_SV39[high_map_index(0x8000_0000)] = sv39_l1_entry(0x8000_0000, IOREMAP_FLAGS);
        BOOT_PT_SV39[high_map_index(0xf000_00000)] = sv39_l1_entry(0xf000_00000, IOREMAP_FLAGS);
    }
}

unsafe fn init_mmu() {
    unsafe {
        ax_cpu::asm::write_kernel_page_table(pa!(&raw const BOOT_PT_SV39 as usize));
        ax_cpu::asm::flush_tlb(None);
    }
}

/// The earliest entry point for the primary CPU.
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
unsafe extern "C" fn _start() -> ! {
    // PC = 0x0820_0000
    // a0 = hartid
    // a1 = dtb
    core::arch::naked_asm!("
        mv      s0, a0                  // save hartid
        mv      s1, a1                  // save DTB pointer
        la      sp, {boot_stack}
        li      t0, {boot_stack_size}
        add     sp, sp, t0              // setup boot stack

        call    {init_boot_page_table}
        call    {init_mmu}              // setup boot page table and enabel MMU

        li      s2, {phys_virt_offset}  // fix up virtual high address
        add     sp, sp, s2

        mv      a0, s0
        mv      a1, s1
        la      a2, {entry}
        add     a2, a2, s2
        jalr    a2                      // call_main(cpu_id, dtb)
        j       .",
        phys_virt_offset = const PHYS_VIRT_OFFSET,
        boot_stack_size = const BOOT_STACK_SIZE,
        boot_stack = sym BOOT_STACK,
        init_boot_page_table = sym init_boot_page_table,
        init_mmu = sym init_mmu,
        entry = sym ax_plat::call_main,
    )
}

/// The earliest entry point for secondary CPUs.
#[cfg(feature = "smp")]
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn _start_secondary() -> ! {
    // a0 = hartid
    // a1 = SP
    core::arch::naked_asm!("
        mv      s0, a0                  // save hartid
        mv      sp, a1                  // set SP

        call    {init_mmu}              // setup boot page table and enabel MMU

        li      s1, {phys_virt_offset}  // fix up virtual high address
        add     a1, a1, s1
        add     sp, sp, s1

        mv      a0, s0
        la      a1, {entry}
        add     a1, a1, s1
        jalr    a1                      // call_secondary_main(cpu_id)
        j       .",
        phys_virt_offset = const PHYS_VIRT_OFFSET,
        init_mmu = sym init_mmu,
        entry = sym ax_plat::call_secondary_main,
    )
}
