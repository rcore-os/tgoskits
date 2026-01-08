use aarch64_cpu::{asm::*, registers::*};

use crate::arch::entry::el_entry;

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

#[inline(always)]
pub fn is_mmu_enabled() -> bool {}
