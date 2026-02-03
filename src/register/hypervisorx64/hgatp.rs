//! Hypervisor Guest Address Translation and Protection Register.
//!
//! The `hgatp` register controls guest address translation for two-stage memory management
//! in RISC-V hypervisor implementations. This register configures:
//! - Guest physical address translation mode (Bare, Sv39x4, Sv48x4)
//! - Virtual Machine ID (VMID) for TLB management
//! - Root page table physical page number (PPN)
//!
//! Two-stage translation involves:
//! 1. Guest virtual → Guest physical (controlled by VS-mode satp)  
//! 2. Guest physical → Host physical (controlled by this hgatp register)

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, write_csr};

/// Hypervisor Guest Address Translation and Protection Register.
#[derive(Copy, Clone, Debug)]
pub struct Hgatp {
    bits: usize,
}

impl Hgatp {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Hgatp { bits: x }
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
    /// Returns the Virtual machine ID.
    #[inline]
    pub fn vmid(&self) -> usize {
        self.bits.get_bits(44..58)
    }
    /// Sets the Virtual machine ID.
    #[inline]
    pub fn set_vmid(&mut self, val: usize) {
        self.bits.set_bits(44..58, val);
    }
    /// Returns the Physical Page Number for root page table.
    #[inline]
    pub fn ppn(&self) -> usize {
        self.bits.get_bits(0..44)
    }
    /// Sets the Physical Page Number for root page table.
    #[inline]
    pub fn set_ppn(&mut self, val: usize) {
        self.bits.set_bits(0..44, val);
    }
}

read_csr_as!(Hgatp, 0x680);
write_csr!(0x680);
set!(0x680);
clear!(0x680);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hgatp_from_bits() {
        let hgatp = Hgatp::from_bits(0x123456789ABCDEF0);
        assert_eq!(hgatp.bits(), 0x123456789ABCDEF0);
    }

    #[test]
    fn test_hgatp_mode() {
        let mut hgatp = Hgatp::from_bits(0);

        // Test setting mode to Bare
        hgatp.set_mode(HgatpValues::Bare);
        assert!(matches!(hgatp.mode(), HgatpValues::Bare));
        assert_eq!(hgatp.bits() & (0xF << 60), 0);

        // Test setting mode to Sv39x4
        hgatp.set_mode(HgatpValues::Sv39x4);
        assert!(matches!(hgatp.mode(), HgatpValues::Sv39x4));
        assert_eq!(hgatp.bits() & (0xF << 60), 8_usize << 60);

        // Test setting mode to Sv48x4
        hgatp.set_mode(HgatpValues::Sv48x4);
        assert!(matches!(hgatp.mode(), HgatpValues::Sv48x4));
        assert_eq!(hgatp.bits() & (0xF << 60), 9_usize << 60);
    }

    #[test]
    fn test_hgatp_vmid() {
        let mut hgatp = Hgatp::from_bits(0);

        // Test setting VMID (14-bit field, bits 44-57)
        hgatp.set_vmid(0x1234);
        assert_eq!(hgatp.vmid(), 0x1234);
        assert_eq!(hgatp.bits() & (0x3FFF << 44), 0x1234 << 44);

        // Test boundary values
        hgatp.set_vmid(0);
        assert_eq!(hgatp.vmid(), 0);

        hgatp.set_vmid(0x3FFF); // Maximum 14-bit value
        assert_eq!(hgatp.vmid(), 0x3FFF);
        assert_eq!(hgatp.bits() & (0x3FFF << 44), 0x3FFF << 44);
    }

    #[test]
    fn test_hgatp_ppn() {
        let mut hgatp = Hgatp::from_bits(0);

        // Test setting PPN (44-bit field, bits 0-43)
        let test_ppn = 0x12345678ABC;
        hgatp.set_ppn(test_ppn);
        assert_eq!(hgatp.ppn(), test_ppn);
        assert_eq!(hgatp.bits() & 0xFFFFFFFFFFF, test_ppn);

        // Test boundary values
        hgatp.set_ppn(0);
        assert_eq!(hgatp.ppn(), 0);

        let max_ppn = 0xFFFFFFFFFFF; // Maximum 44-bit value
        hgatp.set_ppn(max_ppn);
        assert_eq!(hgatp.ppn(), max_ppn);
    }

    #[test]
    fn test_hgatp_values_from() {
        assert!(matches!(HgatpValues::from(0), HgatpValues::Bare));
        assert!(matches!(HgatpValues::from(8), HgatpValues::Sv39x4));
        assert!(matches!(HgatpValues::from(9), HgatpValues::Sv48x4));
    }

    #[test]
    #[should_panic]
    fn test_hgatp_values_from_invalid() {
        HgatpValues::from(7);
    }

    #[test]
    fn test_hgatp_all_fields() {
        let mut hgatp = Hgatp::from_bits(0);

        // Set all fields and verify they don't interfere
        hgatp.set_mode(HgatpValues::Sv48x4);
        hgatp.set_vmid(0x2A3F);
        hgatp.set_ppn(0x123456789AB);

        assert!(matches!(hgatp.mode(), HgatpValues::Sv48x4));
        assert_eq!(hgatp.vmid(), 0x2A3F);
        assert_eq!(hgatp.ppn(), 0x123456789AB);

        // Verify the actual bit pattern
        let expected_bits = (9_usize << 60) | (0x2A3F << 44) | 0x123456789AB;
        assert_eq!(hgatp.bits(), expected_bits);
    }

    #[test]
    fn test_hgatp_copy_clone() {
        let hgatp1 = Hgatp::from_bits(0x123456789ABCDEF0);
        let hgatp2 = hgatp1;
        let hgatp3 = hgatp1.clone();

        assert_eq!(hgatp1.bits(), hgatp2.bits());
        assert_eq!(hgatp1.bits(), hgatp3.bits());
    }
}
