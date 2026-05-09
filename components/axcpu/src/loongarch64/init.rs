//! Helper functions to initialize the CPU states on systems bootstrapping.

use ax_memory_addr::PhysAddr;
use ax_page_table_multiarch::loongarch64::LA64MetaData;
use loongArch64::register::{MemoryAccessType, crmd, stlbps, tlbidx, tlbrehi, tlbrentry};

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
    let tlbrentry_paddr = pa!(handle_tlb_refill as *const () as usize - phys_virt_offset);
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

    // Update CRMD in one CSR write so we do not create a transient state where
    // direct mapping is disabled before paged translation is fully enabled.
    // Larger images such as the LoongArch C memtest can place the following
    // instructions across a page boundary, making that transition window
    // observable as an instruction-fetch fault at the next page.
    let mut crmd_bits = crmd::read().raw();
    crmd_bits &= !(1 << 3); // DA
    crmd_bits |= 1 << 4; // PG
    crmd_bits &= !(0b11 << 5);
    crmd_bits |= (MemoryAccessType::CoherentCached as usize) << 5; // DATF
    crmd_bits &= !(0b11 << 7);
    crmd_bits |= (MemoryAccessType::CoherentCached as usize) << 7; // DATM
    unsafe {
        core::arch::asm!("csrwr {}, {}", in(reg) crmd_bits, const 0x0);
    }
}

/// Initializes trap handling on the current CPU.
///
/// In detail, it initializes the exception vector on LoongArch64 platforms.
pub fn init_trap() {
    #[cfg(feature = "uspace")]
    crate::uspace_common::init_exception_table();
    unsafe {
        unsafe extern "C" {
            fn exception_entry_base();
        }
        core::arch::asm!(include_asm_macros!(), "csrwr $r0, KSAVE_KSP");
        crate::asm::write_exception_entry_base(exception_entry_base as *const () as usize);
    }
}
