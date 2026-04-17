//! USB clock configuration for RK3588
//!
//! This module provides functions to configure and control clocks for USB controllers.
//! The RK3588 supports multiple USB interfaces including USB3 OTG, USB3 Host, and USB2 Host.

// Allow Clippy warnings for hardware register operations
#![allow(clippy::identity_op)]
#![allow(clippy::result_unit_err)]

use crate::{OSC_HZ, Rk3588Cru, constant::*};
use log::{debug, info};
use tock_registers::interfaces::{Readable, Writeable};

/// Clock MUX parent configuration
///
/// This helper struct manages clock multiplexer configurations with parent clocks.
struct ClockMux {
    parents: &'static [usize],
}

impl ClockMux {
    /// mux_150m_50m_24m_p - Common for USB UTMI and PHP clocks
    const MUX_150M_50M_24M: ClockMux = ClockMux {
        parents: &[150_000_000, 50_000_000, OSC_HZ],
    };

    /// mux_200m_100m_50m_24m_p - Common for AHB bus clocks
    const MUX_200M_100M_50M_24M: ClockMux = ClockMux {
        parents: &[200_000_000, 100_000_000, 50_000_000, OSC_HZ],
    };

    /// Find best parent and divider for target rate
    ///
    /// # Arguments
    ///
    /// * `rate` - Target clock frequency in Hz
    /// * `max_div` - Maximum divider value
    ///
    /// # Returns
    ///
    /// Returns `Some((parent_idx, div, actual_rate))` or `None` if no valid configuration found.
    fn find_best(&self, rate: usize, max_div: usize) -> Option<(usize, usize, usize)> {
        let mut best_parent_idx = 0;
        let mut best_div = 1;
        let mut best_rate = 0;
        let mut min_diff = usize::MAX;

        for (idx, &parent_rate) in self.parents.iter().enumerate() {
            for div in 1..=max_div {
                let calc_rate = parent_rate / div;
                if calc_rate <= rate {
                    let diff = rate - calc_rate;
                    if diff < min_diff {
                        min_diff = diff;
                        best_parent_idx = idx;
                        best_div = div;
                        best_rate = calc_rate;
                    }
                }
            }
        }

        if best_rate == 0 {
            None
        } else {
            Some((best_parent_idx, best_div, best_rate))
        }
    }

    /// Select the best fixed frequency (no divider)
    ///
    /// # Arguments
    ///
    /// * `rate` - Target clock frequency in Hz
    ///
    /// # Returns
    ///
    /// Returns a tuple of (parent_idx, actual_rate).
    fn select_fixed(&self, rate: usize) -> (usize, usize) {
        for (idx, &parent_rate) in self.parents.iter().enumerate() {
            if rate >= parent_rate {
                return (idx, parent_rate);
            }
        }
        // Return the slowest option
        let idx = self.parents.len() - 1;
        (idx, self.parents[idx])
    }
}

impl Rk3588Cru {
    /// Get USB clock rate
    ///
    /// # Arguments
    ///
    /// * `clk_id` - The USB clock identifier
    ///
    /// # Returns
    ///
    /// Returns the clock frequency in Hz, or an error if the clock ID is unsupported.
    ///
    /// # Supported Clock IDs
    ///
    /// - `CLK_UTMI_USBHOST3_0` - USB3 UTMI clock
    /// - `PCLK_PHP_USBHOST3_0` - USB3 PHP APB clock
    /// - `CLK_USBPHY_480M` - USB PHY 480MHz reference clock
    /// - `ACLK_USB` - USB AXI bus clock
    /// - `HCLK_USB` - USB AHB bus clock
    pub fn usb_get_clk(&self, clk_id: u32) -> Result<usize, ()> {
        let reg = &self.registers().clksel;

        let rate = match clk_id {
            // USB3 Host/OTG2 UTMI clock
            // CLKSEL_CON(84), MUX: bit[13:12], DIV: bit[11:8]
            CLK_UTMI_USBHOST3_0 => {
                let val = reg.cru_clksel_con84.get();
                let mux_val = (val >> 12) & 0x3;
                let div_val = (val >> 8) & 0xF;
                let parent_rate = ClockMux::MUX_150M_50M_24M.parents[mux_val as usize];
                parent_rate / ((div_val + 1) as usize)
            }

            // USB3 Host PHP APB clock: CLKSEL_CON(80)[1:0], NODIV
            PCLK_PHP_USBHOST3_0 => {
                let val = reg.cru_clksel_con80.get();
                let mux_val = (val >> 0) & 0x3;
                ClockMux::MUX_150M_50M_24M.parents[mux_val as usize]
            }

            // USB PHY 480MHz reference clock
            // PMU_CLKSEL_CON(14), MUX: bit[14], DIV: bit[13:9]
            CLK_USBPHY_480M => {
                // This requires reading PMU registers (not implemented yet)
                // Typically configured to output 480MHz for USB2 PHY
                480_000_000
            }

            // USB AXI bus clock: CLKSEL_CON(170), MUX[5], DIV[4:0]
            // gpll_cpll_p
            ACLK_USB => {
                let val = reg.cru_clksel_con170.get();
                let mux_val = (val >> 5) & 0x1;
                let div_val = (val >> 0) & 0x1F;

                let parent_rate = match mux_val {
                    0 => self.gpll_hz,
                    1 => self.cpll_hz,
                    _ => return Err(()),
                };

                parent_rate / ((div_val + 1) as usize)
            }

            // USB AHB bus clock: CLKSEL_CON(170)[7:6], NODIV
            HCLK_USB => {
                let val = reg.cru_clksel_con170.get();
                let mux_val = (val >> 6) & 0x3;
                ClockMux::MUX_200M_100M_50M_24M.parents[mux_val as usize]
            }

            _ => return Err(()),
        };

        Ok(rate)
    }

    /// Set USB clock rate
    ///
    /// # Arguments
    ///
    /// * `clk_id` - The USB clock identifier
    /// * `rate` - Target clock frequency in Hz
    ///
    /// # Returns
    ///
    /// Returns the actual clock frequency that was set, or an error if the clock ID is unsupported.
    pub fn usb_set_clk(&self, clk_id: u32, rate: usize) -> Result<usize, ()> {
        let reg = &self.registers().clksel;

        match clk_id {
            // USB3 UTMI clock (Host and OTG2 share same register)
            // CLKSEL_CON(84), MUX[13:12], DIV[11:8]
            CLK_UTMI_USBHOST3_0 => {
                let (mux_idx, div, actual_rate) =
                    ClockMux::MUX_150M_50M_24M.find_best(rate, 16).ok_or(())?;

                let div_val = (div - 1) as u32;
                let mux_mask = 0x3 << (12 + 16);
                let div_mask = 0xF << (8 + 16);
                let mux_data = (mux_idx as u32) << 12;
                let div_data = div_val << 8;

                reg.cru_clksel_con84
                    .set(mux_mask | div_mask | mux_data | div_data);
                Ok(actual_rate)
            }

            // PHP APB clock: CLKSEL_CON(80)[1:0], NODIV
            PCLK_PHP_USBHOST3_0 => {
                let (mux_val, actual_rate) = ClockMux::MUX_150M_50M_24M.select_fixed(rate);
                reg.cru_clksel_con80.set((0x3 << 16) | (mux_val as u32));
                Ok(actual_rate)
            }

            // USB AXI bus clock: CLKSEL_CON(170), MUX[5], DIV[4:0]
            ACLK_USB => {
                let parents = [self.gpll_hz, self.cpll_hz];
                let mut best_parent_idx = 0;
                let mut best_div = 1;
                let mut best_rate = 0;
                let mut min_diff = usize::MAX;

                for (idx, parent_rate) in parents.iter().enumerate() {
                    for div in 1..=32 {
                        let calc_rate = parent_rate / div;
                        if calc_rate <= rate {
                            let diff = rate - calc_rate;
                            if diff < min_diff {
                                min_diff = diff;
                                best_parent_idx = idx;
                                best_div = div;
                                best_rate = calc_rate;
                            }
                        }
                    }
                }

                if best_rate == 0 {
                    return Err(());
                }

                let div_val = (best_div - 1) as u32;
                let mux_mask = 0x1 << (5 + 16);
                let div_mask = 0x1F << 16;
                let mux_data = (best_parent_idx as u32) << 5;
                let div_data = div_val;

                reg.cru_clksel_con170
                    .set(mux_mask | div_mask | mux_data | div_data);
                Ok(best_rate)
            }

            // USB AHB bus clock: CLKSEL_CON(170)[7:6], NODIV
            HCLK_USB => {
                let (mux_val, actual_rate) = ClockMux::MUX_200M_100M_50M_24M.select_fixed(rate);
                reg.cru_clksel_con170
                    .set((0x3 << (6 + 16)) | ((mux_val as u32) << 6));
                Ok(actual_rate)
            }

            _ => Err(()),
        }
    }

    /// Enable USB clock gate
    ///
    /// # Arguments
    ///
    /// * `gate_id` - The USB gate identifier to enable
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` if the gate is enabled, or an error message if the gate ID is unknown.
    pub fn usb_gate_enable(&self, gate_id: u32) -> Result<bool, &'static str> {
        debug!("Enabling USB gate_id {}", gate_id);
        let reg = &self.registers().gate;

        match gate_id {
            // USB3 OTG0 (GATE_CON(42))
            ACLK_USB3OTG0 => reg.gate_con42.set((1 << (4 + 16)) | (0 << 4)),
            CLK_SUSPEND_USB3OTG0 => reg.gate_con42.set((1 << (5 + 16)) | (0 << 5)),
            CLK_REF_USB3OTG0 => reg.gate_con42.set((1 << (6 + 16)) | (0 << 6)),

            // USB3 OTG1 (GATE_CON(42))
            ACLK_USB3OTG1 => reg.gate_con42.set((1 << (7 + 16)) | (0 << 7)),
            CLK_SUSPEND_USB3OTG1 => reg.gate_con42.set((1 << (8 + 16)) | (0 << 8)),
            CLK_REF_USB3OTG1 => reg.gate_con42.set((1 << (9 + 16)) | (0 << 9)),

            // USB3 OTG2/Host (GATE_CON(35))
            ACLK_USBHOST3_0 => reg.gate_con35.set((1 << (7 + 16)) | (0 << 7)),
            CLK_SUSPEND_USBHOST3_0 => reg.gate_con35.set((1 << (8 + 16)) | (0 << 8)),
            CLK_REF_USBHOST3_0 => reg.gate_con35.set((1 << (9 + 16)) | (0 << 9)),
            CLK_UTMI_USBHOST3_0 => reg.gate_con35.set((1 << (10 + 16)) | (0 << 10)),
            CLK_PIPE_USBHOST3_0 => reg.gate_con38.set((1 << (9 + 16)) | (0 << 9)),
            PCLK_PHP_USBHOST3_0 => reg.gate_con32.set((1 << (0 + 16)) | (0 << 0)),

            // USB2 Host0 (GATE_CON(42))1
            CLK_USBHOST0 => reg.gate_con42.set((1 << (10 + 16)) | (0 << 10)),
            CLK_USBHOST0_ARB => reg.gate_con42.set((1 << (11 + 16)) | (0 << 11)),

            // USB2 Host1 (GATE_CON(42))
            CLK_USBHOST1 => reg.gate_con42.set((1 << (12 + 16)) | (0 << 12)),
            CLK_USBHOST1_ARB => reg.gate_con42.set((1 << (13 + 16)) | (0 << 13)),

            // USB bus clocks (GATE_CON(74))
            ACLK_USB => reg.gate_con74.set((1 << (0 + 16)) | (0 << 0)),
            HCLK_USB => reg.gate_con74.set((1 << (2 + 16)) | (0 << 2)),

            _ => return Err("Unknown USB gate ID"),
        }

        self.usb_gate_status(gate_id)
    }

    /// Disable USB clock gate
    ///
    /// # Arguments
    ///
    /// * `gate_id` - The USB gate identifier to disable
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` on success, or an error if the gate ID is unsupported.
    pub fn usb_gate_disable(&self, gate_id: u32) -> Result<bool, ()> {
        debug!("Disabling USB gate_id {}", gate_id);
        let reg = &self.registers().gate;

        match gate_id {
            // USB3 OTG0 (GATE_CON(42))
            ACLK_USB3OTG0 => reg.gate_con42.set((1 << (4 + 16)) | (1 << 4)),
            CLK_SUSPEND_USB3OTG0 => reg.gate_con42.set((1 << (5 + 16)) | (1 << 5)),
            CLK_REF_USB3OTG0 => reg.gate_con42.set((1 << (6 + 16)) | (1 << 6)),

            // USB3 OTG1 (GATE_CON(42))
            ACLK_USB3OTG1 => reg.gate_con42.set((1 << (7 + 16)) | (1 << 7)),
            CLK_SUSPEND_USB3OTG1 => reg.gate_con42.set((1 << (8 + 16)) | (1 << 8)),
            CLK_REF_USB3OTG1 => reg.gate_con42.set((1 << (9 + 16)) | (1 << 9)),

            // USB3 OTG2/Host (GATE_CON(35))
            ACLK_USBHOST3_0 => reg.gate_con35.set((1 << (7 + 16)) | (1 << 7)),
            CLK_SUSPEND_USBHOST3_0 => reg.gate_con35.set((1 << (8 + 16)) | (1 << 8)),
            CLK_REF_USBHOST3_0 => reg.gate_con35.set((1 << (9 + 16)) | (1 << 9)),
            CLK_UTMI_USBHOST3_0 => reg.gate_con35.set((1 << (10 + 16)) | (1 << 10)),
            CLK_PIPE_USBHOST3_0 => reg.gate_con38.set((1 << (9 + 16)) | (1 << 9)),
            PCLK_PHP_USBHOST3_0 => reg.gate_con32.set((1 << (0 + 16)) | (1 << 0)),

            // USB2 Host0 (GATE_CON(42))
            CLK_USBHOST0 => reg.gate_con42.set((1 << (10 + 16)) | (1 << 10)),
            CLK_USBHOST0_ARB => reg.gate_con42.set((1 << (11 + 16)) | (1 << 11)),

            // USB2 Host1 (GATE_CON(42))
            CLK_USBHOST1 => reg.gate_con42.set((1 << (12 + 16)) | (1 << 12)),
            CLK_USBHOST1_ARB => reg.gate_con42.set((1 << (13 + 16)) | (1 << 13)),

            // USB bus clocks (GATE_CON(74))
            ACLK_USB => reg.gate_con74.set((1 << (0 + 16)) | (1 << 0)),
            HCLK_USB => reg.gate_con74.set((1 << (2 + 16)) | (1 << 2)),

            _ => return Err(()),
        }

        Ok(true)
    }

    /// Get USB clock gate status
    ///
    /// # Arguments
    ///
    /// * `gate_id` - The USB gate identifier to check
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` if the gate is enabled, `Ok(false)` if disabled, or an error message if the gate ID is unknown.
    pub fn usb_gate_status(&self, gate_id: u32) -> Result<bool, &'static str> {
        debug!("Getting status for USB gate_id {}", gate_id);
        let reg = &self.registers().gate;

        // Bit=0 means enabled, Bit=1 means disabled (CLK_GATE_SET_TO_DISABLE)
        let is_enabled = match gate_id {
            // USB3 OTG0
            ACLK_USB3OTG0 => {
                let val = reg.gate_con42.get();
                info!("gate_con42 value: {:#x}", val);
                (val & (1 << 4)) == 0
            }
            CLK_SUSPEND_USB3OTG0 => (reg.gate_con42.get() & (1 << 5)) == 0,
            CLK_REF_USB3OTG0 => (reg.gate_con42.get() & (1 << 6)) == 0,

            // USB3 OTG1
            ACLK_USB3OTG1 => (reg.gate_con42.get() & (1 << 7)) == 0,
            CLK_SUSPEND_USB3OTG1 => (reg.gate_con42.get() & (1 << 8)) == 0,
            CLK_REF_USB3OTG1 => (reg.gate_con42.get() & (1 << 9)) == 0,

            // USB3 OTG2/Host
            ACLK_USBHOST3_0 => {
                let val = reg.gate_con35.get();
                info!("gate_con35 value: {:#x}", val);
                (val & (1 << 7)) == 0
            }
            CLK_SUSPEND_USBHOST3_0 => (reg.gate_con35.get() & (1 << 8)) == 0,
            CLK_REF_USBHOST3_0 => (reg.gate_con35.get() & (1 << 9)) == 0,
            CLK_UTMI_USBHOST3_0 => (reg.gate_con35.get() & (1 << 10)) == 0,
            CLK_PIPE_USBHOST3_0 => {
                let val = reg.gate_con38.get();
                info!("gate_con38 value: {:#x}", val);
                (val & (1 << 9)) == 0
            }
            PCLK_PHP_USBHOST3_0 => {
                let val = reg.gate_con32.get();
                info!("gate_con32 value: {:#x}", val);
                (val & (1 << 0)) == 0
            }

            // USB2 Host0
            CLK_USBHOST0 => (reg.gate_con42.get() & (1 << 10)) == 0,
            CLK_USBHOST0_ARB => (reg.gate_con42.get() & (1 << 11)) == 0,

            // USB2 Host1
            CLK_USBHOST1 => (reg.gate_con42.get() & (1 << 12)) == 0,
            CLK_USBHOST1_ARB => (reg.gate_con42.get() & (1 << 13)) == 0,

            // USB bus clocks
            ACLK_USB => {
                let val = reg.gate_con74.get();
                info!("gate_con74 value: {:#x}", val);
                (val & (1 << 0)) == 0
            }
            HCLK_USB => (reg.gate_con74.get() & (1 << 2)) == 0,

            _ => return Err("Unknown USB gate ID"),
        };

        debug!(
            "USB Gate {} is {}",
            gate_id,
            if is_enabled { "enabled" } else { "disabled" }
        );
        Ok(is_enabled)
    }
}
