//! Helper functions to initialize the CPU states on systems bootstrapping.

use loongArch64::register::{crmd, stlbps, tlbidx, tlbrehi, tlbrentry};
use memory_addr::PhysAddr;
use page_table_multiarch::loongarch64::LA64MetaData;

/// Initializes TLB and MMU related registers on the current CPU.
///
/// It sets the TLB Refill exception entry (`TLBRENTY`), page table root address,
/// and finally enables the mapped address translation mode.
///
/// - TLBRENTY: <https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#tlb-refill-exception-entry-base-address>
/// - CRMD: <https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#current-mode-information>
pub fn init_mmu(root_paddr: PhysAddr, phys_virt_offset: usize) {
    unsafe extern "C" {
        fn handle_tlb_refill();
    }

    // Configure TLB
    const PS_4K: usize = 0x0c; // Page Size 4KB
    let tlbrentry_paddr = pa!(handle_tlb_refill as usize - phys_virt_offset);
    tlbidx::set_ps(PS_4K);
    stlbps::set_ps(PS_4K);
    tlbrehi::set_ps(PS_4K);
    tlbrentry::set_tlbrentry(tlbrentry_paddr.as_usize());

    // Configure page table walking
    unsafe {
        crate::asm::write_pwc(LA64MetaData::PWCL_VALUE, LA64MetaData::PWCH_VALUE);
        crate::asm::write_kernel_page_table(root_paddr);
        crate::asm::write_user_page_table(pa!(0));
    }
    crate::asm::flush_tlb(None);

    // Enable mapped address translation mode
    crmd::set_pg(true);
}

/// Initializes trap handling on the current CPU.
///
/// `cpu_id` indicates the CPU ID of the current CPU.
///
/// In detail, it initializes the exception vector on LoongArch64 platforms.
pub fn init_trap(_cpu_id: usize) {
    unsafe extern "C" {
        fn exception_entry_base();
    }
    unsafe {
        crate::asm::write_exception_entry_base(exception_entry_base as usize);
    }
}
