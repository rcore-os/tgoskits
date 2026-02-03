//! Hypervisor Virtual Interrupt Pending Register.

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, set_clear_csr, write_csr};

/// Hypervisor Virtual Interrupt Pending Register.
#[derive(Copy, Clone, Debug)]
pub struct Hvip {
    bits: usize,
}

impl Hvip {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Hvip { bits: x }
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
    /// Returns the virtual supervisor software interrupt pending.
    #[inline]
    pub fn vssip(&self) -> bool {
        self.bits.get_bit(2)
    }
    /// Sets the virtual supervisor software interrupt pending.
    #[inline]
    pub fn set_vssip(&mut self, val: bool) {
        self.bits.set_bit(2, val);
    }
    /// Returns the virtual supervisor timer interrupt pending.
    #[inline]
    pub fn vstip(&self) -> bool {
        self.bits.get_bit(6)
    }
    /// Sets the virtual supervisor timer interrupt pending.
    #[inline]
    pub fn set_vstip(&mut self, val: bool) {
        self.bits.set_bit(6, val);
    }
    /// Returns the virtual supervisor external interrupt pending.
    #[inline]
    pub fn vseip(&self) -> bool {
        self.bits.get_bit(10)
    }
    /// Sets the virtual supervisor external interrupt pending.
    #[inline]
    pub fn set_vseip(&mut self, val: bool) {
        self.bits.set_bit(10, val);
    }
}

read_csr_as!(Hvip, 0x645);
write_csr!(0x645);
set!(0x645);
clear!(0x645);

// bit ops
set_clear_csr!(
    /// Virtual supervisor software interrupt pending enable.
    , set_vssip, clear_vssip, 1 << 2);
set_clear_csr!(
    /// Virtual supervisor timer interrupt pending enable.
    , set_vstip, clear_vstip, 1 << 6);
set_clear_csr!(
    /// Virtual supervisor external interrupt pending enable.
    , set_vseip, clear_vseip, 1 << 10);

// enums
