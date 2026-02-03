//! Virtual Supervisor Cause Register.

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, set_clear_csr, write_csr};

/// Virtual Supervisor Cause Register
#[derive(Copy, Clone, Debug)]
pub struct Vscause {
    bits: usize,
}

impl Vscause {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Vscause { bits: x }
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
    /// Returns the interrupt cause status.
    #[inline]
    pub fn interrupt(&self) -> bool {
        self.bits.get_bit(63)
    }
    /// Sets the interrupt cause status.
    #[inline]
    pub fn set_interrupt(&mut self, val: bool) {
        self.bits.set_bit(63, val);
    }
    /// Returns the exception code.
    #[inline]
    pub fn code(&self) -> usize {
        self.bits.get_bits(0..63)
    }
    /// Sets the exception code.
    #[inline]
    pub fn set_code(&mut self, val: usize) {
        self.bits.set_bits(0..63, val);
    }
}

read_csr_as!(Vscause, 0x242);
write_csr!(0x242);
set!(0x242);
clear!(0x242);

// bit ops
set_clear_csr!(
    /// Interrupt cause enable.
    , set_interrupt, clear_interrupt, 1 << 63);

// enums
