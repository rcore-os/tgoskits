//! Guest reset and virtual interrupt register controls.

use super::{Exception, GuestBinding, GuestPrivilege, Vcpu};

/// Virtual interrupt bit in HIE/HVIP, independent of host IRQ routing.
#[repr(usize)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestInterrupt {
    /// Virtual supervisor software interrupt.
    Software = 2,
    /// Virtual supervisor timer interrupt.
    Timer    = 6,
    /// Virtual supervisor external interrupt.
    External = 10,
}

impl Vcpu {
    /// Installs supervisor reset controls without changing guest GPRs or memory.
    pub fn initialize_supervisor(&mut self) {
        // SPP=Supervisor, FS=Initial; global interrupt enables remain clear.
        self.guest_regs.sstatus = (1 << 8) | (1 << 13);
        // SPV, SPVP, VSXL=64; guest privileged instructions are not intercepted.
        self.guest_regs.hstatus = (1 << 7) | (1 << 8) | (2 << 32);
        self.virtual_hs_csrs.hie = (1 << 2) | (1 << 6) | (1 << 10);
        self.virtual_hs_csrs.hvip = 0;
        self.virtual_hs_csrs.hgeie = 0;
        #[cfg(feature = "riscv-sstc")]
        {
            self.vs_csrs.vstimecmp = usize::MAX;
        }
    }

    /// Updates one pending virtual interrupt in the uninstalled register image.
    /// The active binding must explicitly synchronize the image after this call.
    pub fn set_interrupt_pending(&mut self, interrupt: GuestInterrupt, pending: bool) {
        let bit = 1usize << interrupt as usize;
        if pending {
            self.virtual_hs_csrs.hvip |= bit;
        } else {
            self.virtual_hs_csrs.hvip &= !bit;
        }
    }

    /// Captures host-raised virtual interrupt bits for subsequent guest binding.
    ///
    /// # Safety
    /// The caller owns the current hart's HVIP bank and has established that
    /// every pending bit belongs to this guest. It must exclude migration and
    /// concurrent changes while attributing and capturing the pending state.
    pub unsafe fn latch_interrupts(&mut self) {
        let pending: usize;
        // SAFETY: caller owns this hart's guest interrupt attribution.
        unsafe {
            core::arch::asm!("csrr {}, hvip", out(reg) pending, options(nostack));
        }
        self.virtual_hs_csrs.hvip |= pending & ((1 << 2) | (1 << 6) | (1 << 10));
    }
}

impl GuestBinding {
    /// Synchronizes one pending bit without overwriting other hardware events.
    /// The binding's CPU pin and exclusive ownership must remain active.
    pub fn sync_interrupt(&mut self, registers: &Vcpu, interrupt: GuestInterrupt) {
        let bit = 1usize << interrupt as usize;
        // SAFETY: this live binding owns the current hart. CSR set/clear touches
        // only the selected source, preserving independently raised pending bits.
        unsafe {
            if registers.virtual_hs_csrs.hvip & bit != 0 {
                core::arch::asm!("csrs hvip, {}", in(reg) bit, options(nostack));
            } else {
                core::arch::asm!("csrc hvip, {}", in(reg) bit, options(nostack));
            }
        }
    }

    /// Programs the bound virtual supervisor comparator and its saved image.
    #[cfg(feature = "riscv-sstc")]
    pub fn set_timer_compare(&mut self, registers: &mut Vcpu, deadline: usize) {
        let _irqs = super::binding::LocalIrqs::disable();
        registers.vs_csrs.vstimecmp = deadline;
        // SAFETY: successful binding validated STCE and owns this hart's timer.
        unsafe {
            core::arch::asm!("csrw vstimecmp, {}", in(reg) deadline, options(nostack));
        }
    }
}

impl Vcpu {
    /// Returns the privilege captured on the most recent guest exit.
    pub const fn guest_privilege(&self) -> GuestPrivilege {
        if self.guest_regs.hstatus & (1 << 8) != 0 {
            GuestPrivilege::Supervisor
        } else {
            GuestPrivilege::User
        }
    }
}

impl GuestBinding {
    /// Delivers a synchronous exception through the guest's current VS vector.
    /// Updates the installed bank and saved image together before reentry.
    pub fn inject_exception(
        &mut self,
        registers: &mut Vcpu,
        exception: Exception,
        address: crate::virtualization::GuestVirtAddr,
    ) {
        let _irqs = super::binding::LocalIrqs::disable();
        let mut status: usize;
        let vector: usize;
        // SAFETY: this binding owns the current hart and VS register bank.
        unsafe {
            core::arch::asm!("csrr {}, vsstatus", "csrr {}, vstvec", out(reg) status, out(reg) vector, options(nostack));
        }
        let saved_ie = (status >> 1) & 1;
        status = (status & !((1 << 1) | (1 << 5) | (1 << 8)))
            | (saved_ie << 5)
            | (registers.guest_regs.hstatus & (1 << 8));
        registers.vs_csrs.vstvec = vector;
        registers.vs_csrs.vsepc = registers.guest_regs.sepc;
        registers.vs_csrs.vscause = exception as usize;
        registers.vs_csrs.vstval = address.as_usize();
        registers.vs_csrs.vsstatus = status;
        registers.guest_regs.sepc = vector & !3;
        // SAFETY: values are the architectural VS synchronous trap transition;
        // host SSTATUS and host vector are not modified.
        unsafe {
            core::arch::asm!("csrw vsstatus, {status}", "csrw vscause, {cause}",
            "csrw vstval, {address}", "csrw vsepc, {pc}",
            status = in(reg) status, cause = in(reg) exception as usize,
            address = in(reg) address.as_usize(), pc = in(reg) registers.vs_csrs.vsepc,
            options(nostack));
        }
    }
}
