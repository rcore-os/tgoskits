use axplat::mem::{Aligned4K, pa, va};
use page_table_entry::{GenericPTE, MappingFlags, loongarch64::LA64PTE};

use crate::config::plat::{BOOT_STACK_SIZE, PHYS_BOOT_OFFSET, PHYS_VIRT_OFFSET};

#[unsafe(link_section = ".bss.stack")]
static mut BOOT_STACK: [u8; BOOT_STACK_SIZE] = [0; BOOT_STACK_SIZE];

#[unsafe(link_section = ".data")]
static mut BOOT_PT_L0: Aligned4K<[LA64PTE; 512]> = Aligned4K::new([LA64PTE::empty(); 512]);

#[unsafe(link_section = ".data")]
static mut BOOT_PT_L1: Aligned4K<[LA64PTE; 512]> = Aligned4K::new([LA64PTE::empty(); 512]);

#[unsafe(link_section = ".data")]
static mut BOOT_PT_L2: Aligned4K<[LA64PTE; 512]> = Aligned4K::new([LA64PTE::empty(); 512]);

unsafe fn init_boot_page_table() {
    unsafe {
        let l1_va = va!(&raw const BOOT_PT_L1 as usize);
        // 0x0000_0000_0000 ~ 0x0080_0000_0000, table
        BOOT_PT_L0[0x100] = LA64PTE::new_table(axplat::mem::virt_to_phys(l1_va));
        let l2_va = va!(&raw const BOOT_PT_L2 as usize);
        // 0x0000_0000..0x4000_0000, table
        BOOT_PT_L1[0] = LA64PTE::new_table(axplat::mem::virt_to_phys(l2_va));
        for i in 0..512 {
            BOOT_PT_L2[i] = LA64PTE::new_page(
                pa!(i << 21),
                MappingFlags::READ
                    | MappingFlags::WRITE
                    | if i < 128 {
                        MappingFlags::EXECUTE
                    } else {
                        MappingFlags::DEVICE
                    },
                true,
            );
        }
        // 0x8000_0000..0xc000_0000, VPWXGD, 1G block
        BOOT_PT_L1[0x2] = LA64PTE::new_page(
            pa!(0x8000_0000),
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE,
            true,
        );
    }
}

fn enable_fp_simd() {
    // FP/SIMD needs to be enabled early, as the compiler may generate SIMD
    // instructions in the bootstrapping code to speed up the operations
    // like `memset` and `memcpy`.
    #[cfg(feature = "fp-simd")]
    {
        axcpu::asm::enable_fp();
        axcpu::asm::enable_lsx();
    }
}

fn init_mmu() {
    axcpu::init::init_mmu(
        axplat::mem::virt_to_phys(va!(&raw const BOOT_PT_L0 as usize)),
        PHYS_BOOT_OFFSET,
    );
}

const BOOT_TO_VIRT: usize = PHYS_VIRT_OFFSET - PHYS_BOOT_OFFSET;

/// The earliest entry point for the primary CPU.
///
/// We can't use bl to jump to higher address, so we use jirl to jump to higher address.
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!("
        .word   0x5a4d              # MZ, MS-DOS header
        .word   0                   # Reserved
        .dword  0x00200040          # Kernel entry point
        .dword  _ekernel - _skernel # Kernel image effective size
        .dword  0x00200000          # Kernel image load offset from start of RAM
        .dword  0                   # Reserved
        .dword  0                   # Reserved
        .dword  0                   # Reserved
        .word   0x818223cd          # Magic number
        .word   0x0                 # Offset to the PE header

        # Setup DMW
        li.d        $t0, {phys_boot_offset} | 0x11
        csrwr       $t0, 0x180      # DMWIN0

        # Jump to DMW region
        la.local    $t0, 1f
        li.d        $t1, {phys_boot_offset}
        or          $t0, $t0, $t1
        jirl        $zero, $t0, 0

    1:
        # Setup Stack
        la.local    $sp, {boot_stack}
        li.d        $t0, {boot_stack_size}
        add.d       $sp, $sp, $t0       # setup boot stack

        # Init MMU
        bl          {enable_fp_simd}    # enable FP/SIMD instructions
        bl          {init_boot_page_table}
        bl          {init_mmu}          # setup boot page table and enable MMU

        # Adjust stack pointer
        li.d        $t0, {boot_to_virt}
        add.d       $sp, $sp, $t0

        csrrd       $a0, 0x20           # cpuid
        li.d        $a1, 0              # TODO: parse dtb
        la.abs      $t0, {entry}
        li.d        $ra, 0
        jirl        $zero, $t0, 0",

        phys_boot_offset = const PHYS_BOOT_OFFSET,
        boot_to_virt = const BOOT_TO_VIRT,

        boot_stack = sym BOOT_STACK,
        boot_stack_size = const BOOT_STACK_SIZE,
        enable_fp_simd = sym enable_fp_simd,
        init_boot_page_table = sym init_boot_page_table,
        init_mmu = sym init_mmu,
        entry = sym axplat::call_main,
    )
}

/// The earliest entry point for secondary CPUs.
#[cfg(feature = "smp")]
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn _start_secondary() -> ! {
    core::arch::naked_asm!("
        li.w        $t0,  0x1028        # LA_IOCSR_MAIL_BUF1
        iocsrrd.d   $sp,  $t0           # Load stack pointer

        # Setup DMW
        li.d        $t0, {phys_boot_offset} | 0x11
        csrwr       $t0, 0x180          # DMWIN0
        # Already in DMW region

        # Init MMU
        bl          {enable_fp_simd}    # enable FP/SIMD instructions
        bl          {init_mmu}          # setup boot page table and enable MMU

        # Adjust stack pointer
        li.d        $t0, {boot_to_virt}
        add.d       $sp, $sp, $t0

        csrrd       $a0, 0x20           # cpuid
        la.abs      $t0, {entry}
        jirl        $zero, $t0, 0",

        phys_boot_offset = const PHYS_BOOT_OFFSET,
        boot_to_virt = const BOOT_TO_VIRT,

        enable_fp_simd = sym enable_fp_simd,
        init_mmu = sym init_mmu,
        entry = sym axplat::call_secondary_main,
    )
}
