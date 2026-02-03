//! Hypervisor Interrupt Delegation Register.

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, set_clear_csr, write_csr};

/// Hypervisor Interrupt Delegation Register.
#[derive(Copy, Clone, Debug)]
pub struct Hideleg {
    bits: usize,
}

impl Hideleg {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Hideleg { bits: x }
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
    /// Returns the status of the supervisor software interrupt delegation.
    #[inline]
    pub fn sip(&self) -> bool {
        self.bits.get_bit(2)
    }
    /// Sets the status of the supervisor software interrupt delegation.
    #[inline]
    pub fn set_sip(&mut self, val: bool) {
        self.bits.set_bit(2, val);
    }
    /// Returns the status of the supervisor timer interrupt delegation.
    #[inline]
    pub fn tip(&self) -> bool {
        self.bits.get_bit(6)
    }
    /// Sets the status of the supervisor timer interrupt delegation.
    #[inline]
    pub fn set_tip(&mut self, val: bool) {
        self.bits.set_bit(6, val);
    }
    /// Returns the status of the supervisor external interrupt delegation.
    #[inline]
    pub fn eip(&self) -> bool {
        self.bits.get_bit(10)
    }
    /// Sets the status of the supervisor external interrupt delegation.
    #[inline]
    pub fn set_eip(&mut self, val: bool) {
        self.bits.set_bit(10, val);
    }
}

read_csr_as!(Hideleg, 0x603);
write_csr!(0x603);
set!(0x603);
clear!(0x603);

// bit ops
set_clear_csr!(
    /// Supervisor software interrupt delegation.
    , set_sip, clear_sip, 1 << 2);
set_clear_csr!(
    /// Supervisor timer interrupt delegation.
    , set_tip, clear_tip, 1 << 6);
set_clear_csr!(
    /// Supervisor external interrupt delegation.
    , set_eip, clear_eip, 1 << 10);

// enums
