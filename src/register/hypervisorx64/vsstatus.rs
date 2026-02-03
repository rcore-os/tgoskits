//! Virtual Supervisor Status Register.
//!
//! The `vsstatus` register contains status and control fields for the virtual supervisor mode.
//! This register controls various aspects of virtual machine execution including privilege levels,
//! memory management, and floating-point state.

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, set_clear_csr, write_csr};

/// Virtual Supervisor Status Register
#[derive(Copy, Clone, Debug)]
pub struct Vsstatus {
    bits: usize,
}

impl Vsstatus {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Vsstatus { bits: x }
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
    /// Returns the status of the dirty state fields.
    #[inline]
    pub fn sd(&self) -> usize {
        self.bits.get_bits(60..64)
    }
    /// Sets the status of the dirty state fields.
    #[inline]
    pub fn set_sd(&mut self, val: usize) {
        self.bits.set_bits(60..64, val);
    }
    /// Returns the effective user XLEN setting.
    #[inline]
    pub fn uxl(&self) -> UxlValues {
        UxlValues::from(self.bits.get_bits(32..34))
    }
    /// Sets the effective user XLEN setting.
    #[inline]
    pub fn set_uxl(&mut self, val: UxlValues) {
        self.bits.set_bits(32..34, val as usize);
    }
    /// Returns the status of the make executable readable bit.
    #[inline]
    pub fn mxr(&self) -> bool {
        self.bits.get_bit(19)
    }
    /// Sets the MXR (Make eXecutable Readable) bit.
    #[inline]
    pub fn set_mxr(&mut self, val: bool) {
        self.bits.set_bit(19, val);
    }
    /// Returns the status of the supervisor user memory access bit.
    #[inline]
    pub fn sum(&self) -> bool {
        self.bits.get_bit(18)
    }
    /// Sets the status of the supervisor user memory access bit.
    #[inline]
    pub fn set_sum(&mut self, val: bool) {
        self.bits.set_bit(18, val);
    }
    /// Returns the status of the extension state fields.
    #[inline]
    pub fn xs(&self) -> usize {
        self.bits.get_bits(15..17)
    }
    /// Sets the status of the extension state fields.
    #[inline]
    pub fn set_xs(&mut self, val: usize) {
        self.bits.set_bits(15..17, val);
    }
    /// Returns the floating point state.
    #[inline]
    pub fn fs(&self) -> usize {
        self.bits.get_bits(13..15)
    }
    /// Sets the floating point state.
    #[inline]
    pub fn set_fs(&mut self, val: usize) {
        self.bits.set_bits(13..15, val);
    }
    /// Returns the supervisor previous privilege.
    #[inline]
    pub fn spp(&self) -> bool {
        self.bits.get_bit(8)
    }
    /// Sets the supervisor previous privilege.
    #[inline]
    pub fn set_spp(&mut self, val: bool) {
        self.bits.set_bit(8, val);
    }
    /// Returns the user binary endianness.
    #[inline]
    pub fn ube(&self) -> bool {
        self.bits.get_bit(6)
    }
    /// Sets the user binary endianness.
    #[inline]
    pub fn set_ube(&mut self, val: bool) {
        self.bits.set_bit(6, val);
    }
    /// Returns the supervisor previous interrupt enable.
    #[inline]
    pub fn spie(&self) -> bool {
        self.bits.get_bit(5)
    }
    /// Sets the supervisor previous interrupt enable.
    #[inline]
    pub fn set_spie(&mut self, val: bool) {
        self.bits.set_bit(5, val);
    }
    /// Returns the supervisor interrupt enable.
    #[inline]
    pub fn sie(&self) -> bool {
        self.bits.get_bit(1)
    }
    /// Sets the supervisor interrupt enable.
    #[inline]
    pub fn set_sie(&mut self, val: bool) {
        self.bits.set_bit(1, val);
    }
}

read_csr_as!(Vsstatus, 0x200);
write_csr!(0x200);
set!(0x200);
clear!(0x200);
// bit ops
set_clear_csr!(
    /// Make executable readable enable.
    , set_mxr, clear_mxr, 1 << 19);
set_clear_csr!(
    /// Supervisor user memory enable.
    , set_sum, clear_sum, 1 << 18);
set_clear_csr!(
    /// Supervisor previous privilege enable.
    , set_spp, clear_spp, 1 << 8);
set_clear_csr!(
    /// User binary endianness enable.
    , set_ube, clear_ube, 1 << 6);
set_clear_csr!(
    /// Supervisor previous interrupt enable.
    , set_spie, clear_spie, 1 << 5);
set_clear_csr!(
    /// Supervisor interrupt enable.
    , set_sie, clear_sie, 1 << 1);

/// Hypervisor User XLEN values.
#[derive(Copy, Clone, Debug)]
#[repr(usize)]
pub enum UxlValues {
    /// 32-bit virtual address space
    Uxl32 = 1,
    /// 64-bit virtual address space
    Uxl64 = 2,
    /// 128-bit virtual address space
    Uxl128 = 3,
}

impl UxlValues {
    fn from(x: usize) -> Self {
        match x {
            1 => Self::Uxl32,
            2 => Self::Uxl64,
            3 => Self::Uxl128,
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vsstatus_from_bits() {
        let vsstatus = Vsstatus::from_bits(0x123456789ABCDEF0);
        assert_eq!(vsstatus.bits(), 0x123456789ABCDEF0);
    }

    #[test]
    fn test_vsstatus_sd() {
        let mut vsstatus = Vsstatus::from_bits(0);

        // Test setting SD (4-bit field, bits 60-63)
        vsstatus.set_sd(0xA);
        assert_eq!(vsstatus.sd(), 0xA);
        assert_eq!(vsstatus.bits() & (0xF << 60), 0xA << 60);

        // Test boundary values
        vsstatus.set_sd(0);
        assert_eq!(vsstatus.sd(), 0);

        vsstatus.set_sd(0xF); // Maximum 4-bit value
        assert_eq!(vsstatus.sd(), 0xF);
    }

    #[test]
    fn test_vsstatus_uxl() {
        let mut vsstatus = Vsstatus::from_bits(0);

        // Test setting UXL to 32-bit
        vsstatus.set_uxl(UxlValues::Uxl32);
        assert!(matches!(vsstatus.uxl(), UxlValues::Uxl32));
        assert_eq!(vsstatus.bits() & (0b11 << 32), 1 << 32);

        // Test setting UXL to 64-bit
        vsstatus.set_uxl(UxlValues::Uxl64);
        assert!(matches!(vsstatus.uxl(), UxlValues::Uxl64));
        assert_eq!(vsstatus.bits() & (0b11 << 32), 2 << 32);

        // Test setting UXL to 128-bit
        vsstatus.set_uxl(UxlValues::Uxl128);
        assert!(matches!(vsstatus.uxl(), UxlValues::Uxl128));
        assert_eq!(vsstatus.bits() & (0b11 << 32), 3 << 32);
    }

    #[test]
    fn test_vsstatus_boolean_fields() {
        let mut vsstatus = Vsstatus::from_bits(0);

        // Test MXR bit (bit 19)
        assert!(!vsstatus.mxr());
        vsstatus.set_mxr(true);
        assert!(vsstatus.mxr());
        assert_eq!(vsstatus.bits() & (1 << 19), 1 << 19);

        // Test SUM bit (bit 18)
        assert!(!vsstatus.sum());
        vsstatus.set_sum(true);
        assert!(vsstatus.sum());
        assert_eq!(vsstatus.bits() & (1 << 18), 1 << 18);

        // Test SPP bit (bit 8)
        assert!(!vsstatus.spp());
        vsstatus.set_spp(true);
        assert!(vsstatus.spp());
        assert_eq!(vsstatus.bits() & (1 << 8), 1 << 8);

        // Test UBE bit (bit 6)
        assert!(!vsstatus.ube());
        vsstatus.set_ube(true);
        assert!(vsstatus.ube());
        assert_eq!(vsstatus.bits() & (1 << 6), 1 << 6);

        // Test SPIE bit (bit 5)
        assert!(!vsstatus.spie());
        vsstatus.set_spie(true);
        assert!(vsstatus.spie());
        assert_eq!(vsstatus.bits() & (1 << 5), 1 << 5);

        // Test SIE bit (bit 1)
        assert!(!vsstatus.sie());
        vsstatus.set_sie(true);
        assert!(vsstatus.sie());
        assert_eq!(vsstatus.bits() & (1 << 1), 1 << 1);
    }

    #[test]
    fn test_vsstatus_xs() {
        let mut vsstatus = Vsstatus::from_bits(0);

        // Test setting XS (2-bit field, bits 15-16)
        vsstatus.set_xs(0x2);
        assert_eq!(vsstatus.xs(), 0x2);
        assert_eq!(vsstatus.bits() & (0b11 << 15), 0x2 << 15);

        // Test boundary values
        vsstatus.set_xs(0);
        assert_eq!(vsstatus.xs(), 0);

        vsstatus.set_xs(0x3); // Maximum 2-bit value
        assert_eq!(vsstatus.xs(), 0x3);
    }

    #[test]
    fn test_vsstatus_fs() {
        let mut vsstatus = Vsstatus::from_bits(0);

        // Test setting FS (2-bit field, bits 13-14)
        vsstatus.set_fs(0x2);
        assert_eq!(vsstatus.fs(), 0x2);
        assert_eq!(vsstatus.bits() & (0b11 << 13), 0x2 << 13);

        // Test boundary values
        vsstatus.set_fs(0);
        assert_eq!(vsstatus.fs(), 0);

        vsstatus.set_fs(0x3); // Maximum 2-bit value
        assert_eq!(vsstatus.fs(), 0x3);
    }

    #[test]
    fn test_uxl_values_from() {
        assert!(matches!(UxlValues::from(1), UxlValues::Uxl32));
        assert!(matches!(UxlValues::from(2), UxlValues::Uxl64));
        assert!(matches!(UxlValues::from(3), UxlValues::Uxl128));
    }

    #[test]
    #[should_panic]
    fn test_uxl_values_from_invalid() {
        UxlValues::from(0);
    }

    #[test]
    fn test_vsstatus_all_fields() {
        let mut vsstatus = Vsstatus::from_bits(0);

        // Set multiple fields and verify they don't interfere
        vsstatus.set_sd(0xB);
        vsstatus.set_uxl(UxlValues::Uxl64);
        vsstatus.set_mxr(true);
        vsstatus.set_sum(true);
        vsstatus.set_xs(0x2);
        vsstatus.set_fs(0x3);
        vsstatus.set_spp(true);
        vsstatus.set_ube(true);
        vsstatus.set_spie(true);
        vsstatus.set_sie(true);

        assert_eq!(vsstatus.sd(), 0xB);
        assert!(matches!(vsstatus.uxl(), UxlValues::Uxl64));
        assert!(vsstatus.mxr());
        assert!(vsstatus.sum());
        assert_eq!(vsstatus.xs(), 0x2);
        assert_eq!(vsstatus.fs(), 0x3);
        assert!(vsstatus.spp());
        assert!(vsstatus.ube());
        assert!(vsstatus.spie());
        assert!(vsstatus.sie());
    }

    #[test]
    fn test_vsstatus_copy_clone() {
        let vsstatus1 = Vsstatus::from_bits(0x123456789ABCDEF0);
        let vsstatus2 = vsstatus1;
        let vsstatus3 = vsstatus1.clone();

        assert_eq!(vsstatus1.bits(), vsstatus2.bits());
        assert_eq!(vsstatus1.bits(), vsstatus3.bits());
    }
}
