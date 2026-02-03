//! Virtual Supervisor Guest Address Translation and Protection Register.

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, write_csr};

/// Virtual Supervisor Address Translation and Protection Register.
#[derive(Copy, Clone, Debug)]
pub struct Vsatp {
    bits: usize,
}

impl Vsatp {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Vsatp { bits: x }
    }
    /// Writes the register value to the CSR.
    ///
    /// # Safety
    ///
    /// This function is unsafe because writing to CSR registers can have
    /// system-wide effects and may violate memory safety guarantees.
    #[inline]
    pub unsafe fn write(&self) {
        // SAFETY: Caller ensures this is safe to execute
        unsafe { _write(self.bits) };
    }
    /// Returns the guest address translation mode.
    #[inline]
    pub fn mode(&self) -> HgatpValues {
        HgatpValues::from(self.bits.get_bits(60..64))
    }
    /// Sets the guest address translation mode.
    #[inline]
    pub fn set_mode(&mut self, val: HgatpValues) {
        self.bits.set_bits(60..64, val as usize);
    }
    /// Returns the address space identifier.
    #[inline]
    pub fn asid(&self) -> usize {
        self.bits.get_bits(44..60)
    }
    /// Sets the address space identifier.
    #[inline]
    pub fn set_asid(&mut self, val: usize) {
        self.bits.set_bits(44..60, val);
    }
    /// Returns the physical page number for root page table.
    #[inline]
    pub fn ppn(&self) -> usize {
        self.bits.get_bits(0..44)
    }
    /// Sets the physical page number for root page table.
    #[inline]
    pub fn set_ppn(&mut self, val: usize) {
        self.bits.set_bits(0..44, val);
    }
}

read_csr_as!(Vsatp, 0x280);
write_csr!(0x280);
set!(0x280);
clear!(0x280);
// bit ops

/// Hypervisor Guest Address Translation and Protection Register values.
#[derive(Copy, Clone, Debug)]
#[repr(usize)]
pub enum HgatpValues {
    /// Bare
    Bare = 0,
    /// Supervisor Virtual Address Translation (SV39)
    Sv39x4 = 8,
    /// Supervisor Virtual Address Translation (SV48)
    Sv48x4 = 9,
}

impl HgatpValues {
    fn from(x: usize) -> Self {
        match x {
            0 => Self::Bare,
            8 => Self::Sv39x4,
            9 => Self::Sv48x4,
            _ => unreachable!(),
        }
    }
}
