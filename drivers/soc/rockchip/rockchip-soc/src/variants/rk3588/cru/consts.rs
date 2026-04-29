//! RK3588 CRU 寄存器常量定义
//!
//! 参考 u-boot-orangepi/arch/arm/include/asm/arch-rockchip/cru_rk3588.h

#![allow(dead_code)]

// ============================================================================
// 频率常量
// ============================================================================

pub(crate) use crate::variants::MHZ;

pub const KHZ: u32 = 1_000;
pub const OSC_HZ: u64 = 24 * MHZ;

pub const CPU_PVTPLL_HZ: u64 = 1008 * MHZ;
pub const LPLL_HZ: u64 = 816 * MHZ;
pub const GPLL_HZ: u64 = 1188 * MHZ;
pub const CPLL_HZ: u64 = 1500 * MHZ;
pub const NPLL_HZ: u64 = 850 * MHZ;
pub const PPLL_HZ: u64 = 1100 * MHZ;

// ============================================================================
// CRU 基地址偏移
// ============================================================================

pub const RK3588_PHP_CRU_BASE: u32 = 0x8000;
pub const RK3588_PMU_CRU_BASE: u32 = 0x30000;
pub const RK3588_BIGCORE0_CRU_BASE: u32 = 0x50000;
pub const RK3588_BIGCORE1_CRU_BASE: u32 = 0x52000;
pub const RK3588_DSU_CRU_BASE: u32 = 0x58000;

// ============================================================================
// 主 CRU 寄存器偏移
// ============================================================================

/// PLL 配置寄存器偏移
pub const fn pll_con(x: u32) -> u32 {
    x * 0x4
}

/// 模式控制寄存器偏移
pub const RK3588_MODE_CON0: u32 = 0x280;

/// clksel_con 寄存器基址偏移
pub const CLKSEL_CON_OFFSET: u32 = 0x0300;

pub const SOFTRST_CON_OFFSET: u32 = 0x0a00;

/// 时钟选择寄存器偏移
pub const fn clksel_con(x: u32) -> u32 {
    (x * 0x4) + CLKSEL_CON_OFFSET
}

/// 时钟门控寄存器偏移
pub const fn clkgate_con(x: u32) -> u32 {
    x * 0x4 + 0x800
}

/// 软件复位寄存器偏移
pub const fn softrst_con(x: u32) -> u32 {
    x * 0x4 + SOFTRST_CON_OFFSET
}

/// 全局计数阈值寄存器
pub const RK3588_GLB_CNT_TH: u32 = 0xc00;

/// 全局复位状态寄存器
pub const RK3588_GLB_RST_ST: u32 = 0xc04;

/// 全局第一级软件复位寄存器
pub const RK3588_GLB_SRST_FST: u32 = 0xc08;

/// 全局第二级软件复位寄存器
pub const RK3588_GLB_SRST_SND: u32 = 0xc0c;

/// 全局复位控制寄存器
pub const RK3588_GLB_RST_CON: u32 = 0xc10;

/// SDIO 配置寄存器
pub const RK3588_SDIO_CON0: u32 = 0xC24;
pub const RK3588_SDIO_CON1: u32 = 0xC28;

/// SDMMC 配置寄存器
pub const RK3588_SDMMC_CON0: u32 = 0xC30;
pub const RK3588_SDMMC_CON1: u32 = 0xC34;

// ============================================================================
// PHP CRU 寄存器偏移 (PMU/高性能)
// ============================================================================

/// PHP CRU 时钟门控寄存器
pub const fn php_clkgate_con(x: u32) -> u32 {
    x * 0x4 + RK3588_PHP_CRU_BASE + 0x800
}

/// PHP CRU 软件复位寄存器
pub const fn php_softrst_con(x: u32) -> u32 {
    x * 0x4 + RK3588_PHP_CRU_BASE + 0xa00
}

// ============================================================================
// PMU CRU 寄存器偏移
// ============================================================================

/// PMU PLL 配置寄存器
pub const fn pmu_pll_con(x: u32) -> u32 {
    x * 0x4 + RK3588_PHP_CRU_BASE
}

/// PMU 时钟选择寄存器
pub const fn pmu_clksel_con(x: u32) -> u32 {
    x * 0x4 + RK3588_PMU_CRU_BASE + 0x300
}

/// PMU 时钟门控寄存器
pub const fn pmu_clkgate_con(x: u32) -> u32 {
    x * 0x4 + RK3588_PMU_CRU_BASE + 0x800
}

/// PMU 软件复位寄存器
pub const fn pmu_softrst_con(x: u32) -> u32 {
    x * 0x4 + RK3588_PMU_CRU_BASE + 0xa00
}

// ============================================================================
// BIGCORE0 CRU 寄存器偏移
// ============================================================================

/// B0 PLL 配置寄存器
pub const fn b0_pll_con(x: u32) -> u32 {
    x * 0x4 + RK3588_BIGCORE0_CRU_BASE
}

/// B0 PLL 模式控制寄存器
pub const RK3588_B0_PLL_MODE_CON: u32 = RK3588_BIGCORE0_CRU_BASE + 0x280;

/// BIGCORE0 时钟选择寄存器
pub const fn bigcore0_clksel_con(x: u32) -> u32 {
    x * 0x4 + RK3588_BIGCORE0_CRU_BASE + 0x300
}

/// BIGCORE0 时钟门控寄存器
pub const fn bigcore0_clkgate_con(x: u32) -> u32 {
    x * 0x4 + RK3588_BIGCORE0_CRU_BASE + 0x800
}

/// BIGCORE0 软件复位寄存器
pub const fn bigcore0_softrst_con(x: u32) -> u32 {
    x * 0x4 + RK3588_BIGCORE0_CRU_BASE + 0xa00
}

// ============================================================================
// BIGCORE1 CRU 寄存器偏移
// ============================================================================

/// B1 PLL 配置寄存器
pub const fn b1_pll_con(x: u32) -> u32 {
    x * 0x4 + RK3588_BIGCORE1_CRU_BASE
}

/// B1 PLL 模式控制寄存器
pub const RK3588_B1_PLL_MODE_CON: u32 = RK3588_BIGCORE1_CRU_BASE + 0x280;

/// BIGCORE1 时钟选择寄存器
pub const fn bigcore1_clksel_con(x: u32) -> u32 {
    x * 0x4 + RK3588_BIGCORE1_CRU_BASE + 0x300
}

/// BIGCORE1 时钟门控寄存器
pub const fn bigcore1_clkgate_con(x: u32) -> u32 {
    x * 0x4 + RK3588_BIGCORE1_CRU_BASE + 0x800
}

/// BIGCORE1 软件复位寄存器
pub const fn bigcore1_softrst_con(x: u32) -> u32 {
    x * 0x4 + RK3588_BIGCORE1_CRU_BASE + 0xa00
}

// ============================================================================
// DSU CRU 寄存器偏移
// ============================================================================

/// LPLL 配置寄存器
pub const fn lpll_con(x: u32) -> u32 {
    x * 0x4 + RK3588_DSU_CRU_BASE
}

/// LPLL 模式控制寄存器
pub const RK3588_LPLL_MODE_CON: u32 = RK3588_DSU_CRU_BASE + 0x280;

/// DSU 时钟选择寄存器
pub const fn dsu_clksel_con(x: u32) -> u32 {
    x * 0x4 + RK3588_DSU_CRU_BASE + 0x300
}

/// DSU 时钟门控寄存器
pub const fn dsu_clkgate_con(x: u32) -> u32 {
    x * 0x4 + RK3588_DSU_CRU_BASE + 0x800
}

/// DSU 软件复位寄存器
pub const fn dsu_softrst_con(x: u32) -> u32 {
    x * 0x4 + RK3588_DSU_CRU_BASE + 0xa00
}

// ============================================================================
// 位域偏移和掩码定义
// ============================================================================

/// 生成位掩码的辅助函数
const fn bit_mask(width: u32) -> u32 {
    (1 << width) - 1
}

/// 生成移位后的掩码
const fn shift_mask(shift: u32, width: u32) -> u32 {
    bit_mask(width) << shift
}

// CRU_CLK_SEL8_CON - 顶层时钟配置
pub mod clk_sel8 {
    use super::*;
    pub const ACLK_LOW_TOP_ROOT_SRC_SEL_SHIFT: u32 = 14;
    pub const ACLK_LOW_TOP_ROOT_SRC_SEL_MASK: u32 = shift_mask(14, 1);
    pub const ACLK_LOW_TOP_ROOT_SRC_SEL_GPLL: u32 = 0;
    pub const ACLK_LOW_TOP_ROOT_SRC_SEL_CPLL: u32 = 1;

    pub const ACLK_LOW_TOP_ROOT_DIV_SHIFT: u32 = 9;
    pub const ACLK_LOW_TOP_ROOT_DIV_MASK: u32 = shift_mask(9, 5);

    pub const PCLK_TOP_ROOT_SEL_SHIFT: u32 = 7;
    pub const PCLK_TOP_ROOT_SEL_MASK: u32 = shift_mask(7, 2);
    pub const PCLK_TOP_ROOT_SEL_100M: u32 = 0;
    pub const PCLK_TOP_ROOT_SEL_50M: u32 = 1;
    pub const PCLK_TOP_ROOT_SEL_24M: u32 = 2;

    pub const ACLK_TOP_ROOT_SRC_SEL_SHIFT: u32 = 5;
    pub const ACLK_TOP_ROOT_SRC_SEL_MASK: u32 = shift_mask(5, 2);
    pub const ACLK_TOP_ROOT_SRC_SEL_GPLL: u32 = 0;
    pub const ACLK_TOP_ROOT_SRC_SEL_CPLL: u32 = 1;
    pub const ACLK_TOP_ROOT_SRC_SEL_AUPLL: u32 = 2;

    pub const ACLK_TOP_ROOT_DIV_SHIFT: u32 = 0;
    pub const ACLK_TOP_ROOT_DIV_MASK: u32 = shift_mask(0, 5);
}

// CRU_CLK_SEL9_CON - 顶层时钟分频
pub mod clk_sel9 {
    use super::*;
    pub const ACLK_TOP_S400_SEL_SHIFT: u32 = 8;
    pub const ACLK_TOP_S400_SEL_MASK: u32 = shift_mask(8, 2);
    pub const ACLK_TOP_S400_SEL_400M: u32 = 0;
    pub const ACLK_TOP_S400_SEL_200M: u32 = 1;

    pub const ACLK_TOP_S200_SEL_SHIFT: u32 = 6;
    pub const ACLK_TOP_S200_SEL_MASK: u32 = shift_mask(6, 2);
    pub const ACLK_TOP_S200_SEL_200M: u32 = 0;
    pub const ACLK_TOP_S200_SEL_100M: u32 = 1;
}

// CRU_CLK_SEL38_CON - I2C 和总线时钟
pub mod clk_sel38 {
    use super::*;
    pub const CLK_I2C8_SEL_SHIFT: u32 = 13;
    pub const CLK_I2C8_SEL_MASK: u32 = 1 << 13;

    pub const CLK_I2C7_SEL_SHIFT: u32 = 12;
    pub const CLK_I2C7_SEL_MASK: u32 = 1 << 12;

    pub const CLK_I2C6_SEL_SHIFT: u32 = 11;
    pub const CLK_I2C6_SEL_MASK: u32 = 1 << 11;

    pub const CLK_I2C5_SEL_SHIFT: u32 = 10;
    pub const CLK_I2C5_SEL_MASK: u32 = 1 << 10;

    pub const CLK_I2C4_SEL_SHIFT: u32 = 9;
    pub const CLK_I2C4_SEL_MASK: u32 = 1 << 9;

    pub const CLK_I2C3_SEL_SHIFT: u32 = 8;
    pub const CLK_I2C3_SEL_MASK: u32 = 1 << 8;

    pub const CLK_I2C2_SEL_SHIFT: u32 = 7;
    pub const CLK_I2C2_SEL_MASK: u32 = 1 << 7;

    pub const CLK_I2C1_SEL_SHIFT: u32 = 6;
    pub const CLK_I2C1_SEL_MASK: u32 = 1 << 6;

    pub const ACLK_BUS_ROOT_SEL_SHIFT: u32 = 5;
    pub const ACLK_BUS_ROOT_SEL_MASK: u32 = shift_mask(5, 2);
    pub const ACLK_BUS_ROOT_SEL_GPLL: u32 = 0;
    pub const ACLK_BUS_ROOT_SEL_CPLL: u32 = 1;

    pub const ACLK_BUS_ROOT_DIV_SHIFT: u32 = 0;
    pub const ACLK_BUS_ROOT_DIV_MASK: u32 = shift_mask(0, 5);
}

// CRU_CLK_SEL40_CON - SARADC 时钟
pub mod clk_sel40 {
    use super::*;
    pub const CLK_SARADC_SEL_SHIFT: u32 = 14;
    pub const CLK_SARADC_SEL_MASK: u32 = 1 << 14;
    pub const CLK_SARADC_SEL_GPLL: u32 = 0;
    pub const CLK_SARADC_SEL_24M: u32 = 1;

    pub const CLK_SARADC_DIV_SHIFT: u32 = 6;
    pub const CLK_SARADC_DIV_MASK: u32 = shift_mask(6, 8);
}

// CRU_CLK_SEL41_CON - UART 和 TSADC 时钟
pub mod clk_sel41 {
    use super::*;
    pub const CLK_UART_SRC_SEL_SHIFT: u32 = 14;
    pub const CLK_UART_SRC_SEL_MASK: u32 = 1 << 14;
    pub const CLK_UART_SRC_SEL_GPLL: u32 = 0;
    pub const CLK_UART_SRC_SEL_CPLL: u32 = 1;

    pub const CLK_UART_SRC_DIV_SHIFT: u32 = 9;
    pub const CLK_UART_SRC_DIV_MASK: u32 = shift_mask(9, 5);

    pub const CLK_TSADC_SEL_SHIFT: u32 = 8;
    pub const CLK_TSADC_SEL_MASK: u32 = 1 << 8;
    pub const CLK_TSADC_SEL_GPLL: u32 = 0;
    pub const CLK_TSADC_SEL_24M: u32 = 1;

    pub const CLK_TSADC_DIV_SHIFT: u32 = 0;
    pub const CLK_TSADC_DIV_MASK: u32 = shift_mask(0, 8);
}

// CRU_CLK_SEL42_CON - UART 小数分频
pub mod clk_sel42 {
    pub const CLK_UART_FRAC_NUMERATOR_SHIFT: u32 = 16;
    pub const CLK_UART_FRAC_NUMERATOR_MASK: u32 = 0xffff << 16;

    pub const CLK_UART_FRAC_DENOMINATOR_SHIFT: u32 = 0;
    pub const CLK_UART_FRAC_DENOMINATOR_MASK: u32 = 0xffff;
}

// CRU_CLK_SEL43_CON - UART 时钟选择
pub mod clk_sel43 {
    use super::*;
    pub const CLK_UART_SEL_SHIFT: u32 = 0;
    pub const CLK_UART_SEL_MASK: u32 = shift_mask(0, 2);
    pub const CLK_UART_SEL_SRC: u32 = 0;
    pub const CLK_UART_SEL_FRAC: u32 = 1;
    pub const CLK_UART_SEL_XIN24M: u32 = 2;
}

// CRU_CLK_SEL59_CON - SPI 和 PWM 时钟
pub mod clk_sel59 {
    use super::*;
    pub const CLK_PWM2_SEL_SHIFT: u32 = 14;
    pub const CLK_PWM2_SEL_MASK: u32 = shift_mask(14, 2);

    pub const CLK_PWM1_SEL_SHIFT: u32 = 12;
    pub const CLK_PWM1_SEL_MASK: u32 = shift_mask(12, 2);

    pub const CLK_SPI4_SEL_SHIFT: u32 = 10;
    pub const CLK_SPI4_SEL_MASK: u32 = shift_mask(10, 2);

    pub const CLK_SPI3_SEL_SHIFT: u32 = 8;
    pub const CLK_SPI3_SEL_MASK: u32 = shift_mask(8, 2);

    pub const CLK_SPI2_SEL_SHIFT: u32 = 6;
    pub const CLK_SPI2_SEL_MASK: u32 = shift_mask(6, 2);

    pub const CLK_SPI1_SEL_SHIFT: u32 = 4;
    pub const CLK_SPI1_SEL_MASK: u32 = shift_mask(4, 2);

    pub const CLK_SPI0_SEL_SHIFT: u32 = 2;
    pub const CLK_SPI0_SEL_MASK: u32 = shift_mask(2, 2);

    pub const CLK_SPI_SEL_200M: u32 = 0;
    pub const CLK_SPI_SEL_150M: u32 = 1;
    pub const CLK_SPI_SEL_24M: u32 = 2;

    pub const CLK_PWM_SEL_100M: u32 = 0;
    pub const CLK_PWM_SEL_50M: u32 = 1;
    pub const CLK_PWM_SEL_24M: u32 = 2;
}

// CRU_CLK_SEL60_CON - PWM3 时钟
pub mod clk_sel60 {
    use super::*;
    pub const CLK_PWM3_SEL_SHIFT: u32 = 0;
    pub const CLK_PWM3_SEL_MASK: u32 = shift_mask(0, 2);

    pub const CLK_PWM_SEL_100M: u32 = 0;
    pub const CLK_PWM_SEL_50M: u32 = 1;
    pub const CLK_PWM_SEL_24M: u32 = 2;
}

// CRU_CLK_SEL62_CON - DECOM 显示时钟
pub mod clk_sel62 {
    use super::*;
    pub const DCLK_DECOM_SEL_SHIFT: u32 = 5;
    pub const DCLK_DECOM_SEL_MASK: u32 = 1 << 5;
    pub const DCLK_DECOM_SEL_GPLL: u32 = 0;
    pub const DCLK_DECOM_SEL_SPLL: u32 = 1;

    pub const DCLK_DECOM_DIV_SHIFT: u32 = 0;
    pub const DCLK_DECOM_DIV_MASK: u32 = shift_mask(0, 5);
}

// CRU_CLK_SEL77_CON - EMMC 时钟
pub mod clk_sel77 {
    use super::*;
    pub const CCLK_EMMC_SEL_SHIFT: u32 = 14;
    pub const CCLK_EMMC_SEL_MASK: u32 = shift_mask(14, 2);
    pub const CCLK_EMMC_SEL_GPLL: u32 = 0;
    pub const CCLK_EMMC_SEL_CPLL: u32 = 1;
    pub const CCLK_EMMC_SEL_24M: u32 = 2;

    pub const CCLK_EMMC_DIV_SHIFT: u32 = 8;
    pub const CCLK_EMMC_DIV_MASK: u32 = shift_mask(8, 6);
}

// CRU_CLK_SEL78_CON - SFC 和 EMMC 总线时钟
pub mod clk_sel78 {
    use super::*;
    pub const SCLK_SFC_SEL_SHIFT: u32 = 12;
    pub const SCLK_SFC_SEL_MASK: u32 = shift_mask(12, 2);
    pub const SCLK_SFC_SEL_GPLL: u32 = 0;
    pub const SCLK_SFC_SEL_CPLL: u32 = 1;
    pub const SCLK_SFC_SEL_24M: u32 = 2;

    pub const SCLK_SFC_DIV_SHIFT: u32 = 6;
    pub const SCLK_SFC_DIV_MASK: u32 = shift_mask(6, 6);

    pub const BCLK_EMMC_SEL_SHIFT: u32 = 5;
    pub const BCLK_EMMC_SEL_MASK: u32 = 1 << 5;
    pub const BCLK_EMMC_SEL_GPLL: u32 = 0;
    pub const BCLK_EMMC_SEL_CPLL: u32 = 1;

    pub const BCLK_EMMC_DIV_SHIFT: u32 = 0;
    pub const BCLK_EMMC_DIV_MASK: u32 = shift_mask(0, 5);
}

// CRU_CLK_SEL81_CON - GMAC PTP 时钟
pub mod clk_sel81 {
    use super::*;
    pub const CLK_GMAC1_PTP_SEL_SHIFT: u32 = 13;
    pub const CLK_GMAC1_PTP_SEL_MASK: u32 = 1 << 13;
    pub const CLK_GMAC1_PTP_SEL_CPLL: u32 = 0;

    pub const CLK_GMAC1_PTP_DIV_SHIFT: u32 = 7;
    pub const CLK_GMAC1_PTP_DIV_MASK: u32 = shift_mask(7, 6);

    pub const CLK_GMAC0_PTP_SEL_SHIFT: u32 = 6;
    pub const CLK_GMAC0_PTP_SEL_MASK: u32 = 1 << 6;
    pub const CLK_GMAC0_PTP_SEL_CPLL: u32 = 0;

    pub const CLK_GMAC0_PTP_DIV_SHIFT: u32 = 0;
    pub const CLK_GMAC0_PTP_DIV_MASK: u32 = shift_mask(0, 6);
}

// CRU_CLK_SEL83_CON - GMAC 125M 时钟
pub mod clk_sel83 {
    use super::*;
    pub const CLK_GMAC_125M_SEL_SHIFT: u32 = 15;
    pub const CLK_GMAC_125M_SEL_MASK: u32 = 1 << 15;
    pub const CLK_GMAC_125M_SEL_GPLL: u32 = 0;
    pub const CLK_GMAC_125M_SEL_CPLL: u32 = 1;

    pub const CLK_GMAC_125M_DIV_SHIFT: u32 = 8;
    pub const CLK_GMAC_125M_DIV_MASK: u32 = shift_mask(8, 7);
}

// CRU_CLK_SEL84_CON - GMAC 50M 时钟
pub mod clk_sel84 {
    use super::*;

    // CLK_UTMI_OTG2 (bits 12:15)
    pub const CLK_UTMI_OTG2_SEL_SHIFT: u32 = 12;
    pub const CLK_UTMI_OTG2_SEL_MASK: u32 = shift_mask(12, 2);
    pub const CLK_UTMI_OTG2_SEL_150M: u32 = 0;
    pub const CLK_UTMI_OTG2_SEL_50M: u32 = 1;
    pub const CLK_UTMI_OTG2_SEL_24M: u32 = 2;

    pub const CLK_UTMI_OTG2_DIV_SHIFT: u32 = 8;
    pub const CLK_UTMI_OTG2_DIV_MASK: u32 = shift_mask(8, 4);

    // CLK_GMAC_50M (bits 0:7)
    pub const CLK_GMAC_50M_SEL_SHIFT: u32 = 7;
    pub const CLK_GMAC_50M_SEL_MASK: u32 = 1 << 7;
    pub const CLK_GMAC_50M_SEL_GPLL: u32 = 0;
    pub const CLK_GMAC_50M_SEL_CPLL: u32 = 1;

    pub const CLK_GMAC_50M_DIV_SHIFT: u32 = 0;
    pub const CLK_GMAC_50M_DIV_MASK: u32 = shift_mask(0, 7);
}

// CRU_CLK_SEL96_CON - USB 根时钟
pub mod clk_sel96 {
    use super::*;

    // ACLK_USB_ROOT (bits 0:5)
    pub const ACLK_USB_ROOT_SEL_SHIFT: u32 = 5;
    pub const ACLK_USB_ROOT_SEL_MASK: u32 = 1 << 5;
    pub const ACLK_USB_ROOT_SEL_GPLL: u32 = 0;
    pub const ACLK_USB_ROOT_SEL_CPLL: u32 = 1;

    pub const ACLK_USB_ROOT_DIV_SHIFT: u32 = 0;
    pub const ACLK_USB_ROOT_DIV_MASK: u32 = shift_mask(0, 5);

    // HCLK_USB_ROOT (bits 6:7, COMPOSITE_NODIV - 只有 sel 无 div)
    pub const HCLK_USB_ROOT_SEL_SHIFT: u32 = 6;
    pub const HCLK_USB_ROOT_SEL_MASK: u32 = shift_mask(6, 2);
    pub const HCLK_USB_ROOT_SEL_150M: u32 = 0;
    pub const HCLK_USB_ROOT_SEL_100M: u32 = 1;
    pub const HCLK_USB_ROOT_SEL_50M: u32 = 2;
    pub const HCLK_USB_ROOT_SEL_24M: u32 = 3;
}

// CRU_CLK_SEL110_CON - VOP 显示时钟
pub mod clk_sel110 {
    use super::*;
    pub const HCLK_VOP_ROOT_SEL_SHIFT: u32 = 10;
    pub const HCLK_VOP_ROOT_SEL_MASK: u32 = shift_mask(10, 2);
    pub const HCLK_VOP_ROOT_SEL_200M: u32 = 0;
    pub const HCLK_VOP_ROOT_SEL_100M: u32 = 1;
    pub const HCLK_VOP_ROOT_SEL_50M: u32 = 2;
    pub const HCLK_VOP_ROOT_SEL_24M: u32 = 3;

    pub const ACLK_VOP_LOW_ROOT_SEL_SHIFT: u32 = 8;
    pub const ACLK_VOP_LOW_ROOT_SEL_MASK: u32 = shift_mask(8, 2);
    pub const ACLK_VOP_LOW_ROOT_SEL_400M: u32 = 0;
    pub const ACLK_VOP_LOW_ROOT_SEL_200M: u32 = 1;
    pub const ACLK_VOP_LOW_ROOT_SEL_100M: u32 = 2;
    pub const ACLK_VOP_LOW_ROOT_SEL_24M: u32 = 3;

    pub const ACLK_VOP_ROOT_SEL_SHIFT: u32 = 5;
    pub const ACLK_VOP_ROOT_SEL_MASK: u32 = shift_mask(5, 3);
    pub const ACLK_VOP_ROOT_SEL_GPLL: u32 = 0;
    pub const ACLK_VOP_ROOT_SEL_CPLL: u32 = 1;
    pub const ACLK_VOP_ROOT_SEL_AUPLL: u32 = 2;
    pub const ACLK_VOP_ROOT_SEL_NPLL: u32 = 3;
    pub const ACLK_VOP_ROOT_SEL_SPLL: u32 = 4;

    pub const ACLK_VOP_ROOT_DIV_SHIFT: u32 = 0;
    pub const ACLK_VOP_ROOT_DIV_MASK: u32 = shift_mask(0, 5);
}

// CRU_CLK_SEL111_CON - VOP 显示时钟源
pub mod clk_sel111 {
    use super::*;
    pub const DCLK1_VOP_SRC_SEL_SHIFT: u32 = 14;
    pub const DCLK1_VOP_SRC_SEL_MASK: u32 = shift_mask(14, 2);

    pub const DCLK1_VOP_SRC_DIV_SHIFT: u32 = 9;
    pub const DCLK1_VOP_SRC_DIV_MASK: u32 = shift_mask(9, 5);

    pub const DCLK0_VOP_SRC_SEL_SHIFT: u32 = 7;
    pub const DCLK0_VOP_SRC_SEL_MASK: u32 = shift_mask(7, 2);
    pub const DCLK_VOP_SRC_SEL_GPLL: u32 = 0;
    pub const DCLK_VOP_SRC_SEL_CPLL: u32 = 1;
    pub const DCLK_VOP_SRC_SEL_V0PLL: u32 = 2;
    pub const DCLK_VOP_SRC_SEL_AUPLL: u32 = 3;

    pub const DCLK0_VOP_SRC_DIV_SHIFT: u32 = 0;
    pub const DCLK0_VOP_SRC_DIV_MASK: u32 = shift_mask(0, 7);
}

// CRU_CLK_SEL112_CON - VOP 显示时钟输出
pub mod clk_sel112 {
    use super::*;

    pub const DCLK2_VOP_SEL_SHIFT: u32 = 11;
    pub const DCLK2_VOP_SEL_MASK: u32 = shift_mask(11, 2);

    pub const DCLK1_VOP_SEL_SHIFT: u32 = 9;
    pub const DCLK1_VOP_SEL_MASK: u32 = shift_mask(9, 2);

    pub const DCLK0_VOP_SEL_SHIFT: u32 = 7;
    pub const DCLK0_VOP_SEL_MASK: u32 = shift_mask(7, 2);

    pub const DCLK2_VOP_SRC_SEL_SHIFT: u32 = 5;
    pub const DCLK2_VOP_SRC_SEL_MASK: u32 = shift_mask(5, 2);

    pub const DCLK2_VOP_SRC_DIV_SHIFT: u32 = 0;
    pub const DCLK2_VOP_SRC_DIV_MASK: u32 = shift_mask(0, 5);
}

// CRU_CLK_SEL113_CON - VOP DCLK3 时钟
pub mod clk_sel113 {
    use super::*;
    pub const DCLK3_VOP_SRC_SEL_SHIFT: u32 = 7;
    pub const DCLK3_VOP_SRC_SEL_MASK: u32 = shift_mask(7, 2);

    pub const DCLK3_VOP_SRC_DIV_SHIFT: u32 = 0;
    pub const DCLK3_VOP_SRC_DIV_MASK: u32 = shift_mask(0, 7);
}

// CRU_CLK_SEL117_CON - 辅助 16MHz 时钟
pub mod clk_sel117 {
    use super::*;
    pub const CLK_AUX16MHZ_1_DIV_SHIFT: u32 = 8;
    pub const CLK_AUX16MHZ_1_DIV_MASK: u32 = shift_mask(8, 8);

    pub const CLK_AUX16MHZ_0_DIV_SHIFT: u32 = 0;
    pub const CLK_AUX16MHZ_0_DIV_MASK: u32 = shift_mask(0, 8);
}

// CRU_CLK_SEL165_CON - 中心根时钟
pub mod clk_sel165 {
    use super::*;
    pub const PCLK_CENTER_ROOT_SEL_SHIFT: u32 = 6;
    pub const PCLK_CENTER_ROOT_SEL_MASK: u32 = shift_mask(6, 2);
    pub const PCLK_CENTER_ROOT_SEL_200M: u32 = 0;
    pub const PCLK_CENTER_ROOT_SEL_100M: u32 = 1;
    pub const PCLK_CENTER_ROOT_SEL_50M: u32 = 2;
    pub const PCLK_CENTER_ROOT_SEL_24M: u32 = 3;

    pub const HCLK_CENTER_ROOT_SEL_SHIFT: u32 = 4;
    pub const HCLK_CENTER_ROOT_SEL_MASK: u32 = shift_mask(4, 2);
    pub const HCLK_CENTER_ROOT_SEL_400M: u32 = 0;
    pub const HCLK_CENTER_ROOT_SEL_200M: u32 = 1;
    pub const HCLK_CENTER_ROOT_SEL_100M: u32 = 2;
    pub const HCLK_CENTER_ROOT_SEL_24M: u32 = 3;

    pub const ACLK_CENTER_LOW_ROOT_SEL_SHIFT: u32 = 2;
    pub const ACLK_CENTER_LOW_ROOT_SEL_MASK: u32 = shift_mask(2, 2);
    pub const ACLK_CENTER_LOW_ROOT_SEL_500M: u32 = 0;
    pub const ACLK_CENTER_LOW_ROOT_SEL_250M: u32 = 1;
    pub const ACLK_CENTER_LOW_ROOT_SEL_100M: u32 = 2;
    pub const ACLK_CENTER_LOW_ROOT_SEL_24M: u32 = 3;

    pub const ACLK_CENTER_ROOT_SEL_SHIFT: u32 = 0;
    pub const ACLK_CENTER_ROOT_SEL_MASK: u32 = shift_mask(0, 2);
    pub const ACLK_CENTER_ROOT_SEL_700M: u32 = 0;
    pub const ACLK_CENTER_ROOT_SEL_400M: u32 = 1;
    pub const ACLK_CENTER_ROOT_SEL_200M: u32 = 2;
    pub const ACLK_CENTER_ROOT_SEL_24M: u32 = 3;
}

// CRU_CLK_SEL172_CON - SDIO 时钟
pub mod clk_sel172 {
    use super::*;
    pub const CCLK_SDIO_SRC_SEL_SHIFT: u32 = 8;
    pub const CCLK_SDIO_SRC_SEL_MASK: u32 = shift_mask(8, 2);
    pub const CCLK_SDIO_SRC_SEL_GPLL: u32 = 0;
    pub const CCLK_SDIO_SRC_SEL_CPLL: u32 = 1;
    pub const CCLK_SDIO_SRC_SEL_24M: u32 = 2;

    pub const CCLK_SDIO_SRC_DIV_SHIFT: u32 = 2;
    pub const CCLK_SDIO_SRC_DIV_MASK: u32 = shift_mask(2, 6);
}

// CRU_CLK_SEL176_CON - PCIe PHY PLL 分频
pub mod clk_sel176 {
    use super::*;
    pub const CLK_PCIE_PHY1_PLL_DIV_SHIFT: u32 = 6;
    pub const CLK_PCIE_PHY1_PLL_DIV_MASK: u32 = shift_mask(6, 6);

    pub const CLK_PCIE_PHY0_PLL_DIV_SHIFT: u32 = 0;
    pub const CLK_PCIE_PHY0_PLL_DIV_MASK: u32 = shift_mask(0, 6);
}

// CRU_CLK_SEL177_CON - PCIe PHY 参考时钟
pub mod clk_sel177 {
    use super::*;
    pub const CLK_PCIE_PHY2_REF_SEL_SHIFT: u32 = 8;
    pub const CLK_PCIE_PHY2_REF_SEL_MASK: u32 = 1 << 8;

    pub const CLK_PCIE_PHY1_REF_SEL_SHIFT: u32 = 7;
    pub const CLK_PCIE_PHY1_REF_SEL_MASK: u32 = 1 << 7;

    pub const CLK_PCIE_PHY0_REF_SEL_SHIFT: u32 = 6;
    pub const CLK_PCIE_PHY0_REF_SEL_MASK: u32 = 1 << 6;

    pub const CLK_PCIE_PHY_REF_SEL_24M: u32 = 0;
    pub const CLK_PCIE_PHY_REF_SEL_PPLL: u32 = 1;

    pub const CLK_PCIE_PHY2_PLL_DIV_SHIFT: u32 = 0;
    pub const CLK_PCIE_PHY2_PLL_DIV_MASK: u32 = shift_mask(0, 6);
}

// PMUCRU_CLK_SEL2_CON - PMU PWM1 时钟
pub mod pmu_clk_sel2 {
    use super::*;
    pub const CLK_PMU1PWM_SEL_SHIFT: u32 = 9;
    pub const CLK_PMU1PWM_SEL_MASK: u32 = shift_mask(9, 2);

    pub const CLK_PWM_SEL_100M: u32 = 0;
    pub const CLK_PWM_SEL_50M: u32 = 1;
    pub const CLK_PWM_SEL_24M: u32 = 2;
}

// PMUCRU_CLK_SEL3_CON - I2C0 时钟
pub mod pmu_clk_sel3 {
    pub const CLK_I2C0_SEL_SHIFT: u32 = 6;
    pub const CLK_I2C0_SEL_MASK: u32 = 1 << 6;

    pub const CLK_I2C_SEL_200M: u32 = 0;
    pub const CLK_I2C_SEL_100M: u32 = 1;
}

// ============================================================================
// PLL 寄存器位域定义 (RK3588)
// ============================================================================

/// PLL 模式定义
pub mod pll_mode {
    /// 慢速模式 - 直接输出 OSC 时钟
    pub const PLL_MODE_SLOW: u32 = 0;
    /// 正常模式 - PLL 正常工作
    pub const PLL_MODE_NORMAL: u32 = 1;
    /// 深度模式 - 低功耗模式
    pub const PLL_MODE_DEEP: u32 = 2;
}

/// RK3588 PLL 配置寄存器 0 (PLLCON0)
pub mod pllcon0 {
    /// M 分频系数 (反馈分频)
    pub const M_SHIFT: u32 = 0;
    pub const M_MASK: u32 = 0x3ff << M_SHIFT; // 10 bits
}

/// RK3588 PLL 配置寄存器 1 (PLLCON1)
pub mod pllcon1 {
    /// P 分频系数 (预分频)
    pub const P_SHIFT: u32 = 0;
    pub const P_MASK: u32 = 0x3f << P_SHIFT; // 6 bits

    /// S 分频系数 (后分频)
    pub const S_SHIFT: u32 = 6;
    pub const S_MASK: u32 = 0x7 << S_SHIFT; // 3 bits

    /// PLL 掉电使能
    pub const PWRDOWN: u32 = 1 << 13;
}

/// RK3588 PLL 配置寄存器 2 (PLLCON2)
pub mod pllcon2 {
    /// K 小数分频系数
    pub const K_SHIFT: u32 = 0;
    pub const K_MASK: u32 = 0xffff << K_SHIFT; // 16 bits
}

/// RK3588 PLL 配置寄存器 6 (PLLCON6)
pub mod pllcon6 {
    /// PLL 锁定状态
    pub const LOCK_STATUS: u32 = 1 << 15;
}
