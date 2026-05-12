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
    if current_el >= 2 {
        let el_entry = sym_addr!(el_entry);
        let sp = sym_addr!(__cpu0_stack_top);

        if current_el == 3 {
            // Set EL2 to 64bit and enable the HVC instruction.
            SCR_EL3.write(
                SCR_EL3::NS::NonSecure + SCR_EL3::HCE::HvcEnabled + SCR_EL3::RW::NextELIsAarch64,
            );
            // Set the return address and exception level.
            SPSR_EL3.write(
                SPSR_EL3::M::EL1h
                    + SPSR_EL3::D::Masked
                    + SPSR_EL3::A::Masked
                    + SPSR_EL3::I::Masked
                    + SPSR_EL3::F::Masked,
            );
            let switch = sym_addr!(switch_to_elx);

            ELR_EL3.set(switch as _);
            SP_EL2.set(sp as _);
            barrier::isb(barrier::SY);
            eret();
        }
        // Disable EL1 timer traps and the timer offset.
        CNTHCTL_EL2.modify(CNTHCTL_EL2::EL1PCEN::SET + CNTHCTL_EL2::EL1PCTEN::SET);
        CNTVOFF_EL2.set(0);
        // Set EL1 to 64bit.
        HCR_EL2.write(HCR_EL2::RW::EL1IsAarch64);
        // Set the return address and exception level.
        SPSR_EL2.write(
            SPSR_EL2::M::EL1h
                + SPSR_EL2::D::Masked
                + SPSR_EL2::A::Masked
                + SPSR_EL2::I::Masked
                + SPSR_EL2::F::Masked,
        );

        ELR_EL2.set(el_entry as _);
        SP_EL1.set(sp as _);
        barrier::isb(barrier::SY);
        eret();
    }

    el_entry();
}

pub fn switch_to_elx_secondary(cpu_meta_paddr: usize) -> ! {
    SPSel.write(SPSel::SP::ELx);
    SP_EL0.set(0);

    let current_el = CurrentEL.read(CurrentEL::EL);
    let secondary_entry = sym_addr!(crate::arch::entry::secondary_el_entry);
    let stack_top = unsafe { (cpu_meta_paddr as *const usize).read_volatile() };

    if current_el >= 2 {
        if current_el == 3 {
            SCR_EL3.write(
                SCR_EL3::NS::NonSecure + SCR_EL3::HCE::HvcEnabled + SCR_EL3::RW::NextELIsAarch64,
            );
            SPSR_EL3.write(
                SPSR_EL3::M::EL1h
                    + SPSR_EL3::D::Masked
                    + SPSR_EL3::A::Masked
                    + SPSR_EL3::I::Masked
                    + SPSR_EL3::F::Masked,
            );
            let switch = sym_addr!(switch_to_elx_secondary);
            ELR_EL3.set(switch as _);
            SP_EL2.set(stack_top as _);
            barrier::isb(barrier::SY);
            eret();
        }

        CNTHCTL_EL2.modify(CNTHCTL_EL2::EL1PCEN::SET + CNTHCTL_EL2::EL1PCTEN::SET);
        CNTVOFF_EL2.set(0);
        HCR_EL2.write(HCR_EL2::RW::EL1IsAarch64);
        SPSR_EL2.write(
            SPSR_EL2::M::EL1h
                + SPSR_EL2::D::Masked
                + SPSR_EL2::A::Masked
                + SPSR_EL2::I::Masked
                + SPSR_EL2::F::Masked,
        );

        ELR_EL2.set(secondary_entry as _);
        SP_EL1.set(stack_top as _);
        barrier::isb(barrier::SY);
        eret();
    }

    unsafe { crate::arch::entry::secondary_el_entry(cpu_meta_paddr) }
}

#[inline(always)]
pub fn flush_tlb(vaddr: Option<VirtAddr>) {
    match vaddr {
        Some(addr) => {
            tlbi(VAAE1IS::new(addr.raw()));
        }
        None => {
            tlbi(VMALLE1);
        }
    }
    dsb(SY);
    isb(SY);
}

#[inline(always)]
pub fn setup_table_regs() {
    // Device-nGnRE
    let attr0 = MAIR_EL1::Attr0_Device::nonGathering_nonReordering_EarlyWriteAck;
    // Normal
    let attr1 = MAIR_EL1::Attr1_Normal_Inner::WriteBack_NonTransient_ReadWriteAlloc
        + MAIR_EL1::Attr1_Normal_Outer::WriteBack_NonTransient_ReadWriteAlloc;
    // No cache
    let attr2 =
        MAIR_EL1::Attr2_Normal_Inner::NonCacheable + MAIR_EL1::Attr2_Normal_Outer::NonCacheable;
    // WriteThrough
    let attr3 = MAIR_EL1::Attr3_Normal_Inner::WriteThrough_Transient_WriteAlloc
        + MAIR_EL1::Attr3_Normal_Outer::WriteThrough_Transient_WriteAlloc;

    MAIR_EL1.write(attr0 + attr1 + attr2 + attr3);

    // Enable TTBR0 and TTBR1 walks, page size = 4K, vaddr size = 48 bits, paddr size = 40 bits.
    const VADDR_SIZE: u64 = 48;
    const T0SZ: u64 = 64 - VADDR_SIZE;

    let tcr_flags0 = TCR_EL1::EPD0::EnableTTBR0Walks
        + TCR_EL1::TG0::KiB_4
        + TCR_EL1::SH0::Inner
        + TCR_EL1::ORGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable
        + TCR_EL1::IRGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable
        + TCR_EL1::T0SZ.val(T0SZ);
    let tcr_flags1 = TCR_EL1::EPD1::EnableTTBR1Walks
        + TCR_EL1::TG1::KiB_4
        + TCR_EL1::SH1::Inner
        + TCR_EL1::ORGN1::WriteBack_ReadAlloc_WriteAlloc_Cacheable
        + TCR_EL1::IRGN1::WriteBack_ReadAlloc_WriteAlloc_Cacheable
        + TCR_EL1::T1SZ.val(T0SZ);
    TCR_EL1.write(TCR_EL1::IPS::Bits_48 + tcr_flags0 + tcr_flags1);

    tlbi(VMALLE1);
    barrier::dsb(barrier::SY);
    barrier::isb(barrier::SY);
}

pub fn get_kernal_table() -> PageTableInfo {
    let val = TTBR1_EL1.extract();
    PageTableInfo {
        asid: val.read(TTBR1_EL1::ASID) as _,
        addr: (val.read(TTBR1_EL1::BADDR) << 1) as _,
    }
}

pub fn set_kernal_table(tb: PageTableInfo) {
    TTBR1_EL1.set(TTBR1_EL1::ASID.val(tb.asid as u64).value + tb.addr as u64);
}

pub fn set_user_table(tb: PageTableInfo) {
    TTBR0_EL1.set(TTBR0_EL1::ASID.val(tb.asid as u64).value + tb.addr as u64);
}

pub fn get_user_table() -> PageTableInfo {
    let val = TTBR0_EL1.extract();
    PageTableInfo {
        asid: val.read(TTBR0_EL1::ASID) as _,
        addr: (val.read(TTBR0_EL1::BADDR) << 1) as _,
    }
}

#[inline(always)]
pub fn is_mmu_enabled() -> bool {
    SCTLR_EL1.is_set(SCTLR_EL1::M)
}

#[inline(always)]
pub fn setup_sctlr() {
    SCTLR_EL1.modify(SCTLR_EL1::M::Enable + SCTLR_EL1::C::Cacheable + SCTLR_EL1::I::Cacheable);
    flush_tlb(None);
    barrier::dsb(barrier::SY);
    barrier::isb(barrier::SY);
}

pub fn systick_enable() {
    CNTP_CTL_EL0.write(CNTP_CTL_EL0::ENABLE::SET);
}

pub fn systick_irq_disable() {
    CNTP_CTL_EL0.modify(CNTP_CTL_EL0::IMASK::SET);
}

pub fn systick_irq_enable() {
    CNTP_CTL_EL0.modify(CNTP_CTL_EL0::IMASK::CLEAR);
}

pub fn systick_irq_is_enabled() -> bool {
    !CNTP_CTL_EL0.is_set(CNTP_CTL_EL0::IMASK)
}

pub fn systick_set_interval(ticks: usize) {
    CNTP_TVAL_EL0.set(ticks as u64);
}
