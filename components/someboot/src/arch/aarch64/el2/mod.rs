use aarch64_cpu::{
    asm::{barrier::*, *},
    registers::*,
};
use aarch64_cpu_ext::asm::tlb::*;
use page_table_generic::VirtAddr;

use crate::{arch::entry::el_entry, mem::PageTableInfo};

pub fn switch_to_elx() {
    unsafe extern "C" {
        fn __cpu0_stack_top();
    }

    SPSel.write(SPSel::SP::ELx);
    SP_EL0.set(0);
    let current_el = CurrentEL.read(CurrentEL::EL);

    if current_el >= 3 {
        let el_entry = sym_addr!(el_entry);
        let sp = sym_addr!(__cpu0_stack_top);

        if current_el == 3 {
            // Set EL2 to 64bit and enable the HVC instruction.
            SCR_EL3.write(
                SCR_EL3::NS::NonSecure + SCR_EL3::HCE::HvcEnabled + SCR_EL3::RW::NextELIsAarch64,
            );
            // Set the return address and exception level for EL2.
            SPSR_EL3.write(
                SPSR_EL3::M::EL2h           // Target EL2h mode
                    + SPSR_EL3::D::Masked   // Disable debug exceptions
                    + SPSR_EL3::A::Masked   // Disable async data abort
                    + SPSR_EL3::I::Masked   // Disable IRQ
                    + SPSR_EL3::F::Masked, // Disable FIQ
            );

            ELR_EL3.set(el_entry as _);
            SP_EL2.set(sp as _);
            barrier::isb(barrier::SY);
            eret();
        }
    }

    // Call el_entry directly if we're already in EL2
    el_entry();
}

pub fn switch_to_elx_secondary(cpu_meta_paddr: usize) -> ! {
    SPSel.write(SPSel::SP::ELx);
    SP_EL0.set(0);

    let current_el = CurrentEL.read(CurrentEL::EL);
    let secondary_entry = sym_addr!(crate::arch::entry::secondary_el_entry);
    let stack_top = unsafe { (cpu_meta_paddr as *const usize).read_volatile() };

    if current_el >= 3 {
        SCR_EL3.write(
            SCR_EL3::NS::NonSecure + SCR_EL3::HCE::HvcEnabled + SCR_EL3::RW::NextELIsAarch64,
        );
        SPSR_EL3.write(
            SPSR_EL3::M::EL2h
                + SPSR_EL3::D::Masked
                + SPSR_EL3::A::Masked
                + SPSR_EL3::I::Masked
                + SPSR_EL3::F::Masked,
        );
        ELR_EL3.set(secondary_entry as _);
        SP_EL2.set(stack_top as _);
        barrier::isb(barrier::SY);
        eret();
    }

    unsafe { crate::arch::entry::secondary_el_entry(cpu_meta_paddr) }
}

#[inline(always)]
pub fn flush_tlb(vaddr: Option<VirtAddr>) {
    match vaddr {
        Some(addr) => {
            // VAE2IS requires (asid, va), TTBR0_EL2 doesn't have ASID field, so use 0
            tlbi(VAE2IS::new(0, addr.raw()));
        }
        None => {
            tlbi(ALLE2);
        }
    }
    dsb(SY);
    isb(SY);
}

#[inline(always)]
pub fn setup_table_regs() {
    // Set EL1 to 64bit.
    // Enable `IMO` and `FMO` to make sure that:
    // * Physical IRQ interrupts are taken to EL2;
    // * Virtual IRQ interrupts are enabled;
    // * Physical FIQ interrupts are taken to EL2;
    // * Virtual FIQ interrupts are enabled.
    HCR_EL2.write(
        HCR_EL2::VM::Enable
            + HCR_EL2::RW::EL1IsAarch64
            + HCR_EL2::IMO::EnableVirtualIRQ // Physical IRQ Routing.
            + HCR_EL2::FMO::EnableVirtualFIQ // Physical FIQ Routing.
            + HCR_EL2::TSC::EnableTrapEl1SmcToEl2,
    );

    // Device-nGnRE
    let attr0 = MAIR_EL2::Attr0_Device::nonGathering_nonReordering_EarlyWriteAck;
    // Normal Write-Back
    let attr1 = MAIR_EL2::Attr1_Normal_Inner::WriteBack_NonTransient_ReadWriteAlloc
        + MAIR_EL2::Attr1_Normal_Outer::WriteBack_NonTransient_ReadWriteAlloc;
    // No cache
    let attr2 =
        MAIR_EL2::Attr2_Normal_Inner::NonCacheable + MAIR_EL2::Attr2_Normal_Outer::NonCacheable;
    // WriteThrough
    let attr3 = MAIR_EL2::Attr3_Normal_Inner::WriteThrough_Transient_WriteAlloc
        + MAIR_EL2::Attr3_Normal_Outer::WriteThrough_Transient_WriteAlloc;

    MAIR_EL2.write(attr0 + attr1 + attr2 + attr3);

    // Enable TTBR0 walks, page size = 4K, vaddr size = 48 bits, paddr size = 40 bits.
    const VADDR_SIZE: u64 = 48;
    const T0SZ: u64 = 64 - VADDR_SIZE;

    // Note: TCR_EL2 only has one set of translation controls (T0SZ, TG0)
    // TTBR1_EL2 does not exist in ARMv8 architecture
    let tcr_flags0 = TCR_EL2::T0SZ.val(T0SZ)
        + TCR_EL2::TG0::KiB_4
        + TCR_EL2::SH0::Inner
        + TCR_EL2::ORGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable
        + TCR_EL2::IRGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable;

    TCR_EL2.write(TCR_EL2::PS::Bits_40 + tcr_flags0);

    tlbi(ALLE2IS);
    barrier::dsb(barrier::SY);
    barrier::isb(barrier::SY);
}

pub fn get_kernal_table() -> PageTableInfo {
    // EL2 only has TTBR0_EL2 (no TTBR1_EL2)
    // TTBR0_EL2 doesn't have ASID field, so we use 0
    let addr = TTBR0_EL2.get_baddr();
    PageTableInfo {
        asid: 0,
        addr: addr as usize,
    }
}

pub fn set_kernal_table(tb: PageTableInfo) {
    // TTBR0_EL2 doesn't have ASID field, only set the address
    TTBR0_EL2.set_baddr(tb.addr as _);
}

#[inline(always)]
pub fn is_mmu_enabled() -> bool {
    SCTLR_EL2.is_set(SCTLR_EL2::M)
}

#[inline(always)]
pub fn setup_sctlr() {
    SCTLR_EL2.modify(SCTLR_EL2::M::Enable + SCTLR_EL2::C::Cacheable + SCTLR_EL2::I::Cacheable);
    flush_tlb(None);
    barrier::dsb(barrier::SY);
    barrier::isb(barrier::SY);
}

pub fn systick_enable() {
    CNTHP_CTL_EL2.write(CNTHP_CTL_EL2::ENABLE::SET);
}

pub fn systick_irq_disable() {
    CNTHP_CTL_EL2.modify(CNTHP_CTL_EL2::IMASK::SET);
}

pub fn systick_irq_enable() {
    CNTHP_CTL_EL2.modify(CNTHP_CTL_EL2::IMASK::CLEAR);
}

pub fn systick_irq_is_enabled() -> bool {
    !CNTHP_CTL_EL2.is_set(CNTHP_CTL_EL2::IMASK)
}

pub fn systick_set_interval(ticks: usize) {
    unsafe {
        core::arch::asm!("msr CNTHP_TVAL_EL2, {0:x}", in(reg) ticks);
    }
}
