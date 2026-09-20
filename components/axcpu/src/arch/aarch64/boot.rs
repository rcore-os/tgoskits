// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// Exception-level handoff migrated from someboot; original author:
// 周睿 <zrufo747@outlook.com>. Native trap initialization retains its Apache origin.
//! Helper functions to initialize the CPU states on systems bootstrapping.

use aarch64_cpu::{asm::barrier, registers::*};

/// Transfers execution to a mapped entry on a new stack without returning.
///
/// # Safety
/// The target must be valid executable code accepting a fresh boot handoff.
/// The stack must be aligned to 16 bytes, writable and exclusively owned.
/// IRQ state, translation, CPU anchor and TLS must satisfy the target contract;
/// no live stack-owned resource may require destruction after this transfer.
#[unsafe(naked)]
pub unsafe extern "C" fn jump_to(_entry: crate::VirtAddr, _stack: crate::VirtAddr) -> ! {
    core::arch::naked_asm!("mov sp, x1", "br x0");
}

impl super::mmu::El1 {
    /// Installs native EL1 vectors and removes the boot lower-address mapping.
    ///
    /// # Safety
    /// Execute at EL1 with IRQs masked, a valid kernel stack and kernel CPU
    /// anchor installed. No live mapping may depend on the boot TTBR0_EL1 root.
    pub unsafe fn init_trap() {
        #[cfg(feature = "uspace")]
        CNTKCTL_EL1.modify(CNTKCTL_EL1::EL0VCTEN::TrappedNone + CNTKCTL_EL1::EL0PCTEN::TrappedNone);
        unsafe extern "C" {
            fn __ax_cpu_vector_el1();
        }
        VBAR_EL1.set(__ax_cpu_vector_el1 as *const () as u64);
        TTBR0_EL1.set(0);
        barrier::isb(barrier::SY);
    }
}

impl super::mmu::El2 {
    /// Installs native non-VHE EL2 vectors without modifying a guest EL1 bank.
    ///
    /// # Safety
    /// Execute at non-VHE EL2 with IRQs masked and a valid kernel stack and CPU
    /// anchor installed. The mapped CPU vector text must remain executable.
    pub unsafe fn init_trap() {
        unsafe extern "C" {
            fn __ax_cpu_vector_el2();
        }
        VBAR_EL2.set(__ax_cpu_vector_el2 as *const () as u64);
        barrier::isb(barrier::SY);
    }
}

impl super::mmu::El1 {
    /// Installs the early vector using the boot owner's exception policy.
    ///
    /// # Safety
    /// Execute at the corresponding EL with a valid stack and IRQs masked.
    /// The vector and boot handler must remain mapped and callable; runtime
    /// TLS and CPU-local services need not be initialized.
    pub unsafe fn init_boot_trap() {
        unsafe extern "C" {
            fn __ax_cpu_boot_vector_el1();
        }
        VBAR_EL1.set(__ax_cpu_boot_vector_el1 as *const () as u64);
        barrier::isb(barrier::SY);
    }
    /// Reads the currently installed vector base for this exception level.
    pub fn vector_base() -> crate::VirtAddr {
        crate::VirtAddr::from_usize(VBAR_EL1.get() as usize)
    }
}

impl super::mmu::El2 {
    /// Installs the early vector using the boot owner's exception policy.
    ///
    /// # Safety
    /// Execute at the corresponding EL with a valid stack and IRQs masked.
    /// The vector and boot handler must remain mapped and callable; runtime
    /// TLS and CPU-local services need not be initialized.
    pub unsafe fn init_boot_trap() {
        unsafe extern "C" {
            fn __ax_cpu_boot_vector_el2();
        }
        VBAR_EL2.set(__ax_cpu_boot_vector_el2 as *const () as u64);
        barrier::isb(barrier::SY);
    }
    /// Reads the currently installed vector base for this exception level.
    pub fn vector_base() -> crate::VirtAddr {
        crate::VirtAddr::from_usize(VBAR_EL2.get() as usize)
    }
}

/// Selects the current exception level's dedicated stack and clears SP_EL0.
///
/// # Safety
/// The current EL stack must already be valid; no live owner may rely on SP_EL0.
pub unsafe fn select_privileged_stack() {
    SPSel.write(SPSel::SP::ELx);
    SP_EL0.set(0);
}

// The argument is assigned to x0 in the final machine window. In particular,
// no compiler-generated call may clobber a secondary CPU's handoff pointer.
unsafe fn exception_return(argument: usize) -> ! {
    // SAFETY: the mode-specific caller installed the return PC, stack and PSTATE.
    unsafe {
        core::arch::asm!("isb", "eret", in("x0") argument, options(noreturn, nostack));
    }
}

impl super::mmu::El1 {
    /// Transfers from EL2 or EL3 to a non-secure AArch64 EL1 boot entry.
    /// Physical timer access is enabled and the virtual counter offset is zero.
    ///
    /// # Safety
    /// Execute at EL2/EL3 before admitting other owners of the lower register
    /// banks. Entry and its 16-byte-aligned exclusive stack must be accessible
    /// at EL1 with the currently installed translation state. Entry accepts its
    /// sole argument in x0. No live resource may need destruction on this stack.
    pub unsafe fn enter(entry: crate::VirtAddr, stack: crate::VirtAddr, argument: usize) -> ! {
        CNTHCTL_EL2.modify(CNTHCTL_EL2::EL1PCEN::SET + CNTHCTL_EL2::EL1PCTEN::SET);
        CNTVOFF_EL2.set(0);
        HCR_EL2.write(HCR_EL2::RW::EL1IsAarch64);
        SP_EL1.set(stack.as_usize() as u64);
        if CurrentEL.read(CurrentEL::EL) == 3 {
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
            ELR_EL3.set(entry.as_usize() as u64);
        } else {
            SPSR_EL2.write(
                SPSR_EL2::M::EL1h
                    + SPSR_EL2::D::Masked
                    + SPSR_EL2::A::Masked
                    + SPSR_EL2::I::Masked
                    + SPSR_EL2::F::Masked,
            );
            ELR_EL2.set(entry.as_usize() as u64);
        }
        // SAFETY: the selected return bank retains the target, stack and masked PSTATE.
        unsafe { exception_return(argument) }
    }
}

impl super::mmu::El2 {
    /// Transfers from EL3 to a non-secure AArch64 EL2 boot entry with IRQs masked.
    ///
    /// # Safety
    /// Execute at EL3 with exclusive ownership of the lower exception registers.
    /// Entry and its 16-byte-aligned exclusive stack must be accessible at EL2;
    /// entry accepts its argument in x0. No live stack resource needs destruction.
    pub unsafe fn enter(entry: crate::VirtAddr, stack: crate::VirtAddr, argument: usize) -> ! {
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
        ELR_EL3.set(entry.as_usize() as u64);
        SP_EL2.set(stack.as_usize() as u64);
        // SAFETY: all exception-return state is installed for the owned EL2 entry.
        unsafe { exception_return(argument) }
    }
}
