//! Clock driver for RK3588
//!
//! # Overview
//!
//! Clock is the heart of synchronous digital systems. All the events in an SoC are
//! controlled by the active edge of the clock and clock frequency is
//! often synonymous with throughput and performance.
//!
//! ## Clock tree
//! The clock tree is a hierarchical structure that distributes the clock signal
//! from a single source to various components in the system. The clock tree is
//! designed to minimize skew and ensure that all components receive the clock signal
//! at the same time. The clock tree is typically implemented using a combination of
//! buffers, inverters, and multiplexers. The clock tree is also responsible for
//! generating different clock frequencies for different components in the system.
//!
//! ## CRU
//! The Clock Reset Unit (CRU) is responsible for managing the clock and reset signals
//! for the various components in the RK3588 SoC. The CRU is responsible for generating
//! the clock signals for the CPU, GPU, NPU, and other peripherals. The CRU is also
//! responsible for managing the reset signals for the various components in the RK3588 SoC.
//!
//! # About the driver
//!
//! The driver is designed to be used in a no_std environment, and provides
//! abstractions for configuring clocks on the RK3588 SoC. It supports:
//!
//! - MMC (eMMC/SDIO) clock configuration
//! - NPU clock gate control
//! - USB clock management
//! - PLL clock management
//!
//! ## Usage
//!
//! ```rust,ignore
//! use rk3588_clk::{Rk3588Cru, constant::*};
//! use core::ptr::NonNull;
//!
//! let cru = Rk3588Cru::new(NonNull::new(clk_addr as *mut u8).unwrap());
//!
//! // Get clock frequency
//! let rate = cru.mmc_get_clk(CCLK_EMMC)?;
//!
//! // Set clock frequency
//! cru.mmc_set_clk(CCLK_EMMC, 200_000_000)?;
//!
//! // Enable NPU clock gates
//! cru.npu_gate_enable(ACLK_NPU0)?;
//! ```
//!
#![no_std]
// Allow Clippy warnings for specific patterns
#![allow(clippy::manual_is_multiple_of)]
#![allow(clippy::result_unit_err)]

extern crate alloc;

pub mod constant;
pub mod npu;
pub mod registers;
pub mod tools;
pub mod usb;

use core::ptr::NonNull;
use log::debug;
use tock_registers::interfaces::{Readable, Writeable};

use crate::{
    constant::*,
    registers::autocs::ModeRegisters,
    registers::clksel::ClkSelRegisters,
    registers::gate::GateRegisters,
    registers::pll::{AupllRegisters, CpllRegisters, GpllRegisters, NpllRegisters, V0pllRegisters},
    registers::softrst::SoftRstRegisters,
    tools::{div_round_up, div_to_rate},
};

pub const OFFSET: usize = 0x160;

pub const OSC_HZ: usize = 24 * 1000 * 1000;
pub const APLL_L_HZ: usize = 800 * 1000 * 1000;
pub const APLL_B_HZ: usize = 816 * 1000 * 1000;
pub const GPLL_HZ: usize = 1188 * 1000 * 1000;
pub const CPLL_HZ: usize = 1500 * 1000 * 1000;
pub const B0PLL_HZ: usize = 24 * 1000 * 1000;
pub const B1PLL_HZ: usize = 24 * 1000 * 1000;
pub const LPLL_HZ: usize = 24 * 1000 * 1000;
pub const V0PLL_HZ: usize = 24 * 1000 * 1000;
pub const AUPLL_HZ: usize = 786431 * 1000;
pub const NPLL_HZ: usize = 850 * 1000 * 1000;
pub const PPLL_HZ: usize = 1100 * 1000 * 1000;
pub const ACLK_CENTER_ROOT_HZ: usize = 702 * 1000 * 1000;
pub const PCLK_CENTER_ROOT_HZ: usize = 100 * 1000 * 1000;
pub const HCLK_CENTER_ROOT_HZ: usize = 396 * 1000 * 1000;
pub const ACLK_CENTER_LOW_ROOT_HZ: usize = 500 * 1000 * 1000;
pub const ACLK_TOP_ROOT_HZ: usize = 594 * 1000 * 1000;
pub const PCLK_TOP_ROOT_HZ: usize = 100 * 1000 * 1000;
pub const ACLK_LOW_TOP_ROOT_HZ: usize = 396 * 1000 * 1000;

/// RK3588 Clock and Reset Unit (CRU) driver
///
/// This struct provides an interface to configure and manage clocks on the RK3588 SoC.
/// It uses memory-mapped I/O to access the CRU registers.
pub struct Rk3588Cru {
    addr: NonNull<u8>,
    cpll_hz: usize,
    gpll_hz: usize,
}

impl Rk3588Cru {
    /// Create a new CRU driver instance
    ///
    /// # Arguments
    ///
    /// * `addr` - Base address of the CRU registers
    ///
    /// # Safety
    ///
    /// The caller must ensure that `addr` points to valid memory-mapped CRU registers.
    pub fn new(addr: NonNull<u8>) -> Self {
        Self {
            addr,
            cpll_hz: CPLL_HZ,
            gpll_hz: GPLL_HZ,
        }
    }

    /// Initialize the CRU
    ///
    /// This function can be extended to perform any necessary initialization
    /// of the CRU hardware.
    pub fn init(&self) {
        // Initialize the CRU if needed
    }

    /// Get a reference to the CRU registers
    ///
    /// # Safety
    ///
    /// The caller must ensure that the underlying memory is valid for the lifetime of the returned reference.
    pub fn registers(&self) -> &Rk3588CruRegisters {
        unsafe { &*(self.addr.as_ptr().add(OFFSET) as *const Rk3588CruRegisters) }
    }

    /// Get the current clock frequency for a MMC clock ID
    ///
    /// # Arguments
    ///
    /// * `clk_id` - The clock identifier (e.g., `CCLK_EMMC`, `CCLK_SRC_SDIO`)
    ///
    /// # Returns
    ///
    /// Returns the clock frequency in Hz, or an error if the clock ID is unsupported.
    pub fn mmc_get_clk(&self, clk_id: u32) -> Result<usize, ()> {
        debug!("Getting clk_id {}", clk_id);

        let clksel = &self.registers().clksel;

        match clk_id {
            CCLK_SRC_SDIO => {
                todo!("Implement mmc_get_clk for CCLK_SRC_SDIO");
            }
            CCLK_EMMC => {
                let config = clksel.cru_clksel_con77.get();
                let div = (config & CCLK_EMMC_DIV_MASK) >> CCLK_EMMC_DIV_SHIFT;
                let sel = (config & CCLK_EMMC_SEL_MASK) >> CCLK_EMMC_SEL_SHIFT;
                let prate = if sel == CCLK_EMMC_SEL_GPLL {
                    self.gpll_hz
                } else if sel == CCLK_EMMC_SEL_CPLL {
                    self.cpll_hz
                } else {
                    OSC_HZ
                };

                Ok(div_to_rate(prate, div))
            }
            BCLK_EMMC => {
                todo!("Implement mmc_get_clk for BCLK_EMMC");
            }
            SCLK_SFC => {
                todo!("Implement mmc_get_clk for SCLK_SFC");
            }
            DCLK_DECOM => {
                todo!("Implement mmc_get_clk for DCLK_DECOM");
            }
            _ => {
                panic!("Unsupported clk_id: {}", clk_id);
            }
        }
    }

    /// Set the clock frequency for a MMC clock ID
    ///
    /// # Arguments
    ///
    /// * `clk_id` - The MMC clock identifier (e.g., `CCLK_EMMC`, `CCLK_SRC_SDIO`)
    /// * `rate` - Target clock frequency in Hz
    ///
    /// # Returns
    ///
    /// Returns the actual clock frequency that was set, or an error if the clock ID is unsupported.
    pub fn mmc_set_clk(&self, clk_id: u32, rate: usize) -> Result<usize, ()> {
        debug!("Setting clk_id {} to rate {}", clk_id, rate);

        let clksel = &self.registers().clksel;

        let (src_clk, div) = match clk_id {
            CCLK_SRC_SDIO => {
                todo!("Implement mmc_set_clk for CCLK_SRC_SDIO");
            }
            CCLK_EMMC => {
                if OSC_HZ % rate == 0 {
                    let div = div_round_up(OSC_HZ, rate);
                    (SCLK_SFC_SEL_24M, div)
                } else if self.cpll_hz % rate == 0 {
                    let div = div_round_up(self.cpll_hz, rate);
                    (SCLK_SFC_SEL_CPLL, div)
                } else {
                    let div = div_round_up(self.gpll_hz, rate);
                    (SCLK_SFC_SEL_GPLL, div)
                }
            }
            BCLK_EMMC => {
                todo!("Implement mmc_set_clk for BCLK_EMMC");
            }
            SCLK_SFC => {
                todo!("Implement mmc_set_clk for SCLK_SFC");
            }
            DCLK_DECOM => {
                todo!("Implement mmc_set_clk for DCLK_DECOM");
            }
            _ => {
                return Err(());
            }
        };

        match clk_id {
            CCLK_EMMC => {
                let new_value =
                    (src_clk << CCLK_EMMC_SEL_SHIFT) | (((div as u32) - 1) << CCLK_EMMC_DIV_SHIFT);
                let mask = CCLK_EMMC_SEL_MASK | CCLK_EMMC_DIV_MASK;
                let final_value = (mask | new_value) << 16 | new_value;

                debug!(
                    "CCLK_EMMC: src_clk {}, div {}, new_value {:#x}, final_value {:#x}",
                    src_clk, div, new_value, final_value
                );

                clksel.cru_clksel_con77.set(final_value);
            }
            _ => {
                return Err(());
            }
        }

        match self.mmc_get_clk(clk_id) {
            Ok(freq) => Ok(freq),
            Err(_) => Err(()),
        }
    }
}

/// CRU register layout for RK3588
///
/// This struct represents the memory-mapped register layout of the Clock Reset Unit.
#[repr(C)]
pub struct Rk3588CruRegisters {
    v0pll: V0pllRegisters, // 0x160
    aupll: AupllRegisters, // 0x180
    cpll: CpllRegisters,   // 0x1A0
    gpll: GpllRegisters,   // 0x1C0
    npll: NpllRegisters,   // 0x1E0
    _reserved0: [u8; 0x80],
    mode: ModeRegisters,     // 0x280
    clksel: ClkSelRegisters, // 0x300
    _reserved2: [u8; 0x200],
    gate: GateRegisters, // 0x800
    _reserved3: [u8; 0xC8],
    softrst: SoftRstRegisters, // 0xA00
}
