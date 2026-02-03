//! Virtual Supevisor Interrupt Pending Register.

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, set_clear_csr, write_csr};

/// Virtual Supervisor Interrupt Pending Register.
#[derive(Copy, Clone, Debug)]
pub struct Vsip {
    bits: usize,
}

impl Vsip {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Vsip { bits: x }
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
    /// Returns the supervisor software interrupt pending.
    #[inline]
    pub fn ssip(&self) -> bool {
        self.bits.get_bit(1)
    }
    /// Sets the supervisor software interrupt pending.
    #[inline]
    pub fn set_ssip(&mut self, val: bool) {
        self.bits.set_bit(1, val);
    }
    /// Returns the supervisor timer interrupt pending.
    #[inline]
    pub fn stip(&self) -> bool {
        self.bits.get_bit(5)
    }
    /// Sets the supervisor timer interrupt pending.
    #[inline]
    pub fn set_stip(&mut self, val: bool) {
        self.bits.set_bit(5, val);
    }
    /// Returns the supervisor external interrupt pending.
    #[inline]
    pub fn seip(&self) -> bool {
        self.bits.get_bit(9)
    }
    /// Sets the supervisor external interrupt pending.
    #[inline]
    pub fn set_seip(&mut self, val: bool) {
        self.bits.set_bit(9, val);
    }
}

read_csr_as!(Vsip, 0x244);
write_csr!(0x244);
set!(0x244);
clear!(0x244);
// bit ops
set_clear_csr!(
    /// Supervisor software interrupt pending enable.
    , set_ssip, clear_ssip, 1 << 1);
set_clear_csr!(
    /// Supervisor timer interrupt pending enable.
    , set_stip, clear_stip, 1 << 5);
set_clear_csr!(
    /// Supervisor external interrupt pending enable.
    , set_seip, clear_seip, 1 << 9);

// enums
