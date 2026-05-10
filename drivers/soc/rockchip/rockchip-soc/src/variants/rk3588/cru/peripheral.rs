//! RK3588 外设时钟配置
//!
//! 参考 u-boot: drivers/clk/rockchip/clk_rk3588.c

use alloc::vec::Vec;

use super::{clock::*, consts::*, *};
use crate::clock::{ClockError, ClockResult};

fn select_fixed_rate(rate_hz: u64, parents: &[u64]) -> (u32, u64) {
    for (idx, &parent_rate) in parents.iter().enumerate() {
        if rate_hz >= parent_rate {
            return (idx as u32, parent_rate);
        }
    }

    let idx = parents.len() - 1;
    (idx as u32, parents[idx])
}

fn find_best_divider(
    rate_hz: u64,
    sources: &[(u64, u32)],
    max_div: u64,
) -> Option<(u32, u64, u64)> {
    if rate_hz == 0 {
        return None;
    }

    let mut best_sel = 0;
    let mut best_div = 1;
    let mut best_rate = 0;
    let mut min_diff = u64::MAX;

    for &(parent_rate, sel) in sources {
        for div in 1..=max_div {
            let actual_rate = parent_rate / div;
            if actual_rate <= rate_hz {
                let diff = rate_hz - actual_rate;
                if diff < min_diff {
                    min_diff = diff;
                    best_sel = sel;
                    best_div = div;
                    best_rate = actual_rate;
                }
            }
        }
    }

    (best_rate != 0).then_some((best_sel, best_div, best_rate))
}

impl Cru {
    // ========================================================================
    // I2C 时钟
    // ========================================================================

    /// 获取 I2C 时钟频率
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_rk3588.c:rk3588_i2c_get_clk()
    ///
    /// I2C 时钟源选择：100MHz 或 200MHz
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持，返回 `ClockError::UnsupportedClock`
    pub(crate) fn i2c_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        let (con, sel_shift) = match id {
            CLK_I2C0 => (pmu_clksel_con(3), 6),
            CLK_I2C1 => (clksel_con(38), 6),
            CLK_I2C2 => (clksel_con(38), 7),
            CLK_I2C3 => (clksel_con(38), 8),
            CLK_I2C4 => (clksel_con(38), 9),
            CLK_I2C5 => (clksel_con(38), 10),
            CLK_I2C6 => (clksel_con(38), 11),
            CLK_I2C7 => (clksel_con(38), 12),
            CLK_I2C8 => (clksel_con(38), 13),
            _ => return Err(ClockError::unsupported(id)),
        };

        let sel = (self.read(con) >> sel_shift) & 1;
        Ok(if sel == 0 { 200 * MHZ } else { 100 * MHZ })
    }

    /// 设置 I2C 时钟频率
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_rk3588.c:rk3588_i2c_set_clk()
    ///
    /// # 时钟源
    ///
    /// - 100MHz: GPLL/12 或 CPLL/15
    /// - 200MHz: GPLL/6 或 CPLL/7.5
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持，返回 `ClockError::UnsupportedClock`
    pub(crate) fn i2c_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64> {
        let src_200m = if rate_hz >= 198 * MHZ { 0 } else { 1 };

        let (offset, mask, shift) = match id {
            CLK_I2C0 => (pmu_clksel_con(3), 1 << 6, 6),
            CLK_I2C1 => (clksel_con(38), 1 << 6, 6),
            CLK_I2C2 => (clksel_con(38), 1 << 7, 7),
            CLK_I2C3 => (clksel_con(38), 1 << 8, 8),
            CLK_I2C4 => (clksel_con(38), 1 << 9, 9),
            CLK_I2C5 => (clksel_con(38), 1 << 10, 10),
            CLK_I2C6 => (clksel_con(38), 1 << 11, 11),
            CLK_I2C7 => (clksel_con(38), 1 << 12, 12),
            CLK_I2C8 => (clksel_con(38), 1 << 13, 13),
            _ => return Err(ClockError::unsupported(id)),
        };

        self.clrsetreg(offset, mask, src_200m << shift);

        Ok(if src_200m == 0 { 200 * MHZ } else { 100 * MHZ })
    }

    // ========================================================================
    // SPI 时钟
    // ========================================================================

    /// 获取 SPI 时钟频率
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_rk3588.c:rk3588_spi_get_clk()
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持，返回 `ClockError::UnsupportedClock`
    pub(crate) fn spi_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        let con = self.read(clksel_con(59));
        let sel_shift = match id {
            CLK_SPI0 => 2,
            CLK_SPI1 => 4,
            CLK_SPI2 => 6,
            CLK_SPI3 => 8,
            CLK_SPI4 => 10,
            _ => return Err(ClockError::unsupported(id)),
        };

        let sel = (con >> sel_shift) & 0x3;
        Ok(match sel {
            0 => 200 * MHZ, // CLK_SPI_SEL_200M
            1 => 150 * MHZ, // CLK_SPI_SEL_150M
            2 => OSC_HZ,    // CLK_SPI_SEL_24M
            _ => 0,
        })
    }

    /// 设置 SPI 时钟频率
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_rk3588.c:rk3588_spi_set_clk()
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持，返回 `ClockError::UnsupportedClock`
    pub(crate) fn spi_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64> {
        let src_clk = if rate_hz >= 198 * MHZ {
            0 // CLK_SPI_SEL_200M
        } else if rate_hz >= 140 * MHZ {
            1 // CLK_SPI_SEL_150M
        } else {
            2 // CLK_SPI_SEL_24M
        };

        let (mask, shift) = match id {
            CLK_SPI0 => (0x3 << 2, 2),
            CLK_SPI1 => (0x3 << 4, 4),
            CLK_SPI2 => (0x3 << 6, 6),
            CLK_SPI3 => (0x3 << 8, 8),
            CLK_SPI4 => (0x3 << 10, 10),
            _ => return Err(ClockError::unsupported(id)),
        };

        self.clrsetreg(clksel_con(59), mask, src_clk << shift);

        Ok(match src_clk {
            0 => 200 * MHZ,
            1 => 150 * MHZ,
            2 => OSC_HZ,
            _ => 0,
        })
    }

    // ========================================================================
    // PWM 时钟
    // ========================================================================

    /// 获取 PWM 时钟频率
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_rk3588.c:rk3588_pwm_get_clk()
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持，返回 `ClockError::UnsupportedClock`
    pub(crate) fn pwm_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        let (con, sel_shift) = match id {
            CLK_PWM1 => (clksel_con(59), 12),
            CLK_PWM2 => (clksel_con(59), 14),
            CLK_PWM3 => (clksel_con(60), 0),
            CLK_PMU1PWM => (pmu_clksel_con(2), 9),
            _ => return Err(ClockError::unsupported(id)),
        };

        let sel = (self.read(con) >> sel_shift) & 0x3;
        Ok(match sel {
            0 => 100 * MHZ, // CLK_PWM_SEL_100M
            1 => 50 * MHZ,  // CLK_PWM_SEL_50M
            2 => OSC_HZ,    // CLK_PWM_SEL_24M
            _ => 0,
        })
    }

    /// 设置 PWM 时钟频率
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_rk3588.c:rk3588_pwm_set_clk()
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持，返回 `ClockError::UnsupportedClock`
    pub(crate) fn pwm_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64> {
        let src_clk = if rate_hz >= 99 * MHZ {
            0 // CLK_PWM_SEL_100M
        } else if rate_hz >= 50 * MHZ {
            1 // CLK_PWM_SEL_50M
        } else {
            2 // CLK_PWM_SEL_24M
        };

        let (offset, mask, shift) = match id {
            CLK_PWM1 => (clksel_con(59), 0x3 << 12, 12),
            CLK_PWM2 => (clksel_con(59), 0x3 << 14, 14),
            CLK_PWM3 => (clksel_con(60), 0x3, 0),
            CLK_PMU1PWM => (pmu_clksel_con(2), 0x3 << 9, 9),
            _ => return Err(ClockError::unsupported(id)),
        };

        self.clrsetreg(offset, mask, src_clk << shift);

        Ok(match src_clk {
            0 => 100 * MHZ,
            1 => 50 * MHZ,
            2 => OSC_HZ,
            _ => 0,
        })
    }

    // ========================================================================
    // ADC (SARADC/TSADC) 时钟
    // ========================================================================

    /// 获取 ADC 时钟频率
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_rk3588.c:rk3588_adc_get_clk()
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持，返回 `ClockError::UnsupportedClock`
    pub(crate) fn adc_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        Ok(match id {
            CLK_SARADC => {
                let con = self.read(clksel_con(40));
                let div = ((con & 0xFF) >> 6) as u64;
                let sel = (con >> 14) & 1;
                let prate = if sel == 1 { OSC_HZ } else { self.gpll_hz };
                prate / (div + 1)
            }
            CLK_TSADC => {
                let con = self.read(clksel_con(41));
                let div = (con & 0xFF) as u64;
                let sel = (con >> 8) & 1;
                let prate = if sel == 1 { OSC_HZ } else { 100 * MHZ };
                prate / (div + 1)
            }
            _ => return Err(ClockError::unsupported(id)),
        })
    }

    /// 设置 ADC 时钟频率
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_rk3588.c:rk3588_adc_set_clk()
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持，返回 `ClockError::UnsupportedClock`
    pub(crate) fn adc_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64> {
        Ok(match id {
            CLK_SARADC => {
                if OSC_HZ.is_multiple_of(rate_hz) {
                    let src_clk_div = (OSC_HZ / rate_hz) as u32;
                    self.clrsetreg(
                        clksel_con(40),
                        (1 << 14) | (0xFF << 6),
                        (1 << 14) | ((src_clk_div - 1) << 6),
                    );
                    OSC_HZ / (src_clk_div as u64)
                } else {
                    let src_clk_div = (self.gpll_hz / rate_hz) as u32;
                    self.clrsetreg(
                        clksel_con(40),
                        (1 << 14) | (0xFF << 6),
                        (src_clk_div - 1) << 6,
                    );
                    self.gpll_hz / (src_clk_div as u64)
                }
            }
            CLK_TSADC => {
                if OSC_HZ.is_multiple_of(rate_hz) {
                    let src_clk_div = (OSC_HZ / rate_hz).min(255) as u32;
                    self.clrsetreg(
                        clksel_con(41),
                        (1 << 8) | 0xFF,
                        (1 << 8) | (src_clk_div - 1),
                    );
                    OSC_HZ / (src_clk_div as u64)
                } else {
                    let src_clk_div = (self.gpll_hz / rate_hz).min(7) as u32;
                    self.clrsetreg(clksel_con(41), (1 << 8) | 0xFF, src_clk_div - 1);
                    100 * MHZ / (src_clk_div as u64)
                }
            }
            _ => return Err(ClockError::unsupported(id)),
        })
    }

    // ========================================================================
    // UART 时钟
    // ========================================================================

    /// 获取 UART 时钟频率
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_rk3588.c:rk3588_uart_get_rate()
    ///
    /// 注意：仅支持 SCLK_UART0-3 (ID: 632-635)
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持，返回 `ClockError::UnsupportedClock`
    pub(crate) fn uart_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        let reg = match id {
            SCLK_UART1 => 41,
            SCLK_UART2 => 43,
            SCLK_UART3 => 45,
            SCLK_UART4 => 47,
            SCLK_UART5 => 49,
            SCLK_UART6 => 51,
            SCLK_UART7 => 53,
            SCLK_UART8 => 55,
            SCLK_UART9 => 57,
            _ => return Err(ClockError::unsupported(id)),
        };

        let con = self.read(clksel_con(reg + 2));
        let src = con & 0x3;

        let con = self.read(clksel_con(reg));
        let div = ((con >> 9) & 0x1F) as u64;
        let p_src = (con >> 14) & 1;
        let p_rate = if p_src == 0 {
            self.gpll_hz
        } else {
            self.cpll_hz
        };

        Ok(match src {
            0 => p_rate / (div + 1), // CLK_UART_SEL_SRC
            1 => {
                // CLK_UART_SEL_FRAC
                let fracdiv = self.read(clksel_con(reg + 1));
                let n = (fracdiv >> 16) & 0xFFFF;
                let m = fracdiv & 0xFFFF;
                (p_rate / (div + 1)) * n as u64 / m as u64
            }
            2 => OSC_HZ, // CLK_UART_SEL_XIN24M
            _ => 0,
        })
    }

    /// 设置 UART 时钟频率
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_rk3588.c:rk3588_uart_set_rate()
    ///
    /// 注意：仅支持 SCLK_UART0-3 (ID: 632-635)
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持，返回 `ClockError::UnsupportedClock`
    pub(crate) fn uart_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64> {
        let reg = match id {
            SCLK_UART1 => 41,
            SCLK_UART2 => 43,
            SCLK_UART3 => 45,
            SCLK_UART4 => 47,
            SCLK_UART5 => 49,
            SCLK_UART6 => 51,
            SCLK_UART7 => 53,
            SCLK_UART8 => 55,
            SCLK_UART9 => 57,
            _ => return Err(ClockError::unsupported(id)),
        };

        let (clk_src, uart_src, div) = if self.gpll_hz.is_multiple_of(rate_hz) {
            (0, 0, (self.gpll_hz / rate_hz) as u32) // GPLL, SEL_SRC
        } else if self.cpll_hz.is_multiple_of(rate_hz) {
            (1, 0, (self.cpll_hz / rate_hz) as u32) // CPLL, SEL_SRC
        } else if rate_hz == OSC_HZ {
            (0, 2, 2) // GPLL, SEL_XIN24M
        } else {
            // 小数分频模式 - 简化实现
            (0, 1, 2) // GPLL, SEL_FRAC
        };

        // 配置时钟源和分频
        self.clrsetreg(
            clksel_con(reg),
            (1 << 14) | (0x1F << 9),
            (clk_src << 14) | ((div - 1) << 9),
        );

        // 配置 UART 时钟选择
        self.clrsetreg(clksel_con(reg + 2), 0x3, uart_src);

        Ok(match uart_src {
            0 => {
                if clk_src == 0 {
                    self.gpll_hz / div as u64
                } else {
                    self.cpll_hz / div as u64
                }
            }
            2 => OSC_HZ,
            _ => rate_hz,
        })
    }

    // ========================================================================
    // MMC/SDMMC 时钟
    // ========================================================================

    /// 获取 MMC 时钟频率
    ///
    /// 参考 Linux: drivers/clk/rockchip/clk-rk3588.c
    ///
    /// 支持的时钟：
    /// - CCLK_EMMC: EMMC card clock (CLKSEL_CON(77))
    /// - BCLK_EMMC: EMMC bus clock (CLKSEL_CON(78))
    /// - CCLK_SRC_SDIO: SDIO source clock (CLKSEL_CON(172))
    /// - SCLK_SFC: SFC clock (CLKSEL_CON(78))
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持，返回 `ClockError::UnsupportedClock`
    pub(crate) fn mmc_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        // 根据时钟 ID 确定寄存器和位域
        let (con_reg, sel_shift, sel_mask, div_shift, div_mask, _parent_sources): (
            u32,
            u32,
            u32,
            u32,
            u32,
            &[u64],
        ) = match id {
            CCLK_EMMC => {
                // CLksel_CON(77): sel[14:15], div[8:13]
                static PARENTS: [u64; 3] = [0, 0, 24 * MHZ];
                (
                    77,
                    clk_sel77::CCLK_EMMC_SEL_SHIFT,
                    clk_sel77::CCLK_EMMC_SEL_MASK,
                    clk_sel77::CCLK_EMMC_DIV_SHIFT,
                    clk_sel77::CCLK_EMMC_DIV_MASK,
                    &PARENTS, // 稍后填充实际值
                )
            }
            BCLK_EMMC => {
                // CLKSEL_CON(78): sel[5], div[0:4]
                static PARENTS: [u64; 2] = [0, 0];
                (
                    78,
                    clk_sel78::BCLK_EMMC_SEL_SHIFT,
                    clk_sel78::BCLK_EMMC_SEL_MASK,
                    clk_sel78::BCLK_EMMC_DIV_SHIFT,
                    clk_sel78::BCLK_EMMC_DIV_MASK,
                    &PARENTS, // 稍后填充实际值
                )
            }
            CCLK_SRC_SDIO => {
                // CLKSEL_CON(172): sel[8:9], div[2:7]
                static PARENTS: [u64; 3] = [0, 0, 24 * MHZ];
                (
                    172,
                    clk_sel172::CCLK_SDIO_SRC_SEL_SHIFT,
                    clk_sel172::CCLK_SDIO_SRC_SEL_MASK,
                    clk_sel172::CCLK_SDIO_SRC_DIV_SHIFT,
                    clk_sel172::CCLK_SDIO_SRC_DIV_MASK,
                    &PARENTS, // 稍后填充实际值
                )
            }
            SCLK_SFC => {
                // CLKSEL_CON(78): sel[12:13], div[6:11]
                static PARENTS: [u64; 3] = [0, 0, 24 * MHZ];
                (
                    78,
                    clk_sel78::SCLK_SFC_SEL_SHIFT,
                    clk_sel78::SCLK_SFC_SEL_MASK,
                    clk_sel78::SCLK_SFC_DIV_SHIFT,
                    clk_sel78::SCLK_SFC_DIV_MASK,
                    &PARENTS, // 稍后填充实际值
                )
            }
            _ => {
                return Err(ClockError::unsupported(id));
            }
        };

        // 动态填充父时钟频率
        let parents: Vec<u64> = match id {
            CCLK_EMMC | CCLK_SRC_SDIO | SCLK_SFC => {
                vec![self.gpll_hz, self.cpll_hz, 24 * MHZ]
            }
            BCLK_EMMC => vec![self.gpll_hz, self.cpll_hz],
            _ => return Err(ClockError::unsupported(id)),
        };

        // 读取寄存器
        let val = self.read(clksel_con(con_reg));

        // 提取时钟源选择和分频值
        let sel = ((val & sel_mask) >> sel_shift) as usize;
        let div = ((val & div_mask) >> div_shift) as u64;

        // 获取父时钟频率
        let parent_rate = parents
            .get(sel)
            .copied()
            .ok_or_else(|| ClockError::rate_read_failed(id, "Invalid parent clock source"))?;

        // 计算实际频率: rate = parent_rate / (div + 1)
        let rate = parent_rate / (div + 1);

        Ok(rate)
    }

    /// 设置 MMC 时钟频率
    ///
    /// 参考 Linux: drivers/clk/rockchip/clk-rk3588.c
    ///
    /// 支持的时钟：
    /// - CCLK_EMMC: EMMC card clock (CLKSEL_CON(77))
    /// - BCLK_EMMC: EMMC bus clock (CLKSEL_CON(78))
    /// - CCLK_SRC_SDIO: SDIO source clock (CLKSEL_CON(172))
    /// - SCLK_SFC: SFC clock (CLKSEL_CON(78))
    ///
    /// # 参数
    ///
    /// * `id` - 时钟 ID
    /// * `rate_hz` - 目标频率 (Hz)
    ///
    /// # 返回
    ///
    /// 返回实际设置的频率
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持或无法设置目标频率，返回错误
    pub(crate) fn mmc_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64> {
        // 根据时钟 ID 确定寄存器和位域，以及可用的时钟源
        let (con_reg, sel_shift, sel_mask, div_shift, div_mask, parent_sources): (
            u32,
            u32,
            u32,
            u32,
            u32,
            &[(u64, u32)],
        ) = match id {
            CCLK_EMMC => {
                // CLKSEL_CON(77): sel[14:15], div[8:13]
                static SOURCES: [(u64, u32); 3] = [
                    (0, clk_sel77::CCLK_EMMC_SEL_GPLL),
                    (0, clk_sel77::CCLK_EMMC_SEL_CPLL),
                    (24 * MHZ, clk_sel77::CCLK_EMMC_SEL_24M),
                ];
                (
                    77,
                    clk_sel77::CCLK_EMMC_SEL_SHIFT,
                    clk_sel77::CCLK_EMMC_SEL_MASK,
                    clk_sel77::CCLK_EMMC_DIV_SHIFT,
                    clk_sel77::CCLK_EMMC_DIV_MASK,
                    &SOURCES, // 稍后填充实际值
                )
            }
            BCLK_EMMC => {
                // CLKSEL_CON(78): sel[5], div[0:4]
                static SOURCES: [(u64, u32); 2] = [
                    (0, clk_sel78::BCLK_EMMC_SEL_GPLL),
                    (0, clk_sel78::BCLK_EMMC_SEL_CPLL),
                ];
                (
                    78,
                    clk_sel78::BCLK_EMMC_SEL_SHIFT,
                    clk_sel78::BCLK_EMMC_SEL_MASK,
                    clk_sel78::BCLK_EMMC_DIV_SHIFT,
                    clk_sel78::BCLK_EMMC_DIV_MASK,
                    &SOURCES, // 稍后填充实际值
                )
            }
            CCLK_SRC_SDIO => {
                // CLKSEL_CON(172): sel[8:9], div[2:7]
                static SOURCES: [(u64, u32); 3] = [
                    (0, clk_sel172::CCLK_SDIO_SRC_SEL_GPLL),
                    (0, clk_sel172::CCLK_SDIO_SRC_SEL_CPLL),
                    (24 * MHZ, clk_sel172::CCLK_SDIO_SRC_SEL_24M),
                ];
                (
                    172,
                    clk_sel172::CCLK_SDIO_SRC_SEL_SHIFT,
                    clk_sel172::CCLK_SDIO_SRC_SEL_MASK,
                    clk_sel172::CCLK_SDIO_SRC_DIV_SHIFT,
                    clk_sel172::CCLK_SDIO_SRC_DIV_MASK,
                    &SOURCES, // 稍后填充实际值
                )
            }
            SCLK_SFC => {
                // CLKSEL_CON(78): sel[12:13], div[6:11]
                static SOURCES: [(u64, u32); 3] = [
                    (0, clk_sel78::SCLK_SFC_SEL_GPLL),
                    (0, clk_sel78::SCLK_SFC_SEL_CPLL),
                    (24 * MHZ, clk_sel78::SCLK_SFC_SEL_24M),
                ];
                (
                    78,
                    clk_sel78::SCLK_SFC_SEL_SHIFT,
                    clk_sel78::SCLK_SFC_SEL_MASK,
                    clk_sel78::SCLK_SFC_DIV_SHIFT,
                    clk_sel78::SCLK_SFC_DIV_MASK,
                    &SOURCES, // 稍后填充实际值
                )
            }
            _ => {
                return Err(ClockError::unsupported(id));
            }
        };

        // 动态构建时钟源列表（填充实际 PLL 频率）
        let sources: Vec<(u64, u32)> = match id {
            CCLK_EMMC | CCLK_SRC_SDIO | SCLK_SFC => vec![
                (self.gpll_hz, parent_sources[0].1),
                (self.cpll_hz, parent_sources[1].1),
                (24 * MHZ, parent_sources[2].1),
            ],
            BCLK_EMMC => vec![
                (self.gpll_hz, parent_sources[0].1),
                (self.cpll_hz, parent_sources[1].1),
            ],
            _ => return Err(ClockError::unsupported(id)),
        };

        // 选择最佳时钟源和分频值
        let mut best_parent_rate = 0u64;
        let mut best_sel = 0u32;
        let mut best_div = 0u64;
        let mut min_error = u64::MAX;

        // 遍历所有可能的时钟源，找到最接近目标频率的配置
        for &(parent_rate, sel_val) in &sources {
            // 计算最佳分频值: div = parent_rate / target_rate
            let div = (parent_rate + rate_hz / 2) / rate_hz; // 四舍五入

            // 限制分频范围
            let max_div = (div_mask >> div_shift) + 1;
            let div = div.clamp(1, max_div as u64);

            // 计算实际频率
            let actual_rate = parent_rate / div;

            // 计算误差
            let error = actual_rate.abs_diff(rate_hz);

            // 如果误差更小，则更新最佳配置
            if error < min_error {
                min_error = error;
                best_parent_rate = parent_rate;
                best_sel = sel_val;
                best_div = div - 1; // 寄存器值 = div - 1
            }
        }

        // 使用 Rockchip 写掩码机制配置寄存器
        // 格式: (mask << 16) | value
        // mask = sel_mask | div_mask
        // value = (sel << sel_shift) | (div << div_shift)
        let mask = sel_mask | div_mask;
        let value = (best_sel << sel_shift) | ((best_div as u32) << div_shift);

        self.clrsetreg(clksel_con(con_reg), mask, value);

        // 返回实际频率
        let actual_rate = best_parent_rate / (best_div + 1);
        Ok(actual_rate)
    }

    // ========================================================================
    // NPU 时钟
    // ========================================================================

    /// 获取 NPU 时钟频率
    ///
    /// 参考迁移前的 RK3588 NPU clock 实现。
    pub(crate) fn npu_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        Ok(match id {
            HCLK_NPU_ROOT => {
                let sel = self.read(clksel_con(73)) & 0x3;
                match sel {
                    0 => 200 * MHZ,
                    1 => 100 * MHZ,
                    2 => 50 * MHZ,
                    3 => OSC_HZ,
                    _ => return Err(ClockError::rate_read_failed(id, "Invalid NPU HCLK source")),
                }
            }
            CLK_NPU_DSU0 => {
                const PARENTS: [u64; 5] = [GPLL_HZ, 1000 * MHZ, 786_432_000, 850 * MHZ, 702 * MHZ];

                let val = self.read(clksel_con(73));
                let sel = ((val >> 7) & 0x7) as usize;
                let div = ((val >> 2) & 0x1f) as u64;
                let parent = PARENTS
                    .get(sel)
                    .copied()
                    .ok_or_else(|| ClockError::rate_read_failed(id, "Invalid NPU DSU0 source"))?;

                parent / (div + 1)
            }
            PCLK_NPU_ROOT => {
                let sel = (self.read(clksel_con(74)) >> 1) & 0x3;
                match sel {
                    0 => 100 * MHZ,
                    1 => 50 * MHZ,
                    2 => OSC_HZ,
                    _ => return Err(ClockError::rate_read_failed(id, "Invalid NPU PCLK source")),
                }
            }
            HCLK_NPU_CM0_ROOT => {
                let sel = (self.read(clksel_con(74)) >> 5) & 0x3;
                match sel {
                    0 => 400 * MHZ,
                    1 => 200 * MHZ,
                    2 => 100 * MHZ,
                    3 => OSC_HZ,
                    _ => return Err(ClockError::rate_read_failed(id, "Invalid NPU CM0 source")),
                }
            }
            CLK_NPU_CM0_RTC => {
                let val = self.read(clksel_con(74));
                let sel = (val >> 12) & 0x1;
                let div = ((val >> 7) & 0x1f) as u64;
                let parent = if sel == 0 { OSC_HZ } else { 32_768 };

                parent / (div + 1)
            }
            CLK_NPUTIMER_ROOT => {
                if (self.read(clksel_con(74)) & (1 << 3)) == 0 {
                    OSC_HZ
                } else {
                    100 * MHZ
                }
            }
            _ => return Err(ClockError::unsupported(id)),
        })
    }

    /// 设置 NPU 时钟频率
    ///
    /// 参考迁移前的 RK3588 NPU clock 实现。
    pub(crate) fn npu_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64> {
        if rate_hz == 0 {
            return Err(ClockError::invalid_rate(id, rate_hz));
        }

        Ok(match id {
            HCLK_NPU_ROOT => {
                const PARENTS: [u64; 4] = [200 * MHZ, 100 * MHZ, 50 * MHZ, OSC_HZ];
                let (sel, actual_rate) = select_fixed_rate(rate_hz, &PARENTS);
                self.clrsetreg(clksel_con(73), 0x3, sel);
                actual_rate
            }
            CLK_NPU_DSU0 => {
                const SOURCES: [(u64, u32); 5] = [
                    (GPLL_HZ, 0),
                    (1000 * MHZ, 1),
                    (786_432_000, 2),
                    (850 * MHZ, 3),
                    (702 * MHZ, 4),
                ];
                let (sel, div, actual_rate) = find_best_divider(rate_hz, &SOURCES, 32)
                    .ok_or_else(|| ClockError::invalid_rate(id, rate_hz))?;
                self.clrsetreg(
                    clksel_con(73),
                    (0x7 << 7) | (0x1f << 2),
                    (sel << 7) | (((div - 1) as u32) << 2),
                );
                actual_rate
            }
            PCLK_NPU_ROOT => {
                const PARENTS: [u64; 3] = [100 * MHZ, 50 * MHZ, OSC_HZ];
                let (sel, actual_rate) = select_fixed_rate(rate_hz, &PARENTS);
                self.clrsetreg(clksel_con(74), 0x3 << 1, sel << 1);
                actual_rate
            }
            HCLK_NPU_CM0_ROOT => {
                const PARENTS: [u64; 4] = [400 * MHZ, 200 * MHZ, 100 * MHZ, OSC_HZ];
                let (sel, actual_rate) = select_fixed_rate(rate_hz, &PARENTS);
                self.clrsetreg(clksel_con(74), 0x3 << 5, sel << 5);
                actual_rate
            }
            CLK_NPU_CM0_RTC => {
                const SOURCES: [(u64, u32); 2] = [(OSC_HZ, 0), (32_768, 1)];
                let (sel, div, actual_rate) = find_best_divider(rate_hz, &SOURCES, 32)
                    .ok_or_else(|| ClockError::invalid_rate(id, rate_hz))?;
                self.clrsetreg(
                    clksel_con(74),
                    (1 << 12) | (0x1f << 7),
                    (sel << 12) | (((div - 1) as u32) << 7),
                );
                actual_rate
            }
            CLK_NPUTIMER_ROOT => {
                let (sel, actual_rate) = if rate_hz >= 100 * MHZ {
                    (1, 100 * MHZ)
                } else {
                    (0, OSC_HZ)
                };
                self.clrsetreg(clksel_con(74), 1 << 3, sel << 3);
                actual_rate
            }
            _ => return Err(ClockError::unsupported(id)),
        })
    }

    // ========================================================================
    // USB 时钟
    // ========================================================================

    /// 获取 USB 时钟频率
    ///
    /// 参考 Linux: drivers/clk/rockchip/clk-rk3588.c
    ///
    /// 支持的时钟：
    /// - ACLK_USB_ROOT: USB ACLK root (CLKSEL_CON(96))
    /// - HCLK_USB_ROOT: USB HCLK root (CLKSEL_CON(96))
    /// - CLK_UTMI_OTG2: UTMI clock for OTG2 (CLKSEL_CON(84))
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持或寄存器读取失败，返回错误
    pub(crate) fn usb_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        // 导入 USB clock ID 常量
        use clock::{
            ACLK_USB, ACLK_USB_ROOT, CLK_USBPHY_480M, CLK_UTMI_OTG2, HCLK_USB, HCLK_USB_ROOT,
            PCLK_PHP_USBHOST3_0,
        };

        // USB 时钟源常量
        const CLK_200M: u64 = 200 * MHZ;
        const CLK_150M: u64 = 150 * MHZ;
        const CLK_100M: u64 = 100 * MHZ;
        const CLK_50M: u64 = 50 * MHZ;

        if id == CLK_USBPHY_480M {
            return Ok(480 * MHZ);
        }

        // 根据时钟 ID 确定寄存器和位域
        let (con_reg, sel_shift, sel_mask, div_shift, div_mask, parent_sources): (
            u32,
            u32,
            u32,
            u32,
            u32,
            &[u64],
        ) = match id {
            ACLK_USB_ROOT => {
                // CLKSEL_CON(96): sel[5], div[0:4]
                static PARENTS: [u64; 2] = [0, 0];
                (
                    96,
                    clk_sel96::ACLK_USB_ROOT_SEL_SHIFT,
                    clk_sel96::ACLK_USB_ROOT_SEL_MASK,
                    clk_sel96::ACLK_USB_ROOT_DIV_SHIFT,
                    clk_sel96::ACLK_USB_ROOT_DIV_MASK,
                    &PARENTS,
                )
            }
            HCLK_USB_ROOT => {
                // CLKSEL_CON(96): sel[6:7], 无 div
                static PARENTS: [u64; 4] = [CLK_150M, CLK_100M, CLK_50M, 24 * MHZ];
                (
                    96,
                    clk_sel96::HCLK_USB_ROOT_SEL_SHIFT,
                    clk_sel96::HCLK_USB_ROOT_SEL_MASK,
                    0, // 无 div
                    0, // 无 div
                    &PARENTS,
                )
            }
            CLK_UTMI_OTG2 => {
                // CLKSEL_CON(84): sel[12:13], div[8:11]
                static PARENTS: [u64; 3] = [CLK_150M, CLK_50M, 24 * MHZ];
                (
                    84,
                    clk_sel84::CLK_UTMI_OTG2_SEL_SHIFT,
                    clk_sel84::CLK_UTMI_OTG2_SEL_MASK,
                    clk_sel84::CLK_UTMI_OTG2_DIV_SHIFT,
                    clk_sel84::CLK_UTMI_OTG2_DIV_MASK,
                    &PARENTS,
                )
            }
            PCLK_PHP_USBHOST3_0 => {
                static PARENTS: [u64; 3] = [CLK_150M, CLK_50M, 24 * MHZ];
                (80, 0, 0x3, 0, 0, &PARENTS)
            }
            ACLK_USB => {
                static PARENTS: [u64; 2] = [0, 0];
                (170, 5, 1 << 5, 0, 0x1f, &PARENTS)
            }
            HCLK_USB => {
                static PARENTS: [u64; 4] = [CLK_200M, CLK_100M, CLK_50M, 24 * MHZ];
                (170, 6, 0x3 << 6, 0, 0, &PARENTS)
            }
            _ => {
                return Err(ClockError::unsupported(id));
            }
        };

        // 动态填充父时钟频率
        let parents: Vec<u64> = match id {
            ACLK_USB_ROOT | ACLK_USB => vec![self.gpll_hz, self.cpll_hz],
            HCLK_USB_ROOT | CLK_UTMI_OTG2 | PCLK_PHP_USBHOST3_0 | HCLK_USB => {
                parent_sources.to_vec()
            }
            _ => return Err(ClockError::unsupported(id)),
        };

        // 读取寄存器
        let val = self.read(clksel_con(con_reg));

        // 提取时钟源选择
        let sel = ((val & sel_mask) >> sel_shift) as usize;

        // 获取父时钟频率
        let parent_rate = parents
            .get(sel)
            .copied()
            .ok_or_else(|| ClockError::rate_read_failed(id, "Invalid parent clock source"))?;

        // 对于无分频器的时钟，直接返回父时钟频率
        if matches!(id, HCLK_USB_ROOT | PCLK_PHP_USBHOST3_0 | HCLK_USB) {
            return Ok(parent_rate);
        }

        // 提取分频值并计算实际频率
        let div = ((val & div_mask) >> div_shift) as u64;
        let rate = parent_rate / (div + 1);

        Ok(rate)
    }

    /// 设置 USB 时钟频率
    ///
    /// 参考 Linux: drivers/clk/rockchip/clk-rk3588.c
    ///
    /// 支持的时钟：
    /// - ACLK_USB_ROOT: USB ACLK root (CLKSEL_CON(96))
    /// - CLK_UTMI_OTG2: UTMI clock for OTG2 (CLKSEL_CON(84))
    ///
    /// 注意: HCLK_USB_ROOT 是 COMPOSITE_NODIV 时钟，不支持 set_rate
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持或寄存器写入失败，返回错误
    pub(crate) fn usb_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64> {
        // 导入 USB clock ID 常量
        use clock::{
            ACLK_USB, ACLK_USB_ROOT, CLK_USBPHY_480M, CLK_UTMI_OTG2, HCLK_USB, HCLK_USB_ROOT,
            PCLK_PHP_USBHOST3_0,
        };

        const CLK_200M: u64 = 200 * MHZ;
        const CLK_150M: u64 = 150 * MHZ;
        const CLK_100M: u64 = 100 * MHZ;
        const CLK_50M: u64 = 50 * MHZ;

        if rate_hz == 0 {
            return Err(ClockError::invalid_rate(id, rate_hz));
        }

        if id == CLK_USBPHY_480M {
            return Err(ClockError::unsupported(id));
        }

        // HCLK_USB_ROOT 是 COMPOSITE_NODIV，不支持 set_rate
        if id == HCLK_USB_ROOT {
            return Err(ClockError::unsupported(id));
        }

        if id == PCLK_PHP_USBHOST3_0 {
            const PARENTS: [u64; 3] = [CLK_150M, CLK_50M, OSC_HZ];
            let (sel, actual_rate) = select_fixed_rate(rate_hz, &PARENTS);
            self.clrsetreg(clksel_con(80), 0x3, sel);
            return Ok(actual_rate);
        }

        if id == ACLK_USB {
            let sources = [(self.gpll_hz, 0), (self.cpll_hz, 1)];
            let (sel, div, actual_rate) = find_best_divider(rate_hz, &sources, 32)
                .ok_or_else(|| ClockError::invalid_rate(id, rate_hz))?;
            self.clrsetreg(
                clksel_con(170),
                (1 << 5) | 0x1f,
                (sel << 5) | ((div - 1) as u32),
            );
            return Ok(actual_rate);
        }

        if id == HCLK_USB {
            const PARENTS: [u64; 4] = [CLK_200M, CLK_100M, CLK_50M, OSC_HZ];
            let (sel, actual_rate) = select_fixed_rate(rate_hz, &PARENTS);
            self.clrsetreg(clksel_con(170), 0x3 << 6, sel << 6);
            return Ok(actual_rate);
        }

        // 根据时钟 ID 确定寄存器和位域
        let (con_reg, sel_shift, sel_mask, div_shift, div_mask, parent_sources): (
            u32,
            u32,
            u32,
            u32,
            u32,
            &[(u64, u32)],
        ) = match id {
            ACLK_USB_ROOT => {
                static SOURCES: [(u64, u32); 2] = [
                    (0, clk_sel96::ACLK_USB_ROOT_SEL_GPLL),
                    (0, clk_sel96::ACLK_USB_ROOT_SEL_CPLL),
                ];
                (
                    96,
                    clk_sel96::ACLK_USB_ROOT_SEL_SHIFT,
                    clk_sel96::ACLK_USB_ROOT_SEL_MASK,
                    clk_sel96::ACLK_USB_ROOT_DIV_SHIFT,
                    clk_sel96::ACLK_USB_ROOT_DIV_MASK,
                    &SOURCES,
                )
            }
            CLK_UTMI_OTG2 => {
                static SOURCES: [(u64, u32); 3] = [
                    (CLK_150M, clk_sel84::CLK_UTMI_OTG2_SEL_150M),
                    (CLK_50M, clk_sel84::CLK_UTMI_OTG2_SEL_50M),
                    (24 * MHZ, clk_sel84::CLK_UTMI_OTG2_SEL_24M),
                ];
                (
                    84,
                    clk_sel84::CLK_UTMI_OTG2_SEL_SHIFT,
                    clk_sel84::CLK_UTMI_OTG2_SEL_MASK,
                    clk_sel84::CLK_UTMI_OTG2_DIV_SHIFT,
                    clk_sel84::CLK_UTMI_OTG2_DIV_MASK,
                    &SOURCES,
                )
            }
            _ => {
                return Err(ClockError::unsupported(id));
            }
        };

        // 动态填充父时钟频率
        let sources: Vec<(u64, u32)> = match id {
            ACLK_USB_ROOT => vec![
                (self.gpll_hz, parent_sources[0].1),
                (self.cpll_hz, parent_sources[1].1),
            ],
            CLK_UTMI_OTG2 => parent_sources.to_vec(),
            _ => return Err(ClockError::unsupported(id)),
        };

        // 查找最佳时钟源和分频值
        let mut best_parent_rate = 0u64;
        let mut best_sel = 0u32;
        let mut best_div = 0u64;
        let mut min_error = u64::MAX;

        for &(parent_rate, sel_val) in &sources {
            // 计算最佳分频值 (四舍五入)
            let div = (parent_rate + rate_hz / 2) / rate_hz;

            // 限制分频范围
            let max_div = (div_mask >> div_shift) + 1;
            let div = div.clamp(1, max_div as u64);

            // 计算实际频率和误差
            let actual_rate = parent_rate / div;
            let error = actual_rate.abs_diff(rate_hz);

            // 如果误差更小，则更新最佳配置
            if error < min_error {
                min_error = error;
                best_parent_rate = parent_rate;
                best_sel = sel_val;
                best_div = div - 1; // 寄存器值 = div - 1
            }
        }

        // 使用 Rockchip 写掩码机制配置寄存器
        let mask = sel_mask | div_mask;
        let value = (best_sel << sel_shift) | ((best_div as u32) << div_shift);

        self.clrsetreg(clksel_con(con_reg), mask, value);

        // 返回实际频率
        let actual_rate = best_parent_rate / (best_div + 1);
        Ok(actual_rate)
    }

    // ========================================================================
    // 根时钟
    // ========================================================================

    /// 获取根时钟频率
    ///
    /// # Errors
    ///
    /// 如果时钟 ID 不支持，返回 `ClockError::UnsupportedClock`
    pub(crate) fn root_clk_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        Ok(match id {
            ACLK_BUS_ROOT => {
                let clksel_38 = self.read(clksel_con(38));
                let div = ((clksel_38 & 0x1F) + 1) as u64;
                self.gpll_hz / div
            }
            ACLK_TOP_ROOT | ACLK_LOW_TOP_ROOT => 200 * MHZ,
            PCLK_TOP_ROOT => 100 * MHZ,
            ACLK_CENTER_ROOT | PCLK_CENTER_ROOT | HCLK_CENTER_ROOT | ACLK_CENTER_LOW_ROOT => {
                self.gpll_hz / 2
            }
            _ => OSC_HZ,
        })
    }
}
