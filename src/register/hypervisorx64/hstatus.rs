//! Hypervisor Status Register.
//!
//! The `hstatus` register provides status and control information for the hypervisor.
//! It contains fields that control virtual machine execution, interrupt handling,
//! and memory management in virtualized environments.
//!
//! This register is central to RISC-V hypervisor operation and includes fields such as:
//! - Virtual machine privilege and execution state
//! - Guest virtual address translation controls  
//! - Virtual interrupt management
//! - Hypervisor user mode support

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, set_clear_csr, write_csr};

/// Hypervisor Status Register
#[derive(Copy, Clone, Debug)]
pub struct Hstatus {
    bits: usize,
}

impl Hstatus {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Hstatus { bits: x }
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
    /// Returns the effective XLEN for VS-mode.
    #[inline]
    pub fn vsxl(&self) -> VsxlValues {
        VsxlValues::from(self.bits.get_bits(32..34))
    }
    /// Sets the effective XLEN for VS-mode.
    #[inline]
    pub fn set_vsxl(&mut self, val: VsxlValues) {
        self.bits.set_bits(32..34, val as usize);
    }
    /// Returns the TSR for VS-mode.
    #[inline]
    pub fn vtsr(&self) -> bool {
        self.bits.get_bit(22)
    }
    /// Sets the TSR for VS-mode.
    #[inline]
    pub fn set_vtsr(&mut self, val: bool) {
        self.bits.set_bit(22, val);
    }
    /// Returns the TW for VS-mode.
    #[inline]
    pub fn vtw(&self) -> bool {
        self.bits.get_bit(21)
    }
    /// Sets the TW for VS-mode.
    #[inline]
    pub fn set_vtw(&mut self, val: bool) {
        self.bits.set_bit(21, val);
    }
    /// Returns the TVM for VS-mode.
    #[inline]
    pub fn vtvm(&self) -> bool {
        self.bits.get_bit(20)
    }
    /// Sets the TVM for VS-mode.
    #[inline]
    pub fn set_vtvm(&mut self, val: bool) {
        self.bits.set_bit(20, val);
    }
    /// Returns the virtual guest external interrupt number.
    #[inline]
    pub fn vgein(&self) -> usize {
        self.bits.get_bits(12..18)
    }
    /// Sets the virtual guest external interrupt number.
    #[inline]
    pub fn set_vgein(&mut self, val: usize) {
        self.bits.set_bits(12..18, val);
    }
    /// Returns the hypervisor user mode status.
    #[inline]
    pub fn hu(&self) -> bool {
        self.bits.get_bit(9)
    }
    /// Sets the hypervisor user mode status.
    #[inline]
    pub fn set_hu(&mut self, val: bool) {
        self.bits.set_bit(9, val);
    }
    /// Returns the supervisor previous virtual privilege.
    #[inline]
    pub fn spvp(&self) -> bool {
        self.bits.get_bit(8)
    }
    /// Sets the supervisor previous virtual privilege.
    #[inline]
    pub fn set_spvp(&mut self, val: bool) {
        self.bits.set_bit(8, val);
    }
    /// Returns the supervisor previous virtualization mode.
    #[inline]
    pub fn spv(&self) -> bool {
        self.bits.get_bit(7)
    }
    /// Sets the supervisor previous virtualization mode.
    #[inline]
    pub fn set_spv(&mut self, val: bool) {
        self.bits.set_bit(7, val);
    }
    /// Returns the guest virtual address status.
    #[inline]
    pub fn gva(&self) -> bool {
        self.bits.get_bit(6)
    }
    /// Sets the guest virtual address status.
    #[inline]
    pub fn set_gva(&mut self, val: bool) {
        self.bits.set_bit(6, val);
    }
    /// Returns the VS-mode memory access endianness.
    #[inline]
    pub fn vsbe(&self) -> bool {
        self.bits.get_bit(5)
    }
    /// Sets the VS-mode memory access endianness.
    #[inline]
    pub fn set_vsbe(&mut self, val: bool) {
        self.bits.set_bit(5, val);
    }
}

read_csr_as!(Hstatus, 0x600);
write_csr!(0x600);
set!(0x600);
clear!(0x600);

// bit ops
set_clear_csr!(
    /// TSR for VS-mode enable.
    , set_vtsr, clear_vtsr, 1 << 22);
set_clear_csr!(
    /// TW for VS-mode enable.
    , set_vtw, clear_vtw, 1 << 21);
set_clear_csr!(
    /// TVM for VS-mode enable.
    , set_vtvm, clear_vtvm, 1 << 20);
set_clear_csr!(
    /// Hypervisor user mode enable.
    , set_hu, clear_hu, 1 << 9);
set_clear_csr!(
    /// Supervisor previous virtual privilege enable.
    , set_spvp, clear_spvp, 1 << 8);
set_clear_csr!(
    /// Supervisor previous virtualization mode enable.
    , set_spv, clear_spv, 1 << 7);
set_clear_csr!(
    /// Guest virtual address enable.
    , set_gva, clear_gva, 1 << 6);
set_clear_csr!(
    /// VS-mode memory access endianness enable.
    , set_vsbe, clear_vsbe, 1 << 5);

/// Virtual Supervisor Address Translation and Protection Register values.
#[derive(Copy, Clone, Debug)]
#[repr(usize)]
pub enum VsxlValues {
    /// 32-bit virtual address space
    Vsxl32 = 1,
    /// 64-bit virtual address space
    Vsxl64 = 2,
    /// 128-bit virtual address space
    Vsxl128 = 3,
}

impl VsxlValues {
    fn from(x: usize) -> Self {
        match x {
            1 => Self::Vsxl32,
            2 => Self::Vsxl64,
            3 => Self::Vsxl128,
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hstatus_from_bits() {
        let hstatus = Hstatus::from_bits(0x12345678);
        assert_eq!(hstatus.bits(), 0x12345678);
    }

    #[test]
    fn test_hstatus_vsxl() {
        let mut hstatus = Hstatus::from_bits(0);

        // Test setting VSXL to 32-bit
        hstatus.set_vsxl(VsxlValues::Vsxl32);
        assert_eq!(hstatus.vsxl() as usize, 1);
        assert_eq!(hstatus.bits() & (0b11 << 32), 1 << 32);

        // Test setting VSXL to 64-bit
        hstatus.set_vsxl(VsxlValues::Vsxl64);
        assert_eq!(hstatus.vsxl() as usize, 2);
        assert_eq!(hstatus.bits() & (0b11 << 32), 2 << 32);

        // Test setting VSXL to 128-bit
        hstatus.set_vsxl(VsxlValues::Vsxl128);
        assert_eq!(hstatus.vsxl() as usize, 3);
        assert_eq!(hstatus.bits() & (0b11 << 32), 3 << 32);
    }

    #[test]
    fn test_hstatus_boolean_fields() {
        let mut hstatus = Hstatus::from_bits(0);

        // Test VTSR bit (bit 22)
        assert!(!hstatus.vtsr());
        hstatus.set_vtsr(true);
        assert!(hstatus.vtsr());
        assert_eq!(hstatus.bits() & (1 << 22), 1 << 22);
        hstatus.set_vtsr(false);
        assert!(!hstatus.vtsr());
        assert_eq!(hstatus.bits() & (1 << 22), 0);

        // Test VTW bit (bit 21)
        assert!(!hstatus.vtw());
        hstatus.set_vtw(true);
        assert!(hstatus.vtw());
        assert_eq!(hstatus.bits() & (1 << 21), 1 << 21);

        // Test VTVM bit (bit 20)
        assert!(!hstatus.vtvm());
        hstatus.set_vtvm(true);
        assert!(hstatus.vtvm());
        assert_eq!(hstatus.bits() & (1 << 20), 1 << 20);

        // Test HU bit (bit 9)
        assert!(!hstatus.hu());
        hstatus.set_hu(true);
        assert!(hstatus.hu());
        assert_eq!(hstatus.bits() & (1 << 9), 1 << 9);

        // Test SPVP bit (bit 8)
        assert!(!hstatus.spvp());
        hstatus.set_spvp(true);
        assert!(hstatus.spvp());
        assert_eq!(hstatus.bits() & (1 << 8), 1 << 8);

        // Test SPV bit (bit 7)
        assert!(!hstatus.spv());
        hstatus.set_spv(true);
        assert!(hstatus.spv());
        assert_eq!(hstatus.bits() & (1 << 7), 1 << 7);

        // Test GVA bit (bit 6)
        assert!(!hstatus.gva());
        hstatus.set_gva(true);
        assert!(hstatus.gva());
        assert_eq!(hstatus.bits() & (1 << 6), 1 << 6);

        // Test VSBE bit (bit 5)
        assert!(!hstatus.vsbe());
        hstatus.set_vsbe(true);
        assert!(hstatus.vsbe());
        assert_eq!(hstatus.bits() & (1 << 5), 1 << 5);
    }

    #[test]
    fn test_hstatus_vgein() {
        let mut hstatus = Hstatus::from_bits(0);

        // Test setting VGEIN to various values (6-bit field, bits 12-17)
        hstatus.set_vgein(0x15); // 21 in decimal
        assert_eq!(hstatus.vgein(), 0x15);
        assert_eq!(hstatus.bits() & (0x3F << 12), 0x15 << 12);

        // Test boundary values
        hstatus.set_vgein(0);
        assert_eq!(hstatus.vgein(), 0);

        hstatus.set_vgein(0x3F); // Maximum 6-bit value
        assert_eq!(hstatus.vgein(), 0x3F);
        assert_eq!(hstatus.bits() & (0x3F << 12), 0x3F << 12);
    }

    #[test]
    fn test_vsxl_values_from() {
        assert!(matches!(VsxlValues::from(1), VsxlValues::Vsxl32));
        assert!(matches!(VsxlValues::from(2), VsxlValues::Vsxl64));
        assert!(matches!(VsxlValues::from(3), VsxlValues::Vsxl128));
    }

    #[test]
    #[should_panic]
    fn test_vsxl_values_from_invalid() {
        VsxlValues::from(0);
    }

    #[test]
    fn test_hstatus_multiple_fields() {
        let mut hstatus = Hstatus::from_bits(0);

        // Set multiple fields and verify they don't interfere
        hstatus.set_vtsr(true);
        hstatus.set_vgein(0x2A);
        hstatus.set_hu(true);
        hstatus.set_vsxl(VsxlValues::Vsxl64);

        assert!(hstatus.vtsr());
        assert_eq!(hstatus.vgein(), 0x2A);
        assert!(hstatus.hu());
        assert!(matches!(hstatus.vsxl(), VsxlValues::Vsxl64));

        // Verify the actual bit pattern
        let expected_bits = (1 << 22) | (0x2A << 12) | (1 << 9) | (2 << 32);
        assert_eq!(hstatus.bits(), expected_bits);
    }

    #[test]
    fn test_hstatus_copy_clone() {
        let hstatus1 = Hstatus::from_bits(0x12345678);
        let hstatus2 = hstatus1;
        let hstatus3 = hstatus1.clone();

        assert_eq!(hstatus1.bits(), hstatus2.bits());
        assert_eq!(hstatus1.bits(), hstatus3.bits());
    }
}
