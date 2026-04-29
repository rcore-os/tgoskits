//! RK3588 时钟 ID 定义
//!
//! 参考 u-boot: include/dt-bindings/clock/rk3588-cru.h
//!
//! 所有 clkid 值与 Linux/u-boot 定义严格一致，不可随意修改

#![allow(dead_code)]

use crate::clock::ClkId;

// =============================================================================
// 宏定义：批量定义时钟常量
// =============================================================================

/// 批量定义时钟 ID 常量
///
/// # 语法
/// ```ignore
/// clk_id_group!(
///     PLL_B0PLL = 1,
///     PLL_B1PLL = 2,
/// );
/// ```
macro_rules! clk_id_group {
    ($($name:ident = $value:expr),* $(,)?) => {
        $(
            pub const $name: ClkId = ClkId::new($value);
        )*
    };
}

// =============================================================================
// PLL 时钟 ID
// =============================================================================

clk_id_group!(
    PLL_B0PLL = 1,
    PLL_B1PLL = 2,
    PLL_LPLL = 3,
    PLL_V0PLL = 4,
    PLL_AUPLL = 5,
    PLL_CPLL = 6,
    PLL_GPLL = 7,
    PLL_NPLL = 8,
    PLL_PPLL = 9,
);

// =============================================================================
// I2C 时钟 ID
// =============================================================================

clk_id_group!(
    PCLK_I2C1 = 133,
    PCLK_I2C2 = 134,
    PCLK_I2C3 = 135,
    PCLK_I2C4 = 136,
    PCLK_I2C5 = 137,
    PCLK_I2C6 = 138,
    PCLK_I2C7 = 139,
    PCLK_I2C8 = 140,
);

clk_id_group!(
    CLK_I2C1 = 141,
    CLK_I2C2 = 142,
    CLK_I2C3 = 143,
    CLK_I2C4 = 144,
    CLK_I2C5 = 145,
    CLK_I2C6 = 146,
    CLK_I2C7 = 147,
    CLK_I2C8 = 148,
);

// PMU I2C (I2C0) 在 PMU CRU 中
clk_id_group!(PCLK_I2C0 = 646, CLK_I2C0 = 647,);

// =============================================================================
// UART 时钟 ID
// =============================================================================

clk_id_group!(
    PCLK_UART1 = 171,
    PCLK_UART2 = 172,
    PCLK_UART3 = 173,
    PCLK_UART4 = 174,
    PCLK_UART5 = 175,
    PCLK_UART6 = 176,
    PCLK_UART7 = 177,
    PCLK_UART8 = 178,
    PCLK_UART9 = 179,
);

clk_id_group!(
    CLK_UART1_SRC = 180,
    CLK_UART1_FRAC = 181,
    CLK_UART1 = 182,
    SCLK_UART1 = 183,
    CLK_UART2_SRC = 184,
    CLK_UART2_FRAC = 185,
    CLK_UART2 = 186,
    SCLK_UART2 = 187,
    CLK_UART3_SRC = 188,
    CLK_UART3_FRAC = 189,
    CLK_UART3 = 190,
    SCLK_UART3 = 191,
    CLK_UART4_SRC = 192,
    CLK_UART4_FRAC = 193,
    CLK_UART4 = 194,
    SCLK_UART4 = 195,
    CLK_UART5_SRC = 196,
    CLK_UART5_FRAC = 197,
    CLK_UART5 = 198,
    SCLK_UART5 = 199,
    CLK_UART6_SRC = 200,
    CLK_UART6_FRAC = 201,
    CLK_UART6 = 202,
    SCLK_UART6 = 203,
    CLK_UART7_SRC = 204,
    CLK_UART7_FRAC = 205,
    CLK_UART7 = 206,
    SCLK_UART7 = 207,
    CLK_UART8_SRC = 208,
    CLK_UART8_FRAC = 209,
    CLK_UART8 = 210,
    SCLK_UART8 = 211,
    CLK_UART9_SRC = 212,
    CLK_UART9_FRAC = 213,
    CLK_UART9 = 214,
    SCLK_UART9 = 215,
);

// PMU UART (UART0) 在 PMU CRU 中
clk_id_group!(
    CLK_UART0_SRC = 683,
    CLK_UART0_FRAC = 684,
    CLK_UART0 = 685,
    SCLK_UART0 = 686,
    PCLK_UART0 = 687,
);

// =============================================================================
// SPI 时钟 ID
// =============================================================================

clk_id_group!(
    PCLK_SPI0 = 158,
    PCLK_SPI1 = 159,
    PCLK_SPI2 = 160,
    PCLK_SPI3 = 161,
    PCLK_SPI4 = 162,
);

clk_id_group!(
    CLK_SPI0 = 163,
    CLK_SPI1 = 164,
    CLK_SPI2 = 165,
    CLK_SPI3 = 166,
    CLK_SPI4 = 167,
);

// =============================================================================
// PWM 时钟 ID
// =============================================================================

clk_id_group!(
    PCLK_PWM1 = 83,
    CLK_PWM1 = 84,
    CLK_PWM1_CAPTURE = 85,
    PCLK_PWM2 = 86,
    CLK_PWM2 = 87,
    CLK_PWM2_CAPTURE = 88,
    PCLK_PWM3 = 89,
    CLK_PWM3 = 90,
    CLK_PWM3_CAPTURE = 91,
);

// PMU PWM 在 PMU CRU 中
clk_id_group!(
    PCLK_PMU1PWM = 676,
    CLK_PMU1PWM = 677,
    CLK_PMU1PWM_CAPTURE = 678,
);

// =============================================================================
// ADC 时钟 ID
// =============================================================================

clk_id_group!(PCLK_SARADC = 156, CLK_SARADC = 157,);

clk_id_group!(PCLK_TSADC = 169, CLK_TSADC = 170,);

// =============================================================================
// 根时钟 ID
// =============================================================================

clk_id_group!(
    ACLK_BUS_ROOT = 123,
    ACLK_TOP_ROOT = 270,
    PCLK_TOP_ROOT = 271,
    ACLK_LOW_TOP_ROOT = 272,
    ACLK_CENTER_ROOT = 216,
    ACLK_CENTER_LOW_ROOT = 217,
    HCLK_CENTER_ROOT = 218,
    PCLK_CENTER_ROOT = 219,
);

// =============================================================================
// SDMMC/EMMC/SFC 时钟 ID
// =============================================================================

clk_id_group!(
    HCLK_SDIO = 409,
    CCLK_SRC_SDIO = 410,
    HCLK_EMMC = 312,
    ACLK_EMMC = 313,
    CCLK_EMMC = 314,
    BCLK_EMMC = 315,
    TMCLK_EMMC = 316,
    SCLK_SFC = 317,
    HCLK_SFC = 318,
    HCLK_SFC_XIP = 319,
);

// =============================================================================
// NPU 时钟 ID
// =============================================================================

clk_id_group!(
    ACLK_NPU1 = 290,
    HCLK_NPU1 = 291,
    ACLK_NPU2 = 292,
    HCLK_NPU2 = 293,
    HCLK_NPU_CM0_ROOT = 294,
    FCLK_NPU_CM0_CORE = 295,
    CLK_NPU_CM0_RTC = 296,
    PCLK_NPU_PVTM = 297,
    PCLK_NPU_GRF = 298,
    CLK_NPU_PVTM = 299,
    CLK_CORE_NPU_PVTM = 300,
    ACLK_NPU0 = 301,
    HCLK_NPU0 = 302,
    HCLK_NPU_ROOT = 303,
    CLK_NPU_DSU0 = 304,
    PCLK_NPU_ROOT = 305,
    PCLK_NPU_TIMER = 306,
    CLK_NPUTIMER_ROOT = 307,
    CLK_NPUTIMER0 = 308,
    CLK_NPUTIMER1 = 309,
    PCLK_NPU_WDT = 310,
    TCLK_NPU_WDT = 311,
);

// =============================================================================
// GMAC 时钟 ID
// =============================================================================

clk_id_group!(
    CLK_GMAC0_PTP_REF = 322,
    CLK_GMAC1_PTP_REF = 323,
    CLK_GMAC_125M = 324,
    CLK_GMAC_50M = 325,
);

// =============================================================================
// USB 时钟 ID
// =============================================================================

clk_id_group!(
    PCLK_PHP_USBHOST3_0 = 358,
    ACLK_USB3OTG2 = 375,
    SUSPEND_CLK_USB3OTG2 = 376,
    REF_CLK_USB3OTG2 = 377,
    CLK_UTMI_OTG2 = 378,
    CLK_PIPE_USBHOST3_0 = 385,
);

clk_id_group!(
    ACLK_USB_ROOT = 411,
    HCLK_USB_ROOT = 412,
    HCLK_HOST0 = 413,
    HCLK_HOST_ARB0 = 414,
    HCLK_HOST1 = 415,
    HCLK_HOST_ARB1 = 416,
    ACLK_USB3OTG0 = 417,
    SUSPEND_CLK_USB3OTG0 = 418,
    REF_CLK_USB3OTG0 = 419,
    ACLK_USB3OTG1 = 420,
    SUSPEND_CLK_USB3OTG1 = 421,
    REF_CLK_USB3OTG1 = 422,
    UTMI_OHCI_CLK48_HOST0 = 423,
    UTMI_OHCI_CLK48_HOST1 = 424,
);

clk_id_group!(
    ACLK_USB = 611,
    HCLK_USB = 612,
    PCLK_USBDPPHY0 = 617,
    PCLK_USBDPPHY1 = 618,
    USBDP_PHY0_IMMORTAL = 639,
    USBDP_PHY1_IMMORTAL = 640,
);

clk_id_group!(CLK_USBPHY_480M = 693,);
clk_id_group!(USBDPPHY_MIPIDCPPHY_REF = 694,);

pub const CLK_REF_USB3OTG0: ClkId = REF_CLK_USB3OTG0;
pub const CLK_SUSPEND_USB3OTG0: ClkId = SUSPEND_CLK_USB3OTG0;
pub const CLK_REF_USB3OTG1: ClkId = REF_CLK_USB3OTG1;
pub const CLK_SUSPEND_USB3OTG1: ClkId = SUSPEND_CLK_USB3OTG1;
pub const ACLK_USBHOST3_0: ClkId = ACLK_USB3OTG2;
pub const CLK_SUSPEND_USBHOST3_0: ClkId = SUSPEND_CLK_USB3OTG2;
pub const CLK_REF_USBHOST3_0: ClkId = REF_CLK_USB3OTG2;
pub const CLK_UTMI_USBHOST3_0: ClkId = CLK_UTMI_OTG2;
pub const CLK_USBHOST0: ClkId = HCLK_HOST0;
pub const CLK_USBHOST0_ARB: ClkId = HCLK_HOST_ARB0;
pub const CLK_USBHOST1: ClkId = HCLK_HOST1;
pub const CLK_USBHOST1_ARB: ClkId = HCLK_HOST_ARB1;

// =============================================================================
// 辅助函数：时钟类型判断和外设编号提取
// =============================================================================

/// 判断时钟 ID 是否为 PLL
pub fn is_pll_clk(clk_id: ClkId) -> bool {
    (PLL_B0PLL..=PLL_PPLL).contains(&clk_id)
}

/// 判断时钟 ID 是否为 I2C
pub fn is_i2c_clk(clk_id: ClkId) -> bool {
    // PMU I2C0: PCLK_I2C0, CLK_I2C0
    // I2C1-8: PCLK_I2C1-8, CLK_I2C1-8
    (PCLK_I2C0..=CLK_I2C0).contains(&clk_id) || (PCLK_I2C1..=CLK_I2C8).contains(&clk_id)
}

/// 判断时钟 ID 是否为 UART
pub fn is_uart_clk(clk_id: ClkId) -> bool {
    (CLK_UART0_SRC..= PCLK_UART0).contains(&clk_id)      // UART0 (PMU)
        || ( PCLK_UART1..= SCLK_UART9).contains(&clk_id) // UART1-9
}

/// 判断时钟 ID 是否为 SPI
pub fn is_spi_clk(clk_id: ClkId) -> bool {
    (PCLK_SPI0..=CLK_SPI4).contains(&clk_id)
}

/// 判断时钟 ID 是否为 PWM
pub fn is_pwm_clk(clk_id: ClkId) -> bool {
    // PWM1-3: 83-91, PMU1PWM: 676-678
    (PCLK_PWM1..=CLK_PWM3_CAPTURE).contains(&clk_id)
        || (PCLK_PMU1PWM..=CLK_PMU1PWM_CAPTURE).contains(&clk_id)
}

/// 判断时钟 ID 是否为 ADC
pub fn is_adc_clk(clk_id: ClkId) -> bool {
    // SARADC: 156-157, TSADC: 169-170
    (PCLK_SARADC..=CLK_SARADC).contains(&clk_id) || (PCLK_TSADC..=CLK_TSADC).contains(&clk_id)
}

/// 判断时钟 ID 是否为 MMC/EMMC/SDIO/SFC
pub fn is_mmc_clk(clk_id: ClkId) -> bool {
    // CCLK_EMMC, BCLK_EMMC, CCLK_SRC_SDIO, SCLK_SFC
    matches!(clk_id, CCLK_EMMC | BCLK_EMMC | CCLK_SRC_SDIO | SCLK_SFC)
}

/// 判断时钟 ID 是否为 NPU
pub fn is_npu_clk(clk_id: ClkId) -> bool {
    (ACLK_NPU1..=TCLK_NPU_WDT).contains(&clk_id)
}

/// 判断时钟 ID 是否为 USB
///
/// USB 时钟包括：
/// - 可配置频率时钟：ACLK_USB_ROOT, HCLK_USB_ROOT, CLK_UTMI_OTG2, PCLK_PHP_USBHOST3_0, ACLK_USB, HCLK_USB
/// - 固定频率门控时钟：其他所有 USB 时钟
pub fn is_usb_clk(clk_id: ClkId) -> bool {
    // 可配置频率时钟
    let configurable = matches!(
        clk_id,
        ACLK_USB_ROOT
            | HCLK_USB_ROOT
            | CLK_UTMI_OTG2
            | PCLK_PHP_USBHOST3_0
            | CLK_USBPHY_480M
            | ACLK_USB
            | HCLK_USB
    );

    // 固定频率门控时钟
    let gates = matches!(
        clk_id,
        ACLK_USB3OTG2
            | SUSPEND_CLK_USB3OTG2
            | REF_CLK_USB3OTG2
            | ACLK_USB3OTG0
            | SUSPEND_CLK_USB3OTG0
            | REF_CLK_USB3OTG0
            | ACLK_USB3OTG1
            | SUSPEND_CLK_USB3OTG1
            | REF_CLK_USB3OTG1
            | HCLK_HOST0
            | HCLK_HOST_ARB0
            | HCLK_HOST1
            | HCLK_HOST_ARB1
            | CLK_PIPE_USBHOST3_0
            | PCLK_USBDPPHY0
            | PCLK_USBDPPHY1
            | USBDP_PHY0_IMMORTAL
            | USBDP_PHY1_IMMORTAL
            | USBDPPHY_MIPIDCPPHY_REF
    );

    configurable || gates
}

/// 获取 I2C 编号 (0-8)
///
/// # 返回
///
/// 返回 I2C 编号，如果不是 I2C 时钟则返回 None
pub fn get_i2c_num(clk_id: ClkId) -> Option<u32> {
    // I2C0 (PMU): PCLK_I2C0, CLK_I2C0
    if (PCLK_I2C0..=CLK_I2C0).contains(&clk_id) {
        Some(0)
    }
    // I2C1-8: PCLK 和 CLK 交替出现
    else if (PCLK_I2C1..=CLK_I2C8).contains(&clk_id) {
        match clk_id {
            CLK_I2C1 | PCLK_I2C1 => Some(1),
            CLK_I2C2 | PCLK_I2C2 => Some(2),
            CLK_I2C3 | PCLK_I2C3 => Some(3),
            CLK_I2C4 | PCLK_I2C4 => Some(4),
            CLK_I2C5 | PCLK_I2C5 => Some(5),
            CLK_I2C6 | PCLK_I2C6 => Some(6),
            CLK_I2C7 | PCLK_I2C7 => Some(7),
            CLK_I2C8 | PCLK_I2C8 => Some(8),
            _ => None,
        }
    } else {
        None
    }
}

/// 获取 UART 编号 (0-9)
///
/// # 返回
///
/// 返回 UART 编号，如果不是 UART 时钟则返回 None
pub fn get_uart_num(clk_id: ClkId) -> Option<u32> {
    if (CLK_UART0_SRC..=PCLK_UART0).contains(&clk_id) {
        Some(0) // UART0 (PMU)
    } else if (PCLK_UART1..=SCLK_UART1).contains(&clk_id) {
        Some(1) // UART1
    } else if (CLK_UART2_SRC..=SCLK_UART2).contains(&clk_id) {
        Some(2) // UART2
    } else if (CLK_UART3_SRC..=SCLK_UART3).contains(&clk_id) {
        Some(3) // UART3
    } else if (CLK_UART4_SRC..=SCLK_UART4).contains(&clk_id) {
        Some(4) // UART4
    } else if (CLK_UART5_SRC..=SCLK_UART5).contains(&clk_id) {
        Some(5) // UART5
    } else if (CLK_UART6_SRC..=SCLK_UART6).contains(&clk_id) {
        Some(6) // UART6
    } else if (CLK_UART7_SRC..=SCLK_UART7).contains(&clk_id) {
        Some(7) // UART7
    } else if (CLK_UART8_SRC..=SCLK_UART8).contains(&clk_id) {
        Some(8) // UART8
    } else if (CLK_UART9_SRC..=SCLK_UART9).contains(&clk_id) {
        Some(9) // UART9
    } else {
        None
    }
}

/// 获取 SPI 编号 (0-4)
///
/// # 返回
///
/// 返回 SPI 编号，如果不是 SPI 时钟则返回 None
pub fn get_spi_num(clk_id: ClkId) -> Option<u32> {
    // SPI0-4: PCLK 和 CLK 交替出现
    if (PCLK_SPI0..=CLK_SPI4).contains(&clk_id) {
        match clk_id {
            PCLK_SPI0 | CLK_SPI0 => Some(0),
            PCLK_SPI1 | CLK_SPI1 => Some(1),
            PCLK_SPI2 | CLK_SPI2 => Some(2),
            PCLK_SPI3 | CLK_SPI3 => Some(3),
            PCLK_SPI4 | CLK_SPI4 => Some(4),
            _ => None,
        }
    } else {
        None
    }
}

// =============================================================================
// 单元测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clk_id_values_match_uboot() {
        // PLL
        assert_eq!(PLL_GPLL.value(), 7);
        assert_eq!(PLL_CPLL.value(), 6);

        // I2C
        assert_eq!(CLK_I2C0.value(), 647, "CLK_I2C0 should match u-boot (647)");
        assert_eq!(CLK_I2C1.value(), 141, "CLK_I2C1 should match u-boot (141)");
        assert_eq!(
            PCLK_I2C0.value(),
            646,
            "PCLK_I2C0 should match u-boot (646)"
        );
        assert_eq!(
            PCLK_I2C1.value(),
            133,
            "PCLK_I2C1 should match u-boot (133)"
        );

        // UART
        assert_eq!(
            CLK_UART0.value(),
            685,
            "CLK_UART0 should match u-boot (685)"
        );
        assert_eq!(
            CLK_UART1.value(),
            182,
            "CLK_UART1 should match u-boot (182)"
        );
        assert_eq!(
            PCLK_UART0.value(),
            687,
            "PCLK_UART0 should match u-boot (687)"
        );
        assert_eq!(
            PCLK_UART1.value(),
            171,
            "PCLK_UART1 should match u-boot (171)"
        );
        assert_eq!(
            SCLK_UART0.value(),
            686,
            "SCLK_UART0 should match u-boot (686)"
        );
        assert_eq!(
            SCLK_UART1.value(),
            183,
            "SCLK_UART1 should match u-boot (183)"
        );

        // SPI
        assert_eq!(CLK_SPI0.value(), 163, "CLK_SPI0 should match u-boot (163)");
        assert_eq!(
            PCLK_SPI0.value(),
            158,
            "PCLK_SPI0 should match u-boot (158)"
        );

        // PWM
        assert_eq!(CLK_PWM1.value(), 84, "CLK_PWM1 should match u-boot (84)");
        assert_eq!(
            CLK_PMU1PWM.value(),
            677,
            "CLK_PMU1PWM should match u-boot (677)"
        );

        // ADC
        assert_eq!(
            CLK_SARADC.value(),
            157,
            "CLK_SARADC should match u-boot (157)"
        );
        assert_eq!(
            CLK_TSADC.value(),
            170,
            "CLK_TSADC should match u-boot (170)"
        );
    }

    #[test]
    fn test_is_pll_clk() {
        assert!(is_pll_clk(PLL_GPLL));
        assert!(is_pll_clk(PLL_CPLL));
        assert!(!is_pll_clk(CLK_I2C1));
    }

    #[test]
    fn test_is_i2c_clk() {
        assert!(is_i2c_clk(CLK_I2C1));
        assert!(is_i2c_clk(CLK_I2C8));
        assert!(is_i2c_clk(PCLK_I2C0));
        assert!(!is_i2c_clk(CLK_UART0));
    }

    #[test]
    fn test_is_uart_clk() {
        assert!(is_uart_clk(CLK_UART0));
        assert!(is_uart_clk(CLK_UART9));
        assert!(is_uart_clk(PCLK_UART1));
        assert!(is_uart_clk(SCLK_UART4));
        assert!(!is_uart_clk(CLK_I2C1));
    }

    #[test]
    fn test_is_spi_clk() {
        assert!(is_spi_clk(CLK_SPI0));
        assert!(is_spi_clk(PCLK_SPI4));
        assert!(!is_spi_clk(CLK_UART0));
    }

    #[test]
    fn test_is_pwm_clk() {
        assert!(is_pwm_clk(CLK_PWM1));
        assert!(is_pwm_clk(CLK_PMU1PWM));
        assert!(!is_pwm_clk(CLK_UART0));
    }

    #[test]
    fn test_get_i2c_num() {
        assert_eq!(get_i2c_num(CLK_I2C1), Some(1));
        assert_eq!(get_i2c_num(CLK_I2C8), Some(8));
        assert_eq!(get_i2c_num(PCLK_I2C0), Some(0));
        assert_eq!(get_i2c_num(CLK_UART0), None);
    }

    #[test]
    fn test_get_uart_num() {
        assert_eq!(get_uart_num(CLK_UART0), Some(0));
        assert_eq!(get_uart_num(CLK_UART9), Some(9));
        assert_eq!(get_uart_num(PCLK_UART1), Some(1));
        assert_eq!(get_uart_num(SCLK_UART4), Some(4));
        assert_eq!(get_uart_num(CLK_I2C1), None);
    }

    #[test]
    fn test_get_uart_num_with_boundaries() {
        // 测试每个 UART 的边界时钟
        // UART0 (PMU)
        assert_eq!(get_uart_num(CLK_UART0_SRC), Some(0));
        assert_eq!(get_uart_num(PCLK_UART0), Some(0));

        // UART1
        assert_eq!(get_uart_num(PCLK_UART1), Some(1));
        assert_eq!(get_uart_num(SCLK_UART1), Some(1));

        // UART2
        assert_eq!(get_uart_num(CLK_UART2_SRC), Some(2));
        assert_eq!(get_uart_num(SCLK_UART2), Some(2));

        // UART3
        assert_eq!(get_uart_num(CLK_UART3_SRC), Some(3));
        assert_eq!(get_uart_num(SCLK_UART3), Some(3));

        // UART4
        assert_eq!(get_uart_num(CLK_UART4_SRC), Some(4));
        assert_eq!(get_uart_num(SCLK_UART4), Some(4));

        // UART5
        assert_eq!(get_uart_num(CLK_UART5_SRC), Some(5));
        assert_eq!(get_uart_num(SCLK_UART5), Some(5));

        // UART6
        assert_eq!(get_uart_num(CLK_UART6_SRC), Some(6));
        assert_eq!(get_uart_num(SCLK_UART6), Some(6));

        // UART7
        assert_eq!(get_uart_num(CLK_UART7_SRC), Some(7));
        assert_eq!(get_uart_num(SCLK_UART7), Some(7));

        // UART8
        assert_eq!(get_uart_num(CLK_UART8_SRC), Some(8));
        assert_eq!(get_uart_num(SCLK_UART8), Some(8));

        // UART9
        assert_eq!(get_uart_num(CLK_UART9_SRC), Some(9));
        assert_eq!(get_uart_num(SCLK_UART9), Some(9));
    }

    #[test]
    fn test_clkid_comparison() {
        // 验证 ClkId 的比较运算符正常工作
        assert!(PCLK_UART1 < SCLK_UART1);
        assert!(CLK_UART2_SRC <= SCLK_UART2);
        assert!(SCLK_UART9 > CLK_UART9_SRC);

        // 验证边界值的正确性
        assert!(SCLK_UART1.value() == 183);
        assert!(CLK_UART2_SRC.value() == 184);
        assert!(SCLK_UART9.value() == 215);
    }

    #[test]
    fn test_get_spi_num() {
        assert_eq!(get_spi_num(CLK_SPI0), Some(0));
        assert_eq!(get_spi_num(CLK_SPI4), Some(4));
        assert_eq!(get_spi_num(PCLK_SPI2), Some(2));
        assert_eq!(get_spi_num(CLK_UART0), None);
    }
}
