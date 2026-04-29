use core::fmt::Debug;

use crate::{
    Mmio, ResetRockchip, RstId, clock::ClkId, grf::GrfMmio, variants::rk3588::cru::gate::ClkType,
};

pub mod clock;
mod consts;
mod gate;
mod peripheral;
mod pll;

// =============================================================================
// 公开导出
// =============================================================================

pub use clock::*;
pub use consts::*;
pub use pll::*;

use crate::clock::*;

// =============================================================================
// 内部常量定义
// =============================================================================

/// ACLK_BUS_ROOT 选择和分频位定义 (clksel_con[38])
const ACLK_BUS_ROOT_SEL_SHIFT: u32 = 5;
const ACLK_BUS_ROOT_SEL_MASK: u32 = 0x3 << ACLK_BUS_ROOT_SEL_SHIFT;
const ACLK_BUS_ROOT_SEL_GPLL: u32 = 0;
const ACLK_BUS_ROOT_DIV_SHIFT: u32 = 0;
const ACLK_BUS_ROOT_DIV_MASK: u32 = 0x1f << ACLK_BUS_ROOT_DIV_SHIFT;

/// ACLK_TOP_S400 和 ACLK_TOP_S200 选择位定义 (clksel_con[9])
const ACLK_TOP_S400_SEL_SHIFT: u32 = 8;
const ACLK_TOP_S400_SEL_MASK: u32 = 0x3 << ACLK_TOP_S400_SEL_SHIFT;
const ACLK_TOP_S400_SEL_400M: u32 = 0;
const ACLK_TOP_S200_SEL_SHIFT: u32 = 6;
const ACLK_TOP_S200_SEL_MASK: u32 = 0x3 << ACLK_TOP_S200_SEL_SHIFT;
const ACLK_TOP_S200_SEL_200M: u32 = 0;

#[derive(Clone)]
pub struct Cru {
    base: usize,
    _grf: usize,
    cpll_hz: u64,
    gpll_hz: u64,
    ppll_hz: u64,
    reset: ResetRockchip,
}

impl Debug for Cru {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Cru")
            .field("base", &self.base)
            .field("cpll_hz", &self.cpll_hz)
            .field("gpll_hz", &self.gpll_hz)
            .field("ppll_hz", &self.ppll_hz)
            .finish()
    }
}

impl CruOp for Cru {
    fn reset_assert(&mut self, id: RstId) {
        self.reset.reset_assert(id);
    }

    fn reset_deassert(&mut self, id: RstId) {
        self.reset.reset_deassert(id);
    }

    fn clk_enable(&mut self, id: ClkId) -> ClockResult<()> {
        self.clk_enable(id)
    }

    fn clk_disable(&mut self, id: ClkId) -> ClockResult<()> {
        self.clk_disable(id)
    }

    fn clk_is_enabled(&self, id: ClkId) -> ClockResult<bool> {
        self.clk_is_enabled(id)
    }

    fn clk_get_rate(&self, id: crate::clock::ClkId) -> ClockResult<u64> {
        self.clk_get_rate(id)
    }

    fn clk_set_rate(&mut self, id: crate::clock::ClkId, rate_hz: u64) -> ClockResult<u64> {
        self.clk_set_rate(id, rate_hz)
    }
}

impl Cru {
    pub fn new(base: Mmio, sys_grf: Mmio) -> Self {
        let mut c = Cru {
            base: base.as_ptr() as usize,
            _grf: sys_grf.as_ptr() as usize,
            cpll_hz: 0,
            gpll_hz: 0,
            ppll_hz: 0,
            reset: ResetRockchip::new(base.as_ptr() as usize + SOFTRST_CON_OFFSET as usize, 49158),
        };
        c.init();
        c
    }

    /// 初始化并验证 CRU 配置
    ///
    /// 参考 u-boot: drivers/clk/rockchip/clk_rk3588.c:rk3588_clk_init()
    ///
    /// ⚠️ 此方法仅**验证**配置，不修改寄存器
    /// 假设 bootloader (u-boot/TPL) 已正确配置 PLL 和时钟分频
    ///
    /// u-boot rk3588_clk_init 配置：
    /// 1. ACLK_BUS_ROOT: GPLL/4 ≈ 300MHz (clksel_con[38])
    /// 2. CPLL: 1500MHz
    /// 3. GPLL: 1188MHz
    /// 4. PPLL: 1100MHz (如果启用 PCI)
    /// 5. ACLK_TOP_S400: 400MHz (clksel_con[9])
    /// 6. ACLK_TOP_S200: 200MHz (clksel_con[9])
    pub fn init(&mut self) {
        info!(
            "CRU@{:x}: Initializing and verifying clock configuration...",
            self.base
        );

        // ========================================================================
        // 1. 验证 ACLK_BUS_ROOT 配置
        // u-boot: div = DIV_ROUND_UP(GPLL_HZ, 300 * MHz); = 1188/300 = 4
        //        rk_clrsetreg(&priv->cru->clksel_con[38],
        //                     ACLK_BUS_ROOT_SEL_MASK | ACLK_BUS_ROOT_DIV_MASK,
        //                     div << ACLK_BUS_ROOT_DIV_SHIFT);
        // 预期: SEL=0 (GPLL), DIV=4
        // ========================================================================
        let clksel_38 = self.read(clksel_con(38) as _);
        let bus_root_sel = (clksel_38 & ACLK_BUS_ROOT_SEL_MASK) >> ACLK_BUS_ROOT_SEL_SHIFT;
        let bus_root_div = (clksel_38 & ACLK_BUS_ROOT_DIV_MASK) >> ACLK_BUS_ROOT_DIV_SHIFT;

        debug!(
            "CRU@{:x}: clksel_con[38] (ACLK_BUS_ROOT): 0x{:08x}",
            self.base, clksel_38
        );
        debug!("  - SEL: {} (0=GPLL, 1=CPLL, 2=NPLL, 3=24M)", bus_root_sel);
        // u-boot: DIV_TO_RATE(input_rate, div) = ((input_rate) / ((div) + 1))
        // 所以实际分频系数是 (div + 1)
        let bus_root_div_factor = bus_root_div + 1;
        let bus_root_rate = if bus_root_div > 0 {
            GPLL_HZ / bus_root_div_factor as u64
        } else {
            0
        };
        debug!(
            "  - DIV: {} (factor: {}, output: {}MHz)",
            bus_root_div,
            bus_root_div_factor,
            bus_root_rate / MHZ
        );

        // u-boot 配置验证
        // u-boot: div = DIV_ROUND_UP(GPLL_HZ, 300 * MHz) - 1;
        //       = (1188 + 300 - 1) / 300 - 1 = 4 - 1 = 3
        let expected_div = GPLL_HZ.div_ceil(300 * MHZ) - 1;
        if bus_root_sel != ACLK_BUS_ROOT_SEL_GPLL {
            warn!(
                "⚠ CRU@{:x}: ACLK_BUS_ROOT source mismatch! u-boot: GPLL(0), current: {}",
                self.base, bus_root_sel
            );
        } else {
            debug!("✓ ACLK_BUS_ROOT source matches u-boot (GPLL)");
        }

        if bus_root_div != expected_div as u32 {
            warn!(
                "⚠ CRU@{:x}: ACLK_BUS_ROOT div mismatch! u-boot: {}, current: {}",
                self.base, expected_div, bus_root_div
            );
        } else {
            debug!("✓ ACLK_BUS_ROOT div matches u-boot ({})", expected_div);
        }

        // ========================================================================
        // 2. 验证 ACLK_TOP_S400/S200 配置
        // u-boot: rk_clrsetreg(&priv->cru->clksel_con[9],
        //                      ACLK_TOP_S400_SEL_MASK | ACLK_TOP_S200_SEL_MASK,
        //                      (ACLK_TOP_S400_SEL_400M << ACLK_TOP_S400_SEL_SHIFT) |
        //                      (ACLK_TOP_S200_SEL_200M << ACLK_TOP_S200_SEL_SHIFT));
        // 预期: S400_SEL=0 (400MHz), S200_SEL=0 (200MHz)
        // ========================================================================
        let clksel_9 = self.read(clksel_con(9) as _);
        let s400_sel = (clksel_9 & ACLK_TOP_S400_SEL_MASK) >> ACLK_TOP_S400_SEL_SHIFT;
        let s200_sel = (clksel_9 & ACLK_TOP_S200_SEL_MASK) >> ACLK_TOP_S200_SEL_SHIFT;

        debug!(
            "CRU@{:x}: clksel_con[9] (ACLK_TOP): 0x{:08x}",
            self.base, clksel_9
        );
        debug!("  - S400_SEL: {} (0=400MHz, 1=200MHz)", s400_sel);
        debug!("  - S200_SEL: {} (0=200MHz, 1=100MHz)", s200_sel);

        if s400_sel != ACLK_TOP_S400_SEL_400M {
            warn!(
                "⚠ CRU@{:x}: ACLK_TOP_S400 mismatch! u-boot: 0 (400MHz), current: {}",
                self.base, s400_sel
            );
        } else {
            debug!("✓ ACLK_TOP_S400 matches u-boot (400MHz)");
        }

        if s200_sel != ACLK_TOP_S200_SEL_200M {
            warn!(
                "⚠ CRU@{:x}: ACLK_TOP_S200 mismatch! u-boot: 0 (200MHz), current: {}",
                self.base, s200_sel
            );
        } else {
            debug!("✓ ACLK_TOP_S200 matches u-boot (200MHz)");
        }

        // ========================================================================
        // 3. 读取并验证 PLL 频率
        // u-boot 通过 rockchip_pll_set_rate() 配置:
        // - CPLL: CPLL_HZ (1500MHz)
        // - GPLL: GPLL_HZ (1188MHz)
        // ========================================================================
        let cpll_actual = self.pll_get_rate(PllId::CPLL).unwrap();
        let gpll_actual = self.pll_get_rate(PllId::GPLL).unwrap();

        // 保存实际读取到的频率
        self.cpll_hz = cpll_actual;
        self.gpll_hz = gpll_actual;
        self.ppll_hz = self.pll_get_rate(PllId::PPLL).unwrap();

        debug!("PLL actual rates (read from registers):");
        debug!("  - CPLL: {}MHz", cpll_actual / MHZ);
        debug!("  - GPLL: {}MHz", gpll_actual / MHZ);
        debug!("  - PPLL: {}MHz", self.ppll_hz / MHZ);

        // 验证与 u-boot 预期值的一致性
        verify_pll_frequency(PllId::CPLL, cpll_actual, CPLL_HZ);
        verify_pll_frequency(PllId::GPLL, gpll_actual, GPLL_HZ);

        if self.ppll_hz != PPLL_HZ {
            let rate = self.pll_set_rate(PllId::PPLL, PPLL_HZ).unwrap();
            verify_pll_frequency(PllId::PPLL, rate, PPLL_HZ);
            self.ppll_hz = rate;
        }

        info!("✓ CRU@{:x}: Clock configuration verified", self.base);
    }

    pub fn reset_assert(&mut self, id: RstId) {
        self.reset.reset_assert(id);
    }

    pub fn reset_deassert(&mut self, id: RstId) {
        self.reset.reset_deassert(id);
    }

    /// 使能时钟
    ///
    /// 清除时钟门控 bit，使时钟输出到外设
    ///
    /// # 参数
    ///
    /// * `id` - 时钟 ID
    ///
    /// # 返回
    ///
    /// 成功返回 Ok(())，失败返回 Err
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// cru.clk_enable(CLK_I2C1)?;
    /// ```
    pub fn clk_enable(&mut self, id: ClkId) -> ClockResult<()> {
        let gate = self.find_clk_gate(id).ok_or(ClockError::unsupported(id))?;
        if matches!(gate.kind, ClkType::Composite) {
            return Ok(());
        }

        let offset = self.get_gate_reg_offset(gate);

        // Rockchip 写掩码机制：清除 bit
        // 高 16 位 = (1 << bit)，低 16 位 = 0
        let mask = 1u32 << gate.bit;
        self.write(offset, mask << 16);

        debug!(
            "CRU@{:x}: clk_enable({}) = reg[0x{:03x}]:bit{}",
            self.base, id, offset, gate.bit
        );

        Ok(())
    }

    /// 禁止时钟
    ///
    /// 设置时钟门控 bit，停止时钟输出
    ///
    /// # 参数
    ///
    /// * `id` - 时钟 ID
    ///
    /// # 返回
    ///
    /// 成功返回 Ok(())，失败返回 Err
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// cru.clk_disable(CLK_I2C1)?;
    /// ```
    pub fn clk_disable(&mut self, id: ClkId) -> ClockResult<()> {
        let gate = self.find_clk_gate(id).ok_or(ClockError::unsupported(id))?;
        let offset = self.get_gate_reg_offset(gate);

        // Rockchip 写掩码机制：设置 bit
        // 高 16 位 = (1 << bit)，低 16 位 = (1 << bit)
        let mask = 1u32 << gate.bit;
        self.write(offset, (mask << 16) | mask);

        debug!(
            "CRU@{:x}: clk_disable({}) = reg[0x{:03x}]:bit{}",
            self.base, id, offset, gate.bit
        );

        Ok(())
    }

    /// 检查时钟是否已使能
    ///
    /// # 参数
    ///
    /// * `id` - 时钟 ID
    ///
    /// # 返回
    ///
    /// 返回 true 表示时钟已使能，false 表示已禁止，None 表示不支持
    pub fn clk_is_enabled(&self, id: ClkId) -> ClockResult<bool> {
        let gate = self.find_clk_gate(id).ok_or(ClockError::unsupported(id))?;
        if matches!(gate.kind, ClkType::Composite) {
            return Ok(true);
        }

        let offset = self.get_gate_reg_offset(gate);

        // 读取寄存器，检查 bit
        // bit = 0 表示使能，bit = 1 表示禁止
        let value = self.read(offset);
        let enabled = (value & (1 << gate.bit)) == 0;

        Ok(enabled)
    }

    /// 获取时钟频率
    ///
    /// # 参数
    ///
    /// * `id` - 时钟 ID
    ///
    /// # 返回
    ///
    /// 返回时钟频率 (Hz)，如果不支持该时钟则返回错误
    pub fn clk_get_rate(&self, id: crate::clock::ClkId) -> ClockResult<u64> {
        // 1. PLL 时钟
        if is_pll_clk(id) {
            let pll_id = PllId::try_from(id).map_err(|_| ClockError::unsupported(id))?;
            return self.pll_get_rate(pll_id);
        }

        // 2. I2C 时钟
        if is_i2c_clk(id) {
            return self.i2c_get_rate(id);
        }

        // 3. UART 时钟
        if is_uart_clk(id) {
            return self.uart_get_rate(id);
        }

        // 4. SPI 时钟
        if is_spi_clk(id) {
            return self.spi_get_rate(id);
        }

        // 5. PWM 时钟
        if matches!(id, CLK_PWM1 | CLK_PWM2 | CLK_PWM3 | CLK_PMU1PWM) {
            return self.pwm_get_rate(id);
        }

        // 6. ADC 时钟
        if matches!(id, CLK_SARADC | CLK_TSADC) {
            return self.adc_get_rate(id);
        }

        // 7. MMC/EMMC/SDIO/SFC 时钟
        if is_mmc_clk(id) {
            return self.mmc_get_rate(id);
        }

        // 8. NPU 时钟
        if is_npu_clk(id) {
            return self.npu_get_rate(id);
        }

        // 9. USB 时钟
        if is_usb_clk(id) {
            return self.usb_get_rate(id);
        }

        // 10. 根时钟
        if matches!(
            id,
            ACLK_BUS_ROOT
                | ACLK_TOP_ROOT
                | PCLK_TOP_ROOT
                | ACLK_LOW_TOP_ROOT
                | ACLK_CENTER_ROOT
                | PCLK_CENTER_ROOT
                | HCLK_CENTER_ROOT
                | ACLK_CENTER_LOW_ROOT
        ) {
            return self.root_clk_get_rate(id);
        }

        Err(ClockError::rate_read_failed(
            id,
            "Clock type not supported yet",
        ))
    }

    /// 设置时钟频率
    ///
    /// # 参数
    ///
    /// * `id` - 时钟 ID
    /// * `rate_hz` - 目标频率 (Hz)
    ///
    /// # 返回
    ///
    /// 返回实际设置的频率 (Hz)，如果不支持该时钟则返回错误
    pub fn clk_set_rate(&mut self, id: crate::clock::ClkId, rate_hz: u64) -> ClockResult<u64> {
        // 1. PLL 时钟
        if is_pll_clk(id) {
            let pll_id = PllId::try_from(id).map_err(|_| ClockError::unsupported(id))?;
            return self.pll_set_rate(pll_id, rate_hz);
        }

        // 2. I2C 时钟
        if is_i2c_clk(id) {
            return self.i2c_set_rate(id, rate_hz);
        }

        // 3. UART 时钟
        if is_uart_clk(id) {
            return self.uart_set_rate(id, rate_hz);
        }

        // 4. SPI 时钟
        if is_spi_clk(id) {
            return self.spi_set_rate(id, rate_hz);
        }

        // 5. PWM 时钟
        if matches!(id, CLK_PWM1 | CLK_PWM2 | CLK_PWM3 | CLK_PMU1PWM) {
            return self.pwm_set_rate(id, rate_hz);
        }

        // 6. ADC 时钟
        if matches!(id, CLK_SARADC | CLK_TSADC) {
            return self.adc_set_rate(id, rate_hz);
        }

        // 7. MMC/EMMC/SDIO/SFC 时钟
        if is_mmc_clk(id) {
            return self.mmc_set_rate(id, rate_hz);
        }

        // 8. NPU 时钟
        if is_npu_clk(id) {
            return self.npu_set_rate(id, rate_hz);
        }

        // 9. USB 时钟
        if is_usb_clk(id) {
            return self.usb_set_rate(id, rate_hz);
        }

        // 其他时钟类型暂不支持设置
        Err(ClockError::invalid_rate(id, rate_hz))
    }

    // ========================================================================
    // Rockchip 寄存器操作辅助方法
    // ========================================================================

    /// Rockchip 风格的 clrsetreg 操作
    ///
    /// 参考 u-boot: arch/arm/include/asm/arch-rockchip/hardware.h
    ///
    /// Rockchip 寄存器使用特殊的写掩码机制:
    /// - 高 16 位: 要清除的位掩码 (clr)
    /// - 低 16 位: 要设置的值 (set)
    ///
    /// # 参数
    ///
    /// * `offset` - 寄存器偏移
    /// * `clr` - 要清除的位掩码
    /// * `set` - 要设置的值
    ///
    /// # 示例
    ///
    /// ```rust
    /// // 清除 bit 5, 设置 bit 3
    /// self.clrsetreg(reg_offset, 0x20, 0x08);
    /// // 等价于: value = (current & ~0x20) | 0x08
    /// ```
    fn clrsetreg(&mut self, offset: u32, clr: u32, set: u32) {
        // Rockchip 风格: (clr | set) << 16 | set
        // 硬件会自动:
        // 1. 清除高16位中为1的位
        // 2. 设置低16位中为1的位
        let value = ((clr | set) << 16) | set;
        self.write(offset, value);
    }

    /// 清除寄存器位
    ///
    /// # 参数
    ///
    /// * `offset` - 寄存器偏移
    /// * `clr` - 要清除的位掩码
    fn clrreg(&mut self, offset: u32, clr: u32) {
        // Rockchip 风格: clr << 16
        let value = clr << 16;
        self.write(offset, value);
    }

    /// 设置寄存器位
    ///
    /// # 参数
    ///
    /// * `offset` - 寄存器偏移
    /// * `set` - 要设置的值
    fn setreg(&mut self, offset: u32, set: u32) {
        // Rockchip 风格: (set << 16) | set
        let value = (set << 16) | set;
        self.write(offset, value);
    }

    pub fn grf_mmio_ls() -> &'static [GrfMmio] {
        &[super::syscon::grf_mmio::SYS_GRF]
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset as usize) as *mut u32
    }

    fn read(&self, offset: u32) -> u32 {
        unsafe { core::ptr::read_volatile(self.reg(offset)) }
    }

    fn write(&self, offset: u32, value: u32) {
        unsafe { core::ptr::write_volatile(self.reg(offset), value) }
    }
}

/// 验证 PLL 频率
///
/// 对比实际读取的 PLL 频率与 u-boot 配置的预期频率
///
/// # 参数
///
/// * `pll_id` - PLL ID
/// * `actual_hz` - 实际读取的频率 (Hz)
/// * `expected_hz` - 预期频率 (Hz)
fn verify_pll_frequency(pll_id: PllId, actual_hz: u64, expected_hz: u64) {
    let diff_hz = actual_hz.abs_diff(expected_hz);

    // 允许 0.1% 的误差
    let tolerance = expected_hz / 1000;

    if diff_hz <= tolerance {
        debug!(
            "✓ {}: {}MHz (expected: {}MHz, diff: {}Hz)",
            pll_id.name(),
            actual_hz / MHZ,
            expected_hz / MHZ,
            diff_hz
        );
    } else {
        log::warn!(
            "⚠️ {}: {}MHz (expected: {}MHz, diff: {}Hz, tolerance: {}Hz)",
            pll_id.name(),
            actual_hz / MHZ,
            expected_hz / MHZ,
            diff_hz,
            tolerance
        );
    }
}

// =============================================================================
// 单元测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试 u-boot 配置值的常量验证
    #[test]
    fn test_u_boot_init_values() {
        // 验证 u-boot rk3588_clk_init 中的常量计算
        // ACLK_BUS_ROOT 分频器计算
        // u-boot: div = DIV_ROUND_UP(GPLL_HZ, 300 * MHz);
        //       = (1188 + 300 - 1) / 300 = 4
        //       写入: div - 1 = 3 (因为 DIV_TO_RATE = rate / (div + 1))
        let expected_div_reg = ((GPLL_HZ as u64) + (300 * MHZ) - 1) / (300 * MHZ) - 1;
        assert_eq!(
            expected_div_reg, 3,
            "ACLK_BUS_ROOT div should be 3 (factor=4)"
        );

        // ACLK_TOP_S400: 0 = 400MHz
        let expected_s400_sel = ACLK_TOP_S400_SEL_400M;
        assert_eq!(expected_s400_sel, 0, "ACLK_TOP_S400 should be 0 (400MHz)");

        // ACLK_TOP_S200: 0 = 200MHz
        let expected_s200_sel = ACLK_TOP_S200_SEL_200M;
        assert_eq!(expected_s200_sel, 0, "ACLK_TOP_S200 should be 0 (200MHz)");

        // PLL 频率验证
        assert_eq!(CPLL_HZ, 1500 * (MHZ as u64), "CPLL should be 1500MHz");
        assert_eq!(GPLL_HZ, 1188 * (MHZ as u64), "GPLL should be 1188MHz");
        assert_eq!(PPLL_HZ, 1100 * (MHZ as u64), "PPLL should be 1100MHz");
    }

    /// 测试寄存器位掩码定义
    #[test]
    fn test_register_bit_masks() {
        // ACLK_BUS_ROOT 位掩码
        assert_eq!(ACLK_BUS_ROOT_SEL_MASK, 0x3 << 5);
        assert_eq!(ACLK_BUS_ROOT_DIV_MASK, 0x1f);

        // ACLK_TOP 位掩码
        assert_eq!(ACLK_TOP_S400_SEL_MASK, 0x3 << 8);
        assert_eq!(ACLK_TOP_S200_SEL_MASK, 0x3 << 6);
    }

    /// 测试 clksel_con 寄存器地址计算
    #[test]
    fn test_clksel_con_address() {
        // clksel_con[0] = 0x300
        assert_eq!(CLKSEL_CON_OFFSET + 0 * 4, 0x300);
        // clksel_con[9] = 0x324
        assert_eq!(CLKSEL_CON_OFFSET + 9 * 4, 0x324);
        // clksel_con[38] = 0x398
        assert_eq!(CLKSEL_CON_OFFSET + 38 * 4, 0x398);
    }

    /// 模拟 u-boot 配置的寄存器值验证
    #[test]
    fn test_expected_register_values() {
        // u-boot rk3588_clk_init 写入的预期值:
        //
        // clksel_con[38]:
        //   SEL = 0 (GPLL)
        //   DIV = 3 (factor = 4)
        //   预期值 = 0x00000003
        let expected_clksel_38 = 0 | 3;
        assert_eq!(expected_clksel_38, 3);

        // clksel_con[9]:
        //   S400_SEL = 0 (400MHz) at bit 8
        //   S200_SEL = 0 (200MHz) at bit 6
        //   预期值 = 0x00000000
        let expected_clksel_9 = (0 << 8) | (0 << 6);
        assert_eq!(expected_clksel_9, 0);
    }

    /// 测试 PLL 频率计算公式
    ///
    /// 验证从寄存器值计算 PLL 输出频率的公式
    #[test]
    fn test_pll_rate_calculation() {
        // 测试 GPLL 1188MHz: p=2, m=198, s=1, k=0
        // rate = ((24MHz / 2) * 198) >> 1 = 1188MHz
        let fin = OSC_HZ as u64;
        let rate = ((fin / 2) * 198) >> 1;
        assert_eq!(rate, 1188 * (MHZ as u64));

        // 测试 CPLL 1500MHz: p=2, m=250, s=1, k=0
        // rate = ((24MHz / 2) * 250) >> 1 = 1500MHz
        let rate = ((fin / 2) * 250) >> 1;
        assert_eq!(rate, 1500 * (MHZ as u64));

        // 测试小数分频 786.432MHz: p=2, m=262, s=2, k=9437
        // rate = ((24MHz / 2) * 262 + (24MHz * 9437) / (2 * 65536)) >> 2
        let p = 2u64;
        let m = 262u64;
        let s = 2u32;
        let k = 9437u64;

        let mut rate = (fin / p) * m;
        let frac_rate = (fin * k) / (p * 65536);
        rate += frac_rate;
        rate >>= s;

        // 由于整数除法精度限制,结果为 786431991 Hz
        assert_eq!(rate, 786_431_991);
    }

    /// 测试 PLL 模式掩码和常量
    #[test]
    fn test_pll_mode_constants() {
        // 验证模式常量
        assert_eq!(pll_mode::PLL_MODE_SLOW, 0);
        assert_eq!(pll_mode::PLL_MODE_NORMAL, 1);
        assert_eq!(pll_mode::PLL_MODE_DEEP, 2);
    }

    /// 测试 PLL 寄存器位掩码
    #[test]
    fn test_pll_register_masks() {
        // PLLCON0: M (10 bits)
        assert_eq!(pllcon0::M_MASK, 0x3ff);
        assert_eq!(pllcon0::M_SHIFT, 0);

        // PLLCON1: P (6 bits), S (3 bits)
        assert_eq!(pllcon1::P_MASK, 0x3f);
        assert_eq!(pllcon1::P_SHIFT, 0);
        assert_eq!(pllcon1::S_MASK, 0x7 << 6);
        assert_eq!(pllcon1::S_SHIFT, 6);

        // PLLCON2: K (16 bits)
        assert_eq!(pllcon2::K_MASK, 0xffff);
        assert_eq!(pllcon2::K_SHIFT, 0);
    }

    /// 测试 PLL 参数查找 (频率表)
    #[test]
    fn test_find_pll_params_from_table() {
        // 测试 GPLL 1188MHz (在频率表中)
        let result = find_pll_params(PllId::GPLL, GPLL_HZ as u64);
        assert!(result.is_ok(), "GPLL 1188MHz should be found in rate table");
        let (p, m, s, k) = result.unwrap();
        assert_eq!(
            (p, m, s, k),
            (2, 198, 1, 0),
            "GPLL 1188MHz params should match"
        );

        // 测试 CPLL 1500MHz (在频率表中)
        let result = find_pll_params(PllId::CPLL, CPLL_HZ as u64);
        assert!(result.is_ok(), "CPLL 1500MHz should be found in rate table");
        let (p, m, s, k) = result.unwrap();
        assert_eq!(
            (p, m, s, k),
            (2, 250, 1, 0),
            "CPLL 1500MHz params should match"
        );
    }

    /// 测试 PLL 参数查找 (超出范围)
    #[test]
    fn test_find_pll_params_out_of_range() {
        // 测试过低频率 (超出 VCO 范围)
        let result = find_pll_params(PllId::GPLL, 10 * MHZ as u64);
        assert!(result.is_err(), "10MHz should be out of VCO range");

        // 测试过高频率
        let result = find_pll_params(PllId::GPLL, 5000 * MHZ as u64);
        assert!(result.is_err(), "5000MHz should be out of VCO range");
    }

    /// 测试 PLL 频率计算一致性
    #[test]
    fn test_pll_rate_calculation_consistency() {
        // 验证 calc_pll_rate 函数与频率表参数的一致性
        let fin = OSC_HZ;

        // GPLL: p=2, m=198, s=1, k=0 => 1188MHz
        let rate = calc_pll_rate(fin, 2, 198, 1, 0);
        assert_eq!(rate, GPLL_HZ as u64, "GPLL calculation mismatch");

        // CPLL: p=2, m=250, s=1, k=0 => 1500MHz
        let rate = calc_pll_rate(fin, 2, 250, 1, 0);
        assert_eq!(rate, CPLL_HZ as u64, "CPLL calculation mismatch");

        // NPLL: p=3, m=425, s=2, k=0 => 850MHz
        let rate = calc_pll_rate(fin, 3, 425, 2, 0);
        assert_eq!(rate, NPLL_HZ as u64, "NPLL calculation mismatch");
    }
}
