use crate::config::plat::{BOOT_STACK_SIZE, PHYS_VIRT_OFFSET};
use ax_plat::mem::{Aligned4K, pa};

#[unsafe(link_section = ".bss.stack")]
static mut BOOT_STACK: [u8; BOOT_STACK_SIZE] = [0; BOOT_STACK_SIZE];

#[unsafe(link_section = ".data")]
static mut BOOT_PT_SV39: Aligned4K<[u64; 512]> = Aligned4K::new([0; 512]);

const DTB_HEADER_MAGIC: u32 = 0xd00d_feed;
const DTB_TOTAL_SIZE_OFFSET: usize = 4;
const DTB_RELOC_BUF_SIZE: usize = 0x40_000;

#[unsafe(link_section = ".data")]
static mut DTB_RELOC_BUF: Aligned4K<[u8; DTB_RELOC_BUF_SIZE]> =
    Aligned4K::new([0; DTB_RELOC_BUF_SIZE]);

#[allow(clippy::identity_op)] // (0x0 << 10) here makes sense because it's an address
unsafe fn init_boot_page_table() {
    unsafe {
        // 0x0000_0000..0x4000_0000, VRWX_GAD, 1G block
        BOOT_PT_SV39[0] = (0x0 << 10) | 0xef;
        // 0x8000_0000..0xc000_0000, VRWX_GAD, 4G block
        BOOT_PT_SV39[2] = (0x80000 << 10) | 0xef;
        BOOT_PT_SV39[3] = (0xC0000 << 10) | 0xef;
        BOOT_PT_SV39[4] = (0x100000 << 10) | 0xef;
        BOOT_PT_SV39[5] = (0x140000 << 10) | 0xef;
        // 0xffff_ffc0_0000_0000..0xffff_ffc0_4000_0000, VRWX_GAD, 1G block
        BOOT_PT_SV39[0x100] = (0x0 << 10) | 0xef;
        // 0xffff_ffc0_8000_0000..0xffff_ffc0_c000_0000, VRWX_GAD, 1G block
        BOOT_PT_SV39[0x102] = (0x80000 << 10) | 0xef;
        BOOT_PT_SV39[0x103] = (0xC0000 << 10) | 0xef;
        BOOT_PT_SV39[0x104] = (0x100000 << 10) | 0xef;
        BOOT_PT_SV39[0x105] = (0x140000 << 10) | 0xef;
    }
}

unsafe fn init_mmu() {
    unsafe {
        ax_cpu::asm::write_kernel_page_table(pa!(&raw const BOOT_PT_SV39 as usize));
        ax_cpu::asm::flush_tlb(None);
    }
}

unsafe fn relocate_dtb(dtb_paddr: usize) -> usize {
    if dtb_paddr == 0 {
        return 0;
    }

    let header_ptr = dtb_paddr as *const u8;
    let magic = unsafe { core::ptr::read_unaligned(header_ptr.cast::<u32>()) };
    let total_size =
        unsafe { core::ptr::read_unaligned(header_ptr.add(DTB_TOTAL_SIZE_OFFSET).cast::<u32>()) };

    let magic = u32::from_be(magic);
    let total_size = u32::from_be(total_size) as usize;

    assert_eq!(magic, DTB_HEADER_MAGIC, "invalid DTB magic: {magic:#x}");
    assert!(
        total_size <= DTB_RELOC_BUF_SIZE,
        "DTB too large: {total_size:#x} > buffer {DTB_RELOC_BUF_SIZE:#x}"
    );

    unsafe {
        core::ptr::copy_nonoverlapping(
            header_ptr,
            (&raw mut DTB_RELOC_BUF).cast::<u8>(),
            total_size,
        );
    }

    &raw const DTB_RELOC_BUF as usize
}

/// The earliest entry point for the primary CPU.
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
unsafe extern fn _start() -> ! {
    // PC = 0x8020_0000
    // a0 = hartid
    // a1 = dtb
    core::arch::naked_asm!("
        mv      s0, a0                  // save hartid
        mv      s1, a1                  // save DTB pointer
        la      sp, {boot_stack}
        li      t0, {boot_stack_size}
        add     sp, sp, t0              // setup boot stack

        mv      a0, s1
        call    {relocate_dtb}
        mv      s1, a0

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
        relocate_dtb = sym relocate_dtb,
        entry = sym ax_plat::call_main,
    )
}

/// The earliest entry point for secondary CPUs.
#[cfg(feature = "smp")]
#[unsafe(naked)]
pub(crate) unsafe extern fn _start_secondary() -> ! {
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
