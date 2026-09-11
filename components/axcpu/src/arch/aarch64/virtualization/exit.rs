//! Durable lower-EL machine exit state.

/// Architectural exception class of an entry-vector slot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u64)]
pub enum ExitKind {
    /// Synchronous exception, described by ESR_EL2.
    #[default]
    Synchronous = 0,
    /// Physical IRQ acknowledged before stopping the guest virtual timer.
    Irq         = 1,
    /// Fast interrupt.
    Fiq         = 2,
    /// Asynchronous system error.
    SystemError = 3,
}

/// Register values captured before returning to host Rust code.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Exit {
    /// Architectural class of the vector slot that caused the exit.
    pub kind: ExitKind,
    /// ESR_EL2 at exit; relevant to synchronous exceptions.
    pub syndrome: u64,
    /// FAR_EL2 at exit, in the guest virtual address domain.
    pub fault_address: u64,
    /// HPFAR_EL2 at exit, retaining the architectural encoding.
    pub physical_fault_address: u64,
    /// Guest preferred return address from ELR_EL2.
    pub pc: u64,
    /// Raw host IAR value, or `u32::MAX` when no IRQ was acknowledged.
    pub irq_ack: u32,
    /// IPA resolved before restoring the host translation bank.
    pub guest_address: Result<crate::virtualization::GuestPhysAddr, GuestAddressError>,
}

impl Default for Exit {
    fn default() -> Self {
        Self {
            kind: ExitKind::Synchronous,
            syndrome: 0,
            fault_address: 0,
            physical_fault_address: 0,
            pc: 0,
            irq_ack: u32::MAX,
            guest_address: Err(GuestAddressError::NotDataAbort),
        }
    }
}

/// Guest stage-1 resolution failed while interpreting an EL2 data abort.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GuestAddressError {
    /// The exit does not report a guest data abort.
    #[error("exit does not report a guest data abort")]
    NotDataAbort,
    /// AT S1E1R reported an architectural translation failure.
    #[error("guest stage-1 translation failed: PAR_EL1={par:#x}")]
    TranslationFault {
        /// Complete PAR_EL1 failure encoding.
        par: u64,
    },
}

impl Exit {
    /// Resolves a data-abort IPA while the guest's stage-1 bank is still loaded.
    ///
    /// # Safety
    /// The current CPU must be pinned at EL2 with IRQs masked, and own the
    /// guest register bank corresponding to this exit. Referenced tables stay live.
    pub unsafe fn resolve_guest_address(
        &self,
    ) -> Result<crate::virtualization::GuestPhysAddr, GuestAddressError> {
        use aarch64_cpu::registers::{PAR_EL1, Readable, Writeable};
        if self.kind != ExitKind::Synchronous || (self.syndrome >> 26) & 63 != 0x24 {
            return Err(GuestAddressError::NotDataAbort);
        }
        let mut hpfar = self.physical_fault_address;
        if self.syndrome & (1 << 7) == 0 && self.syndrome & 0x3c == 0x0c {
            let saved = PAR_EL1.get();
            // SAFETY: the caller retains the exclusive guest translation bank;
            // AT returns a fault encoding in PAR instead of dereferencing Rust memory.
            unsafe {
                core::arch::asm!("at s1e1r, {}", "isb", in(reg) self.fault_address, options(nostack));
            }
            let result = PAR_EL1.get();
            PAR_EL1.set(saved);
            if result & 1 != 0 {
                return Err(GuestAddressError::TranslationFault { par: result });
            }
            hpfar = (result & 0x000f_ffff_ffff_f000) >> 8;
        }
        Ok(crate::virtualization::GuestPhysAddr::from_usize(
            ((self.fault_address & 0xfff) | (hpfar << 8)) as usize,
        ))
    }
}
