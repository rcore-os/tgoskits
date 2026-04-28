//! RK3588 时钟门控 (Clock Gate) 管理
//!
//! 参考 Linux: drivers/clk/rockchip/clk-rk3588.c
//!
//! 每个 clkgate_con 寄存器有 32 位，每 bit 控制一个时钟的开关
//! Rockchip 使用写掩码机制：
//! - 高 16 位：要清除的位掩码
//! - 低 16 位：要设置的值
//!
//! 使能时钟：清除对应的 bit
//! 禁止时钟：设置对应的 bit

use super::{consts::*, *};
use crate::clock::ClkId;

#[derive(Debug, Clone, Copy)]
pub enum ClkType {
    Gate,
    Composite,
}

/// 时钟门控配置
#[derive(Debug, Clone, Copy)]
pub struct ClkGate {
    /// 时钟 ID
    pub clk_id: ClkId,
    pub kind: ClkType,
    /// 寄存器索引 (0-31 用于 clkgate_con, 32+ 用于 pmu_clkgate_con)
    pub reg_idx: u32,
    /// 位偏移 (0-15)
    pub bit: u32,
}

// =============================================================================
// 宏定义：生成完整的时钟门控表
// =============================================================================

/// 生成完整时钟门控表
///
/// # 语法
/// ```ignore
/// clk_gate_table!(
///     PCLK_I2C1 => (10, 8),  // clk_id, reg_idx=10, bit=8
///     CLK_I2C1 => (11, 0),
/// );
/// ```
macro_rules! clk_gate_table {
    ($($clk_id:expr => ($reg_idx:expr, $bit:expr)),* $(,)?) => {
        #[allow(non_upper_case_globals)]
        const CLK_GATE_TABLE: &[ClkGate] = &[
            $(
                ClkGate {
                    clk_id: $clk_id,
                    kind: ClkType::Gate,
                    reg_idx: $reg_idx,
                    bit: $bit,
                }
                ),*
        ];
    };
}

macro_rules! clk_composite_table {
    ($($clk_id:expr => ($reg_idx:expr, $bit:expr)),* $(,)?) => {
        #[allow(non_upper_case_globals)]
        const CLK_COMPOSITE_TABLE: &[ClkGate] = &[
            $(
                ClkGate {
                    clk_id: $clk_id,
                    kind: ClkType::Composite,
                    reg_idx: $reg_idx,
                    bit: $bit,
                }
                ),*
        ];
    };
}

// =============================================================================
// 时钟门控表定义
// =============================================================================

clk_gate_table!(
    // ========================================================================
    // I2C 时钟门控
    // ========================================================================
    PCLK_I2C1 => (10, 8),
    CLK_I2C1 => (11, 0),
    PCLK_I2C2 => (10, 9),
    CLK_I2C2 => (11, 1),
    PCLK_I2C3 => (10, 10),
    CLK_I2C3 => (11, 2),
    PCLK_I2C4 => (10, 11),
    CLK_I2C4 => (11, 3),
    PCLK_I2C5 => (10, 12),
    CLK_I2C5 => (11, 4),
    PCLK_I2C6 => (10, 13),
    CLK_I2C6 => (11, 5),
    PCLK_I2C7 => (10, 14),
    CLK_I2C7 => (11, 6),
    PCLK_I2C8 => (10, 15),
    CLK_I2C8 => (11, 7),
    // I2C0 (PMU)
    PCLK_I2C0 => (0x32 + 2, 1),
    CLK_I2C0 => (0x32 + 2, 2),
    // ========================================================================
    // SPI 时钟门控
    // ========================================================================
    PCLK_SPI0 => (14, 6),
    CLK_SPI0 => (14, 11),
    PCLK_SPI1 => (14, 7),
    CLK_SPI1 => (14, 12),
    PCLK_SPI2 => (14, 8),
    CLK_SPI2 => (14, 13),
    PCLK_SPI3 => (14, 9),
    CLK_SPI3 => (14, 14),
    PCLK_SPI4 => (14, 10),
    CLK_SPI4 => (14, 15),
    // ========================================================================
    // UART 时钟门控
    // ========================================================================
    PCLK_UART1 => (12, 2),
    SCLK_UART1 => (12, 13),
    PCLK_UART2 => (12, 3),
    SCLK_UART2 => (13, 0),
    PCLK_UART3 => (12, 4),
    SCLK_UART3 => (13, 3),
    PCLK_UART4 => (12, 5),
    SCLK_UART4 => (13, 6),
    PCLK_UART5 => (12, 6),
    SCLK_UART5 => (13, 9),
    PCLK_UART6 => (12, 7),
    SCLK_UART6 => (13, 12),
    PCLK_UART7 => (12, 8),
    SCLK_UART7 => (13, 15),
    PCLK_UART8 => (12, 9),
    SCLK_UART8 => (14, 2),
    PCLK_UART9 => (12, 10),
    SCLK_UART9 => (14, 5),
    // UART0 (PMU)
    PCLK_UART0 => (0x32 + 2, 6),
    SCLK_UART0 => (0x32 + 2, 5),
    // ========================================================================
    // PWM 时钟门控
    // ========================================================================
    PCLK_PWM1 => (15, 0),
    CLK_PWM1 => (15, 3),
    CLK_PWM1_CAPTURE => (15, 5),
    PCLK_PWM2 => (15, 6),
    CLK_PWM2 => (15, 7),
    CLK_PWM2_CAPTURE => (15, 8),
    PCLK_PWM3 => (15, 1),
    CLK_PWM3 => (15, 4),
    CLK_PWM3_CAPTURE => (15, 9),
    // PMU PWM
    PCLK_PMU1PWM => (0x32 + 2, 8),
    CLK_PMU1PWM => (0x32 + 2, 11),
    CLK_PMU1PWM_CAPTURE => (0x32 + 2, 12),
    // ========================================================================
    // ADC 时钟门控
    // ========================================================================
    PCLK_SARADC => (15, 11),
    CLK_SARADC => (15, 12),
    PCLK_TSADC => (16, 6),
    CLK_TSADC => (16, 7),
    // ========================================================================
    // NPU 时钟门控
    // ========================================================================
    ACLK_NPU1 => (27, 0),
    HCLK_NPU1 => (27, 2),
    ACLK_NPU2 => (28, 0),
    HCLK_NPU2 => (28, 2),
    HCLK_NPU_ROOT => (29, 0),
    CLK_NPU_DSU0 => (29, 1),
    PCLK_NPU_ROOT => (29, 4),
    PCLK_NPU_TIMER => (29, 6),
    CLK_NPUTIMER_ROOT => (29, 7),
    CLK_NPUTIMER0 => (29, 8),
    CLK_NPUTIMER1 => (29, 9),
    PCLK_NPU_WDT => (29, 10),
    TCLK_NPU_WDT => (29, 11),
    PCLK_NPU_PVTM => (29, 12),
    PCLK_NPU_GRF => (29, 13),
    CLK_NPU_PVTM => (29, 14),
    CLK_CORE_NPU_PVTM => (29, 15),
    HCLK_NPU_CM0_ROOT => (30, 1),
    FCLK_NPU_CM0_CORE => (30, 3),
    CLK_NPU_CM0_RTC => (30, 5),
    ACLK_NPU0 => (30, 6),
    HCLK_NPU0 => (30, 8),
    // ========================================================================
    // USB 时钟门控
    // ========================================================================
    // USB3 OTG2
    ACLK_USB3OTG2 => (35, 7),
    SUSPEND_CLK_USB3OTG2 => (35, 8),
    REF_CLK_USB3OTG2 => (35, 9),
    CLK_PIPE_USBHOST3_0 => (38, 9),
    PCLK_PHP_USBHOST3_0 => (32, 0),
    // USB 根时钟
    ACLK_USB_ROOT => (42, 0),
    HCLK_USB_ROOT => (42, 1),
    // USB3 OTG0/1 和 HOST
    ACLK_USB3OTG0 => (42, 4),
    SUSPEND_CLK_USB3OTG0 => (42, 5),
    REF_CLK_USB3OTG0 => (42, 6),
    ACLK_USB3OTG1 => (42, 7),
    SUSPEND_CLK_USB3OTG1 => (42, 8),
    REF_CLK_USB3OTG1 => (42, 9),
    HCLK_HOST0 => (42, 10),
    HCLK_HOST_ARB0 => (42, 11),
    HCLK_HOST1 => (42, 12),
    HCLK_HOST_ARB1 => (42, 13),

    USBDP_PHY0_IMMORTAL => (2, 8),
    USBDP_PHY1_IMMORTAL => (2, 15),

    PCLK_USBDPPHY0 => (72, 2),
    PCLK_USBDPPHY1 => (72, 4),

    ACLK_USB => (74, 0),
    HCLK_USB => (74, 2),

);

clk_composite_table!(
    USBDPPHY_MIPIDCPPHY_REF => (4, 3),
);

// =============================================================================
// Clock Gate 查找和操作
// =============================================================================

impl Cru {
    /// 查找时钟门控配置
    pub fn find_clk_gate(&self, id: ClkId) -> Option<ClkGate> {
        if let Some(res) = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == id)
            .copied()
        {
            return Some(res);
        }

        CLK_COMPOSITE_TABLE
            .iter()
            .find(|gate| gate.clk_id == id)
            .copied()
    }

    /// 获取时钟门控寄存器地址
    pub fn get_gate_reg_offset(&self, gate: ClkGate) -> u32 {
        if gate.reg_idx >= 0x32 {
            // PMU CRU: pmu_clkgate_con
            let idx = gate.reg_idx - 0x32;
            pmu_clkgate_con(idx)
        } else {
            // 主 CRU: clkgate_con
            clkgate_con(gate.reg_idx)
        }
    }
}

// =============================================================================
// 单元测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clk_gate_table_size() {
        // 验证映射表不为空
        assert!(
            !CLK_GATE_TABLE.is_empty(),
            "CLK_GATE_TABLE should not be empty"
        );

        // 验证具体的 gate 数量
        // I2C: 18 (I2C1-8: 16, I2C0: 2)
        // SPI: 10
        // UART: 20 (UART1-9: 18, UART0: 2)
        // PWM: 12 (PWM1-3: 9, PMU1PWM: 3)
        // ADC: 4
        // NPU: 22
        // USB: 23
        // 总计: 109
        assert_eq!(CLK_GATE_TABLE.len(), 109);
    }

    #[test]
    fn test_clk_gate_unique() {
        // 检查是否有重复的 clkid
        let mut clk_ids = CLK_GATE_TABLE
            .iter()
            .map(|gate| gate.clk_id.value())
            .collect::<Vec<_>>();

        clk_ids.sort();
        clk_ids.dedup();

        assert_eq!(
            clk_ids.len(),
            CLK_GATE_TABLE.len(),
            "CLK_GATE_TABLE should not have duplicate clkid entries"
        );
    }

    #[test]
    fn test_i2c_gates() {
        // 验证 I2C gate 配置
        let pclk_i2c1 = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == PCLK_I2C1)
            .expect("PCLK_I2C1 not found");
        assert_eq!(pclk_i2c1.reg_idx, 10);
        assert_eq!(pclk_i2c1.bit, 8);

        let clk_i2c1 = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == CLK_I2C1)
            .expect("CLK_I2C1 not found");
        assert_eq!(clk_i2c1.reg_idx, 11);
        assert_eq!(clk_i2c1.bit, 0);

        // PMU I2C0
        let pclk_i2c0 = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == PCLK_I2C0)
            .expect("PCLK_I2C0 not found");
        assert_eq!(pclk_i2c0.reg_idx, 0x32 + 2);
        assert_eq!(pclk_i2c0.bit, 1);
    }

    #[test]
    fn test_spi_gates() {
        // 验证 SPI gate 配置
        let pclk_spi0 = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == PCLK_SPI0)
            .expect("PCLK_SPI0 not found");
        assert_eq!(pclk_spi0.reg_idx, 14);
        assert_eq!(pclk_spi0.bit, 6);

        let clk_spi0 = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == CLK_SPI0)
            .expect("CLK_SPI0 not found");
        assert_eq!(clk_spi0.reg_idx, 14);
        assert_eq!(clk_spi0.bit, 11);
    }

    #[test]
    fn test_uart_gates() {
        // 验证 UART gate 配置
        let pclk_uart1 = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == PCLK_UART1)
            .expect("PCLK_UART1 not found");
        assert_eq!(pclk_uart1.reg_idx, 12);
        assert_eq!(pclk_uart1.bit, 2);

        let sclk_uart1 = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == SCLK_UART1)
            .expect("SCLK_UART1 not found");
        assert_eq!(sclk_uart1.reg_idx, 12);
        assert_eq!(sclk_uart1.bit, 13);

        // PMU UART0
        let pclk_uart0 = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == PCLK_UART0)
            .expect("PCLK_UART0 not found");
        assert_eq!(pclk_uart0.reg_idx, 0x32 + 2);
        assert_eq!(pclk_uart0.bit, 6);
    }

    #[test]
    fn test_pwm_gates() {
        // 验证 PWM gate 配置
        let pclk_pwm1 = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == PCLK_PWM1)
            .expect("PCLK_PWM1 not found");
        assert_eq!(pclk_pwm1.reg_idx, 15);
        assert_eq!(pclk_pwm1.bit, 0);

        let clk_pwm1 = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == CLK_PWM1)
            .expect("CLK_PWM1 not found");
        assert_eq!(clk_pwm1.reg_idx, 15);
        assert_eq!(clk_pwm1.bit, 3);
    }

    #[test]
    fn test_adc_gates() {
        // 验证 ADC gate 配置
        let pclk_saradc = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == PCLK_SARADC)
            .expect("PCLK_SARADC not found");
        assert_eq!(pclk_saradc.reg_idx, 15);
        assert_eq!(pclk_saradc.bit, 11);

        let clk_saradc = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == CLK_SARADC)
            .expect("CLK_SARADC not found");
        assert_eq!(clk_saradc.reg_idx, 15);
        assert_eq!(clk_saradc.bit, 12);
    }

    #[test]
    fn test_usb_gates() {
        // 验证 USB gate 配置
        let aclk_usb_root = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == ACLK_USB_ROOT)
            .expect("ACLK_USB_ROOT not found");
        assert_eq!(aclk_usb_root.reg_idx, 42);
        assert_eq!(aclk_usb_root.bit, 0);

        let aclk_usb3otg0 = CLK_GATE_TABLE
            .iter()
            .find(|gate| gate.clk_id == ACLK_USB3OTG0)
            .expect("ACLK_USB3OTG0 not found");
        assert_eq!(aclk_usb3otg0.reg_idx, 42);
        assert_eq!(aclk_usb3otg0.bit, 4);
    }
}
