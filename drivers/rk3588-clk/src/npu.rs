//! NPU clock configuration for RK3588
//!
//! This module provides functions to configure and control clocks for the Neural Processing Unit (NPU).
//! The RK3588 includes multiple NPU instances (NPU0, NPU1, NPU2) that require careful clock configuration.

// Allow Clippy warnings for hardware register operations
#![allow(clippy::inconsistent_digit_grouping)]
#![allow(clippy::identity_op)]
#![allow(clippy::result_unit_err)]

use crate::{Rk3588Cru, constant::*};
use log::{debug, info};
use tock_registers::interfaces::{Readable, Writeable};

impl Rk3588Cru {
    /// Get the current clock frequency for a NPU clock ID
    ///
    /// # Arguments
    ///
    /// * `clk_id` - The NPU clock identifier (e.g., `HCLK_NPU_ROOT`, `CLK_NPU_DSU0`, `PCLK_NPU_ROOT`)
    ///
    /// # Returns
    ///
    /// Returns the clock frequency in Hz, or an error if the clock ID is unsupported.
    ///
    /// # Supported Clock IDs
    ///
    /// - `HCLK_NPU_ROOT` - NPU HCLK root clock
    /// - `CLK_NPU_DSU0` - NPU DSU0 clock
    /// - `PCLK_NPU_ROOT` - NPU PCLK root clock
    /// - `HCLK_NPU_CM0_ROOT` - NPU CM0 HCLK root clock
    /// - `CLK_NPU_CM0_RTC` - NPU CM0 RTC clock
    /// - `CLK_NPUTIMER_ROOT` - NPU timer root clock
    pub fn npu_get_clk(&self, clk_id: u32) -> Result<usize, ()> {
        let reg = &self.registers().clksel;

        let rate = match clk_id {
            HCLK_NPU_ROOT => {
                // CLKSEL_CON(73), bit[1:0], mux_200m_100m_50m_24m_p
                let val = reg.cru_clksel_con73.get();
                let mux_val = (val >> 0) & 0x3; // 提取 bit[1:0]

                match mux_val {
                    0 => 200_000_000, // 200MHz
                    1 => 100_000_000, // 100MHz
                    2 => 50_000_000,  // 50MHz
                    3 => 24_000_000,  // 24MHz
                    _ => return Err(()),
                }
            }

            CLK_NPU_DSU0 => {
                // CLKSEL_CON(73), MUX: bit[9:7], DIV: bit[6:2]
                let val = reg.cru_clksel_con73.get();
                let mux_val = (val >> 7) & 0x7; // 提取 bit[9:7]
                let div_val = (val >> 2) & 0x1F; // 提取 bit[6:2]

                let parent_rate = match mux_val {
                    0 => 1188_000_000, // GPLL
                    1 => 1000_000_000, // CPLL
                    2 => 786_432_000,  // AUPLL
                    3 => 850_000_000,  // NPLL
                    4 => 702_000_000,  // SPLL
                    _ => return Err(()),
                };

                // 分频系数 = div_val + 1
                parent_rate / ((div_val + 1) as usize)
            }

            PCLK_NPU_ROOT => {
                // CLKSEL_CON(74), bit[2:1], mux_100m_50m_24m_p
                let val = reg.cru_clksel_con74.get();
                let mux_val = (val >> 1) & 0x3;

                match mux_val {
                    0 => 100_000_000, // 100MHz
                    1 => 50_000_000,  // 50MHz
                    2 => 24_000_000,  // 24MHz
                    _ => return Err(()),
                }
            }

            HCLK_NPU_CM0_ROOT => {
                // CLKSEL_CON(74), bit[6:5], mux_400m_200m_100m_24m_p
                let val = reg.cru_clksel_con74.get();
                let mux_val = (val >> 5) & 0x3;

                match mux_val {
                    0 => 400_000_000, // 400MHz
                    1 => 200_000_000, // 200MHz
                    2 => 100_000_000, // 100MHz
                    3 => 24_000_000,  // 24MHz
                    _ => return Err(()),
                }
            }

            CLK_NPU_CM0_RTC => {
                // CLKSEL_CON(74), MUX: bit[12], DIV: bit[11:7]
                let val = reg.cru_clksel_con74.get();
                let mux_val = (val >> 12) & 0x1;
                let div_val = (val >> 7) & 0x1F;

                let parent_rate = match mux_val {
                    0 => 24_000_000, // 24MHz
                    1 => 32_768,     // 32KHz
                    _ => return Err(()),
                };

                parent_rate / ((div_val + 1) as usize)
            }

            CLK_NPUTIMER_ROOT => {
                // CLKSEL_CON(74), bit[3], mux_24m_100m_p
                let val = reg.cru_clksel_con74.get();
                let mux_val = (val >> 3) & 0x1;

                match mux_val {
                    0 => 24_000_000,  // 24MHz
                    1 => 100_000_000, // 100MHz
                    _ => return Err(()),
                }
            }

            _ => return Err(()),
        };

        Ok(rate)
    }

    /// Set the clock frequency for a NPU clock ID
    ///
    /// # Arguments
    ///
    /// * `clk_id` - The NPU clock identifier
    /// * `rate` - Target clock frequency in Hz
    ///
    /// # Returns
    ///
    /// Returns the actual clock frequency that was set, or an error if the clock ID is unsupported.
    pub fn npu_set_clk(&self, clk_id: u32, rate: usize) -> Result<usize, ()> {
        let reg = &self.registers().clksel;

        match clk_id {
            HCLK_NPU_ROOT => {
                // NODIV: 只能选择固定频率
                // mux_200m_100m_50m_24m_p
                let (mux_val, actual_rate) = if rate >= 200_000_000 {
                    (0, 200_000_000)
                } else if rate >= 100_000_000 {
                    (1, 100_000_000)
                } else if rate >= 50_000_000 {
                    (2, 50_000_000)
                } else {
                    (3, 24_000_000)
                };

                // CLKSEL_CON(73) bit[1:0]
                // HIWORD_MASK: bit[17:16] = mask, bit[1:0] = data
                reg.cru_clksel_con73.set((0x3 << 16) | mux_val);

                Ok(actual_rate)
            }

            CLK_NPU_DSU0 => {
                // 有 MUX 和 DIV，需要选择最佳组合
                // gpll_cpll_aupll_npll_spll_p
                let parents = [
                    1188_000_000, // GPLL
                    1000_000_000, // CPLL
                    786_432_000,  // AUPLL
                    850_000_000,  // NPLL
                    702_000_000,  // SPLL
                ];

                // 找到最佳的 parent + divider 组合
                let mut best_parent_idx = 0;
                let mut best_div = 1;
                let mut best_rate = 0;
                let mut min_diff = usize::MAX;

                for (idx, parent_rate) in parents.iter().enumerate() {
                    // DIV 范围: 1-32 (因为是5位，div_val = 0-31, divider = div_val + 1)
                    for div in 1..=32 {
                        let calc_rate = parent_rate / div;

                        // 寻找最接近且不超过目标频率的
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

                // CLKSEL_CON(73)
                // MUX: bit[9:7], DIV: bit[6:2]
                let div_val = (best_div - 1) as u32;
                let mux_mask = 0x7 << (7 + 16); // bit[23:21]
                let div_mask = 0x1F << (2 + 16); // bit[20:16]
                let mux_data = (best_parent_idx as u32) << 7;
                let div_data = div_val << 2;

                reg.cru_clksel_con73
                    .set(mux_mask | div_mask | mux_data | div_data);

                Ok(best_rate)
            }

            PCLK_NPU_ROOT => {
                // NODIV: mux_100m_50m_24m_p
                let (mux_val, actual_rate) = if rate >= 100_000_000 {
                    (0, 100_000_000)
                } else if rate >= 50_000_000 {
                    (1, 50_000_000)
                } else {
                    (2, 24_000_000)
                };

                // CLKSEL_CON(74) bit[2:1]
                reg.cru_clksel_con74.set((0x3 << (1 + 16)) | (mux_val << 1));

                Ok(actual_rate)
            }

            HCLK_NPU_CM0_ROOT => {
                // NODIV: mux_400m_200m_100m_24m_p
                let (mux_val, actual_rate) = if rate >= 400_000_000 {
                    (0, 400_000_000)
                } else if rate >= 200_000_000 {
                    (1, 200_000_000)
                } else if rate >= 100_000_000 {
                    (2, 100_000_000)
                } else {
                    (3, 24_000_000)
                };

                // CLKSEL_CON(74) bit[6:5]
                reg.cru_clksel_con74.set((0x3 << (5 + 16)) | (mux_val << 5));

                Ok(actual_rate)
            }

            CLK_NPU_CM0_RTC => {
                // mux_24m_32k_p，有 DIV
                let parents = [24_000_000, 32_768];

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

                // CLKSEL_CON(74)
                // MUX: bit[12], DIV: bit[11:7]
                let div_val = (best_div - 1) as u32;
                let mux_mask = 0x1 << (12 + 16);
                let div_mask = 0x1F << (7 + 16);
                let mux_data = (best_parent_idx as u32) << 12;
                let div_data = div_val << 7;

                reg.cru_clksel_con74
                    .set(mux_mask | div_mask | mux_data | div_data);

                Ok(best_rate)
            }

            CLK_NPUTIMER_ROOT => {
                // NODIV: mux_24m_100m_p
                let (mux_val, actual_rate) = if rate >= 100_000_000 {
                    (1, 100_000_000)
                } else {
                    (0, 24_000_000)
                };

                // CLKSEL_CON(74) bit[3]
                reg.cru_clksel_con74.set((0x1 << (3 + 16)) | (mux_val << 3));

                Ok(actual_rate)
            }

            _ => Err(()),
        }
    }

    /// Enable a NPU clock gate
    ///
    /// # Arguments
    ///
    /// * `gate_id` - The NPU gate identifier to enable
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` if the gate is enabled, or an error message if the gate ID is unknown.
    pub fn npu_gate_enable(&self, gate_id: u32) -> Result<bool, &'static str> {
        debug!("Enabling gate_id {}", gate_id);
        let reg = &self.registers().gate;

        match gate_id {
            CLK_NPUTIMER_ROOT => {
                // bit 7
                reg.gate_con29.set((1 << (7 + 16)) | (0 << 7));
            }
            CLK_NPU_CM0_RTC => {
                // bit 0: 掩码在 bit[16], 数据在 bit[0]
                reg.gate_con30.set((1 << (5 + 16)) | (0 << 5));
            }
            HCLK_NPU_ROOT => {
                reg.gate_con29.set((1 << (0 + 16)) | (0 << 0));
            }
            ACLK_NPU1 => {
                // bit 0: 掩码在 bit[16], 数据在 bit[0]
                reg.gate_con27.set((1 << (0 + 16)) | (0 << 0));
            }
            HCLK_NPU1 => {
                // bit 2: 掩码在 bit[18], 数据在 bit[2]
                reg.gate_con27.set((1 << (2 + 16)) | (0 << 2));
            }
            ACLK_NPU2 => {
                // bit 0
                reg.gate_con28.set((1 << (0 + 16)) | (0 << 0));
            }
            HCLK_NPU_CM0_ROOT => {
                // bit 2
                reg.gate_con30.set((1 << (1 + 16)) | (0 << 1));
            }
            HCLK_NPU2 => {
                // bit 2
                reg.gate_con28.set((1 << (2 + 16)) | (0 << 2));
            }
            FCLK_NPU_CM0_CORE => {
                // bit 3
                reg.gate_con30.set((1 << (3 + 16)) | (0 << 3));
            }
            PCLK_NPU_PVTM => {
                // bit 12
                reg.gate_con29.set((1 << (12 + 16)) | (0 << 12));
            }
            PCLK_NPU_GRF => {
                // bit 13
                reg.gate_con29.set((1 << (13 + 16)) | (0 << 13));
            }
            CLK_NPU_PVTM => {
                // bit 14
                reg.gate_con29.set((1 << (14 + 16)) | (0 << 14));
            }
            CLK_CORE_NPU_PVTM => {
                // bit 15
                reg.gate_con29.set((1 << (15 + 16)) | (0 << 15));
            }
            ACLK_NPU0 => {
                // bit 6
                reg.gate_con30.set((1 << (6 + 16)) | (0 << 6));
            }
            HCLK_NPU0 => {
                // bit 8
                reg.gate_con30.set((1 << (8 + 16)) | (0 << 8));
            }
            CLK_NPU_DSU0 => {
                // bit 5
                reg.gate_con29.set((1 << (1 + 16)) | (0 << 1));
            }
            PCLK_NPU_ROOT => {
                reg.gate_con29.set((1 << (4 + 16)) | (0 << 4));
            }
            PCLK_NPU_TIMER => {
                // bit 6
                reg.gate_con29.set((1 << (6 + 16)) | (0 << 6));
            }
            CLK_NPUTIMER0 => {
                // bit 8
                reg.gate_con29.set((1 << (8 + 16)) | (0 << 8));
            }
            CLK_NPUTIMER1 => {
                // bit 9
                reg.gate_con29.set((1 << (9 + 16)) | (0 << 9));
            }
            PCLK_NPU_WDT => {
                // bit 10
                reg.gate_con29.set((1 << (10 + 16)) | (0 << 10));
            }
            TCLK_NPU_WDT => {
                // bit 11
                reg.gate_con29.set((1 << (11 + 16)) | (0 << 11));
            }
            _ => {
                return Err("Unknown gate ID");
            }
        }

        self.npu_gate_status(gate_id)
    }

    /// Disable a NPU clock gate
    ///
    /// # Arguments
    ///
    /// * `gate_id` - The NPU gate identifier to disable
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` on success, or an error if the gate ID is unsupported.
    pub fn npu_gate_disable(&self, gate_id: u32) -> Result<bool, ()> {
        debug!("Disabling gate_id {}", gate_id);
        let reg = &self.registers().gate;

        match gate_id {
            CLK_NPUTIMER_ROOT => {
                // CLK_GATE_SET_TO_DISABLE: 写1禁用
                reg.gate_con29.set((1 << (7 + 16)) | (1 << 7));
            }
            CLK_NPU_CM0_RTC => {
                // CLK_GATE_SET_TO_DISABLE: 写1禁用
                reg.gate_con30.set((1 << (5 + 16)) | (1 << 5));
            }
            HCLK_NPU_ROOT => {
                reg.gate_con29.set((1 << (0 + 16)) | (1 << 0));
            }
            ACLK_NPU1 => {
                // CLK_GATE_SET_TO_DISABLE: 写1禁用
                reg.gate_con27.set((1 << (0 + 16)) | (1 << 0));
            }
            HCLK_NPU1 => {
                reg.gate_con27.set((1 << (2 + 16)) | (1 << 2));
            }
            ACLK_NPU2 => {
                reg.gate_con28.set((1 << (0 + 16)) | (1 << 0));
            }
            HCLK_NPU_CM0_ROOT => {
                reg.gate_con30.set((1 << (1 + 16)) | (1 << 1));
            }
            HCLK_NPU2 => {
                reg.gate_con28.set((1 << (2 + 16)) | (1 << 2));
            }
            FCLK_NPU_CM0_CORE => {
                reg.gate_con30.set((1 << (3 + 16)) | (1 << 3));
            }
            PCLK_NPU_PVTM => {
                reg.gate_con29.set((1 << (12 + 16)) | (1 << 12));
            }
            PCLK_NPU_GRF => {
                reg.gate_con29.set((1 << (13 + 16)) | (1 << 13));
            }
            CLK_NPU_PVTM => {
                reg.gate_con29.set((1 << (14 + 16)) | (1 << 14));
            }
            CLK_CORE_NPU_PVTM => {
                reg.gate_con29.set((1 << (15 + 16)) | (1 << 15));
            }
            ACLK_NPU0 => {
                reg.gate_con30.set((1 << (6 + 16)) | (1 << 6));
            }
            HCLK_NPU0 => {
                reg.gate_con30.set((1 << (8 + 16)) | (1 << 8));
            }
            CLK_NPU_DSU0 => {
                reg.gate_con29.set((1 << (1 + 16)) | (1 << 1));
            }
            PCLK_NPU_ROOT => {
                reg.gate_con29.set((1 << (4 + 16)) | (1 << 4));
            }
            PCLK_NPU_TIMER => {
                reg.gate_con29.set((1 << (6 + 16)) | (1 << 6));
            }
            CLK_NPUTIMER0 => {
                reg.gate_con29.set((1 << (8 + 16)) | (1 << 8));
            }
            CLK_NPUTIMER1 => {
                reg.gate_con29.set((1 << (9 + 16)) | (1 << 9));
            }
            PCLK_NPU_WDT => {
                reg.gate_con29.set((1 << (10 + 16)) | (1 << 10));
            }
            TCLK_NPU_WDT => {
                reg.gate_con29.set((1 << (11 + 16)) | (1 << 11));
            }
            _ => {
                return Err(());
            }
        }

        Ok(true)
    }

    /// Get the status of a NPU clock gate
    ///
    /// # Arguments
    ///
    /// * `gate_id` - The NPU gate identifier to check
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` if the gate is enabled, `Ok(false)` if disabled, or an error message if the gate ID is unknown.
    pub fn npu_gate_status(&self, gate_id: u32) -> Result<bool, &'static str> {
        debug!("Getting status for gate_id {}", gate_id);
        let reg = &self.registers().gate;

        // 读取对应寄存器的低16位，检查对应bit
        // 根据 CLK_GATE_SET_TO_DISABLE: bit=0表示使能，bit=1表示禁用
        let is_enabled = match gate_id {
            CLK_NPUTIMER_ROOT => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 7)) == 0 // bit[7]=0 表示使能
            }
            CLK_NPU_CM0_RTC => {
                let val = reg.gate_con30.get();
                info!("gate_con30 value: {:#x}", val);
                (val & (1 << 5)) == 0 // bit[5]=0 表示使能
            }
            HCLK_NPU_ROOT => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 0)) == 0 // bit[0]=0 表示使能
            }
            ACLK_NPU1 => {
                let val = reg.gate_con27.get();
                info!("gate_con27 value: {:#x}", val);
                (val & (1 << 0)) == 0 // bit[0]=0 表示使能
            }
            HCLK_NPU1 => {
                let val = reg.gate_con27.get();
                info!("gate_con27 value: {:#x}", val);
                (val & (1 << 2)) == 0
            }
            ACLK_NPU2 => {
                let val = reg.gate_con28.get();
                info!("gate_con28 value: {:#x}", val);
                (val & (1 << 0)) == 0
            }
            HCLK_NPU_CM0_ROOT => {
                let val = reg.gate_con30.get();
                info!("gate_con30 value: {:#x}", val);
                (val & (1 << 1)) == 0
            }
            HCLK_NPU2 => {
                let val = reg.gate_con28.get();
                info!("gate_con28 value: {:#x}", val);
                (val & (1 << 2)) == 0
            }
            FCLK_NPU_CM0_CORE => {
                let val = reg.gate_con30.get();
                info!("gate_con30 value: {:#x}", val);
                (val & (1 << 3)) == 0
            }
            PCLK_NPU_PVTM => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 12)) == 0
            }
            PCLK_NPU_GRF => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 13)) == 0
            }
            CLK_NPU_PVTM => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 14)) == 0
            }
            CLK_CORE_NPU_PVTM => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 15)) == 0
            }
            ACLK_NPU0 => {
                let val = reg.gate_con30.get();
                info!("gate_con30 value: {:#x}", val);
                (val & (1 << 6)) == 0
            }
            HCLK_NPU0 => {
                let val = reg.gate_con30.get();
                info!("gate_con30 value: {:#x}", val);
                (val & (1 << 8)) == 0
            }
            CLK_NPU_DSU0 => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 1)) == 0
            }
            PCLK_NPU_ROOT => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 4)) == 0
            }
            PCLK_NPU_TIMER => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 6)) == 0
            }
            CLK_NPUTIMER0 => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 8)) == 0
            }
            CLK_NPUTIMER1 => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 9)) == 0
            }
            PCLK_NPU_WDT => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 10)) == 0
            }
            TCLK_NPU_WDT => {
                let val = reg.gate_con29.get();
                info!("gate_con29 value: {:#x}", val);
                (val & (1 << 11)) == 0
            }
            _ => {
                return Err("Unknown gate ID");
            }
        };

        debug!(
            "Gate {} is {}",
            gate_id,
            if is_enabled { "enabled" } else { "disabled" }
        );
        Ok(is_enabled)
    }
}
