//! PMU register definitions and access functions
//!
//! This module provides bitfield definitions for PMU registers and safe
//! functions to read and write to hardware registers.

use core::ptr::NonNull;

use tock_registers::register_bitfields;

// Define bitfields
register_bitfields! [
    u32,

    /// PMU Power Control Register 0 (PMU_PWR_CON0)
    /// Address: 0x0000
    /// Reset value: 0x0000_0000
    /// See RK3588 TRM 7.4.2
    PMU_PWR_CON0 [
        /// Bit 0: powermode0_en (Power mode 0 enable, R/W SC)
        POWERMODE0_EN OFFSET(0) NUMBITS(1),
        /// Bit 1: pmu1_pwr_bypass (Bypass PD_PMU1 power gating flow)
        PMU1_PWR_BYPASS OFFSET(1) NUMBITS(1),
        /// Bit 2: pmu1_bus_bypass (Bypass BIU_PMU1 idle flow)
        PMU1_BUS_BYPASS OFFSET(2) NUMBITS(1),
        /// Bit 3: wakeup_bypass (Bypass waiting for wake up interrupt)
        WAKEUP_BYPASS OFFSET(3) NUMBITS(1),
        /// Bit 4: pmic_bypass (Bypass waiting for PMIC stability)
        PMIC_BYPASS OFFSET(4) NUMBITS(1),
        /// Bit 5: reset_bypass (Bypass wake up reset clear stability)
        RESET_BYPASS OFFSET(5) NUMBITS(1),
        /// Bit 6: freq_switch_bypass (Bypass frequency switch stability)
        FREQ_SWITCH_BYPASS OFFSET(6) NUMBITS(1),
        /// Bit 7: osc_dis_bypass (Bypass disable oscillator)
        OSC_DIS_BYPASS OFFSET(7) NUMBITS(1),
        /// Bit 8: pmu1_pwr_gate_ena (Enable power down PD_PMU1 by hardware)
        PMU1_PWR_GATE_ENA OFFSET(8) NUMBITS(1),
        /// Bit 9: pmu1_pwr_gate_sftena (Enable power down PD_PMU1 by software)
        PMU1_PWR_GATE_SFTENA OFFSET(9) NUMBITS(1),
        /// Bit 10: pmu1_mempwr_gate_sftena (Enable power down PD_PMU1's memory by software)
        PMU1_MEM_PWR_GATE_SFTENA OFFSET(10) NUMBITS(1),
        /// Bit 11: pmu1_bus_idle_ena (Enable sending idle request to BIU_PMU1 by hardware)
        PMU1_BUS_IDLE_ENA OFFSET(11) NUMBITS(1),
        /// Bit 12: pmu1_bus_idle_sftena (Enable sending idle request to BIU_PMU1 by software)
        PMU1_BUS_IDLE_SFTENA OFFSET(12) NUMBITS(1),
        /// Bit 13: biu_auto_pmu1 (BIU_PMU1 clock auto gate)
        BIU_AUTO_PMU1 OFFSET(13) NUMBITS(1),
        /// Bit 14: power_off_io_ena (Enable VCCIO enter low power mode)
        POWER_OFF_IO_ENA OFFSET(14) NUMBITS(1),
        /// Bit 15: reserved (RO)
        RESERVED15 OFFSET(15) NUMBITS(1),
        /// Bits 31:16: write_enable (Write enable for lower 16 bits, WO)
        WRITE_ENABLE OFFSET(16) NUMBITS(16)
    ],
];

/// PMU register access structure
///
/// Provides safe access to PMU hardware registers at a given base address.
#[derive(Clone, Copy)]
pub struct PmuRegs {
    base_addr: NonNull<u8>,
}

unsafe impl Send for PmuRegs {}

impl PmuRegs {
    /// Create a new PMU register accessor
    ///
    /// # Arguments
    ///
    /// * `base_addr` - Base address of the PMU register block
    pub const fn new(base_addr: NonNull<u8>) -> Self {
        Self { base_addr }
    }

    // fn reg<T>(&self, offset: usize) -> &T {
    //     unsafe { &*(self.base_addr.as_ptr().add(offset) as *const T) }
    // }

    // pub fn pwr_con0(&self) -> &ReadWrite<u32, PMU_PWR_CON0::Register> {
    //     self.reg(0x0)
    // }

    /// Read a 32-bit register value
    ///
    /// # Arguments
    ///
    /// * `offset` - Byte offset from the base address
    ///
    /// # Returns
    ///
    /// The 32-bit value read from the register
    pub fn read_u32(&self, offset: usize) -> u32 {
        unsafe { core::ptr::read_volatile(self.base_addr.as_ptr().add(offset) as *const u32) }
    }

    /// Write a 32-bit register value
    ///
    /// # Arguments
    ///
    /// * `offset` - Byte offset from the base address
    /// * `value` - The 32-bit value to write to the register
    pub fn write_u32(&self, offset: usize, value: u32) {
        unsafe {
            core::ptr::write_volatile(self.base_addr.as_ptr().add(offset) as *mut u32, value);
        }
    }
}
