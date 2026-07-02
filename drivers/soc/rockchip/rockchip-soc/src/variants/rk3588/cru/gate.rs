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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClkType {
    Gate,
    Composite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateBank {
    Main,
    Pmu,
    Php,
}

/// 时钟门控配置
#[derive(Debug, Clone, Copy)]
pub struct ClkGate {
    /// 时钟 ID
    pub clk_id: ClkId,
    pub kind: ClkType,
    pub bank: GateBank,
    /// 寄存器索引，在 `bank` 指定的 CRU gate 寄存器组内编号。
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
                    bank: GateBank::Main,
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
                    bank: GateBank::Main,
                    reg_idx: $reg_idx,
                    bit: $bit,
                }
                ),*
        ];
    };
}

macro_rules! clk_pmu_composite_table {
    ($($clk_id:expr => ($reg_idx:expr, $bit:expr)),* $(,)?) => {
        #[allow(non_upper_case_globals)]
        const CLK_PMU_COMPOSITE_TABLE: &[ClkGate] = &[
            $(
                ClkGate {
                    clk_id: $clk_id,
                    kind: ClkType::Composite,
                    bank: GateBank::Pmu,
                    reg_idx: $reg_idx,
                    bit: $bit,
                }
                ),*
        ];
    };
}

macro_rules! clk_pmu_gate_table {
    ($($clk_id:expr => ($reg_idx:expr, $bit:expr)),* $(,)?) => {
        #[allow(non_upper_case_globals)]
        const CLK_PMU_GATE_TABLE: &[ClkGate] = &[
            $(
                ClkGate {
                    clk_id: $clk_id,
                    kind: ClkType::Gate,
                    bank: GateBank::Pmu,
                    reg_idx: $reg_idx,
                    bit: $bit,
                }
                ),*
        ];
    };
}

macro_rules! clk_php_gate_table {
    ($($clk_id:expr => ($reg_idx:expr, $bit:expr)),* $(,)?) => {
        #[allow(non_upper_case_globals)]
        const CLK_PHP_GATE_TABLE: &[ClkGate] = &[
            $(
                ClkGate {
                    clk_id: $clk_id,
                    kind: ClkType::Gate,
                    bank: GateBank::Php,
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
    // NVM/eMMC 时钟门控
    // ========================================================================
    HCLK_NVM_ROOT => (31, 0),
    ACLK_NVM_ROOT => (31, 1),
    HCLK_EMMC => (31, 4),
    ACLK_EMMC => (31, 5),
    CCLK_EMMC => (31, 6),
    BCLK_EMMC => (31, 7),
    TMCLK_EMMC => (31, 8),
    // ========================================================================
    // PCIe/PHP 时钟门控
    // ========================================================================
    ACLK_PCIE_ROOT => (32, 6),
    ACLK_PHP_ROOT => (32, 7),
    ACLK_PCIE_BRIDGE => (32, 8),
    ACLK_PHP_GIC_ITS => (34, 6),
    ACLK_MMU_PCIE => (34, 7),
    ACLK_MMU_PHP => (34, 8),
    ACLK_PCIE_4L_DBI => (32, 13),
    ACLK_PCIE_2L_DBI => (32, 14),
    ACLK_PCIE_1L0_DBI => (32, 15),
    ACLK_PCIE_1L1_DBI => (33, 0),
    ACLK_PCIE_1L2_DBI => (33, 1),
    ACLK_PCIE_4L_MSTR => (33, 2),
    ACLK_PCIE_2L_MSTR => (33, 3),
    ACLK_PCIE_1L0_MSTR => (33, 4),
    ACLK_PCIE_1L1_MSTR => (33, 5),
    ACLK_PCIE_1L2_MSTR => (33, 6),
    ACLK_PCIE_4L_SLV => (33, 7),
    ACLK_PCIE_2L_SLV => (33, 8),
    ACLK_PCIE_1L0_SLV => (33, 9),
    ACLK_PCIE_1L1_SLV => (33, 10),
    ACLK_PCIE_1L2_SLV => (33, 11),
    PCLK_PCIE_4L => (33, 12),
    PCLK_PCIE_2L => (33, 13),
    PCLK_PCIE_1L0 => (33, 14),
    PCLK_PCIE_1L1 => (33, 15),
    PCLK_PCIE_1L2 => (34, 0),
    CLK_PCIE_AUX0 => (34, 1),
    CLK_PCIE_AUX1 => (34, 2),
    CLK_PCIE_AUX2 => (34, 3),
    CLK_PCIE_AUX3 => (34, 4),
    CLK_PCIE_AUX4 => (34, 5),
    CLK_PIPEPHY0_REF => (37, 0),
    CLK_PIPEPHY1_REF => (37, 1),
    CLK_PIPEPHY2_REF => (37, 2),
    CLK_REF_PIPE_PHY0_OSC_SRC => (77, 0),
    CLK_REF_PIPE_PHY1_OSC_SRC => (77, 1),
    CLK_REF_PIPE_PHY2_OSC_SRC => (77, 2),
    PCLK_PHP_ROOT => (32, 0),
    CLK_PCIE4L_PIPE => (39, 0),
    CLK_PCIE2L_PIPE => (39, 1),
    CLK_PIPEPHY0_PIPE_G => (38, 3),
    CLK_PIPEPHY1_PIPE_G => (38, 4),
    CLK_PIPEPHY2_PIPE_G => (38, 5),
    CLK_PIPEPHY0_PIPE_ASIC_G => (38, 6),
    CLK_PIPEPHY1_PIPE_ASIC_G => (38, 7),
    CLK_PIPEPHY2_PIPE_ASIC_G => (38, 8),
    CLK_PIPEPHY2_PIPE_U3_G => (38, 9),
    CLK_PCIE1L2_PIPE => (38, 13),
    CLK_PCIE1L0_PIPE => (38, 14),
    CLK_PCIE1L1_PIPE => (38, 15),
    // ========================================================================
    // USB 时钟门控
    // ========================================================================
    // USB3 OTG2
    ACLK_USB3OTG2 => (35, 7),
    SUSPEND_CLK_USB3OTG2 => (35, 8),
    REF_CLK_USB3OTG2 => (35, 9),
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

    // ========================================================================
    // JPEG 解码器 (VDPU) 叶子时钟门控
    // (leaf gates verified against mainline drivers/clk/rockchip/clk-rk3588.c:
    //  ACLK_JPEG_DECODER = CLKGATE_CON(45) bit 2, HCLK = CLKGATE_CON(45) bit 3;
    //  the aclk_vdpu_root parent is left enabled by the bootloader, as for RGA2)
    // ========================================================================
    ACLK_JPEG_DECODER => (45, 2),
    HCLK_JPEG_DECODER => (45, 3),

);

clk_pmu_gate_table!(
    // I2C0 (PMU)
    PCLK_I2C0 => (2, 1),
    CLK_I2C0 => (2, 2),
    // UART0 (PMU)
    PCLK_UART0 => (2, 6),
    SCLK_UART0 => (2, 5),
    // PMU PWM
    PCLK_PMU1PWM => (2, 8),
    CLK_PMU1PWM => (2, 11),
    CLK_PMU1PWM_CAPTURE => (2, 12),
);

clk_php_gate_table!(
    PCLK_PCIE_COMBO_PIPE_PHY0 => (0, 5),
    PCLK_PCIE_COMBO_PIPE_PHY1 => (0, 6),
    PCLK_PCIE_COMBO_PIPE_PHY2 => (0, 7),
    PCLK_PCIE_COMBO_PIPE_PHY => (0, 8),
);

clk_composite_table!(
    CLK_REF_PIPE_PHY0_PLL_SRC => (77, 3),
    CLK_REF_PIPE_PHY1_PLL_SRC => (77, 4),
    CLK_REF_PIPE_PHY2_PLL_SRC => (77, 5),
);

clk_pmu_composite_table!(
    CLK_USB2PHY_HDPTXRXPHY_REF => (4, 7),
    CLK_USBDPPHY_MIPIDCPPHY_REF => (4, 3),
);

// =============================================================================
// Clock Gate 查找和操作
// =============================================================================

impl Cru {
    /// 查找时钟门控配置
    pub fn find_clk_gate(&self, id: ClkId) -> Option<ClkGate> {
        CLK_GATE_TABLE
            .iter()
            .chain(CLK_PMU_GATE_TABLE)
            .chain(CLK_PHP_GATE_TABLE)
            .chain(CLK_COMPOSITE_TABLE)
            .chain(CLK_PMU_COMPOSITE_TABLE)
            .find(|gate| gate.clk_id == id)
            .copied()
    }

    /// 获取时钟门控寄存器地址
    pub fn get_gate_reg_offset(&self, gate: ClkGate) -> u32 {
        match gate.bank {
            GateBank::Main => clkgate_con(gate.reg_idx),
            GateBank::Pmu => pmu_clkgate_con(gate.reg_idx),
            GateBank::Php => php_clkgate_con(gate.reg_idx),
        }
    }
}

// =============================================================================
// 单元测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn find_gate(clk_id: ClkId) -> ClkGate {
        CLK_GATE_TABLE
            .iter()
            .chain(CLK_PMU_GATE_TABLE)
            .chain(CLK_PHP_GATE_TABLE)
            .chain(CLK_COMPOSITE_TABLE)
            .chain(CLK_PMU_COMPOSITE_TABLE)
            .find(|gate| gate.clk_id == clk_id)
            .copied()
            .expect("gate not found")
    }

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
        // PCIe/PHP: 57
        // USB: 21 main/php gates + 2 PMU composite gates
        // NVM/eMMC: 7
        // JPEG decoder (VDPU): 2
        // 总计: 175
        assert_eq!(
            CLK_GATE_TABLE.len()
                + CLK_PMU_GATE_TABLE.len()
                + CLK_PHP_GATE_TABLE.len()
                + CLK_COMPOSITE_TABLE.len()
                + CLK_PMU_COMPOSITE_TABLE.len(),
            175
        );
    }

    #[test]
    fn test_clk_gate_unique() {
        // 检查是否有重复的 clkid
        let mut clk_ids = CLK_GATE_TABLE
            .iter()
            .chain(CLK_PMU_GATE_TABLE)
            .chain(CLK_PHP_GATE_TABLE)
            .chain(CLK_COMPOSITE_TABLE)
            .chain(CLK_PMU_COMPOSITE_TABLE)
            .map(|gate| gate.clk_id.value())
            .collect::<Vec<_>>();

        clk_ids.sort();
        clk_ids.dedup();

        assert_eq!(
            clk_ids.len(),
            CLK_GATE_TABLE.len()
                + CLK_PMU_GATE_TABLE.len()
                + CLK_PHP_GATE_TABLE.len()
                + CLK_COMPOSITE_TABLE.len()
                + CLK_PMU_COMPOSITE_TABLE.len(),
            "CLK_GATE_TABLE should not have duplicate clkid entries"
        );
    }

    #[test]
    fn test_i2c_gates() {
        // 验证 I2C gate 配置
        let pclk_i2c1 = find_gate(PCLK_I2C1);
        assert_eq!(pclk_i2c1.bank, GateBank::Main);
        assert_eq!(pclk_i2c1.reg_idx, 10);
        assert_eq!(pclk_i2c1.bit, 8);

        let clk_i2c1 = find_gate(CLK_I2C1);
        assert_eq!(clk_i2c1.reg_idx, 11);
        assert_eq!(clk_i2c1.bit, 0);

        // PMU I2C0
        let pclk_i2c0 = find_gate(PCLK_I2C0);
        assert_eq!(pclk_i2c0.bank, GateBank::Pmu);
        assert_eq!(pclk_i2c0.reg_idx, 2);
        assert_eq!(pclk_i2c0.bit, 1);
    }

    #[test]
    fn test_spi_gates() {
        // 验证 SPI gate 配置
        let pclk_spi0 = find_gate(PCLK_SPI0);
        assert_eq!(pclk_spi0.reg_idx, 14);
        assert_eq!(pclk_spi0.bit, 6);

        let clk_spi0 = find_gate(CLK_SPI0);
        assert_eq!(clk_spi0.reg_idx, 14);
        assert_eq!(clk_spi0.bit, 11);
    }

    #[test]
    fn test_uart_gates() {
        // 验证 UART gate 配置
        let pclk_uart1 = find_gate(PCLK_UART1);
        assert_eq!(pclk_uart1.reg_idx, 12);
        assert_eq!(pclk_uart1.bit, 2);

        let sclk_uart1 = find_gate(SCLK_UART1);
        assert_eq!(sclk_uart1.reg_idx, 12);
        assert_eq!(sclk_uart1.bit, 13);

        // PMU UART0
        let pclk_uart0 = find_gate(PCLK_UART0);
        assert_eq!(pclk_uart0.bank, GateBank::Pmu);
        assert_eq!(pclk_uart0.reg_idx, 2);
        assert_eq!(pclk_uart0.bit, 6);
    }

    #[test]
    fn test_pwm_gates() {
        // 验证 PWM gate 配置
        let pclk_pwm1 = find_gate(PCLK_PWM1);
        assert_eq!(pclk_pwm1.reg_idx, 15);
        assert_eq!(pclk_pwm1.bit, 0);

        let clk_pwm1 = find_gate(CLK_PWM1);
        assert_eq!(clk_pwm1.reg_idx, 15);
        assert_eq!(clk_pwm1.bit, 3);
    }

    #[test]
    fn test_adc_gates() {
        // 验证 ADC gate 配置
        let pclk_saradc = find_gate(PCLK_SARADC);
        assert_eq!(pclk_saradc.reg_idx, 15);
        assert_eq!(pclk_saradc.bit, 11);

        let clk_saradc = find_gate(CLK_SARADC);
        assert_eq!(clk_saradc.reg_idx, 15);
        assert_eq!(clk_saradc.bit, 12);
    }

    #[test]
    fn test_usb_gates() {
        // 验证 USB gate 配置
        let aclk_usb_root = find_gate(ACLK_USB_ROOT);
        assert_eq!(aclk_usb_root.reg_idx, 42);
        assert_eq!(aclk_usb_root.bit, 0);

        let aclk_usb3otg0 = find_gate(ACLK_USB3OTG0);
        assert_eq!(aclk_usb3otg0.reg_idx, 42);
        assert_eq!(aclk_usb3otg0.bit, 4);

        let usb2phy_ref = find_gate(CLK_USB2PHY_HDPTXRXPHY_REF);
        assert_eq!(usb2phy_ref.kind, ClkType::Composite);
        assert_eq!(usb2phy_ref.bank, GateBank::Pmu);
        assert_eq!(usb2phy_ref.reg_idx, 4);
        assert_eq!(usb2phy_ref.bit, 7);

        let usbdpphy_ref = find_gate(CLK_USBDPPHY_MIPIDCPPHY_REF);
        assert_eq!(usbdpphy_ref.kind, ClkType::Composite);
        assert_eq!(usbdpphy_ref.bank, GateBank::Pmu);
        assert_eq!(usbdpphy_ref.reg_idx, 4);
        assert_eq!(usbdpphy_ref.bit, 3);
    }

    #[test]
    fn test_emmc_gates_match_orangepi_6_1() {
        let hclk_nvm = find_gate(HCLK_NVM_ROOT);
        assert_eq!(hclk_nvm.bank, GateBank::Main);
        assert_eq!(hclk_nvm.reg_idx, 31);
        assert_eq!(hclk_nvm.bit, 0);

        let aclk_nvm = find_gate(ACLK_NVM_ROOT);
        assert_eq!(aclk_nvm.reg_idx, 31);
        assert_eq!(aclk_nvm.bit, 1);

        let hclk_emmc = find_gate(HCLK_EMMC);
        assert_eq!(hclk_emmc.reg_idx, 31);
        assert_eq!(hclk_emmc.bit, 4);

        let aclk_emmc = find_gate(ACLK_EMMC);
        assert_eq!(aclk_emmc.reg_idx, 31);
        assert_eq!(aclk_emmc.bit, 5);

        let cclk_emmc = find_gate(CCLK_EMMC);
        assert_eq!(cclk_emmc.reg_idx, 31);
        assert_eq!(cclk_emmc.bit, 6);

        let bclk_emmc = find_gate(BCLK_EMMC);
        assert_eq!(bclk_emmc.reg_idx, 31);
        assert_eq!(bclk_emmc.bit, 7);

        let tmclk_emmc = find_gate(TMCLK_EMMC);
        assert_eq!(tmclk_emmc.reg_idx, 31);
        assert_eq!(tmclk_emmc.bit, 8);
    }

    #[test]
    fn test_pcie_gates_match_orangepi_6_1() {
        let aclk_mst_1l1 = find_gate(ACLK_PCIE_1L1_MSTR);
        assert_eq!(aclk_mst_1l1.bank, GateBank::Main);
        assert_eq!(aclk_mst_1l1.reg_idx, 33);
        assert_eq!(aclk_mst_1l1.bit, 5);

        let aclk_slv_1l1 = find_gate(ACLK_PCIE_1L1_SLV);
        assert_eq!(aclk_slv_1l1.reg_idx, 33);
        assert_eq!(aclk_slv_1l1.bit, 10);

        let aclk_dbi_1l1 = find_gate(ACLK_PCIE_1L1_DBI);
        assert_eq!(aclk_dbi_1l1.reg_idx, 33);
        assert_eq!(aclk_dbi_1l1.bit, 0);

        let pclk_1l1 = find_gate(PCLK_PCIE_1L1);
        assert_eq!(pclk_1l1.reg_idx, 33);
        assert_eq!(pclk_1l1.bit, 15);

        let aux_1l1 = find_gate(CLK_PCIE_AUX3);
        assert_eq!(aux_1l1.reg_idx, 34);
        assert_eq!(aux_1l1.bit, 4);

        let pipe_1l1 = find_gate(CLK_PCIE1L1_PIPE);
        assert_eq!(pipe_1l1.reg_idx, 38);
        assert_eq!(pipe_1l1.bit, 15);

        let aclk_mst_1l2 = find_gate(ACLK_PCIE_1L2_MSTR);
        assert_eq!(aclk_mst_1l2.reg_idx, 33);
        assert_eq!(aclk_mst_1l2.bit, 6);

        let pipe_1l2 = find_gate(CLK_PCIE1L2_PIPE);
        assert_eq!(pipe_1l2.reg_idx, 38);
        assert_eq!(pipe_1l2.bit, 13);
    }

    #[test]
    fn test_php_gates() {
        let combo_phy0 = find_gate(PCLK_PCIE_COMBO_PIPE_PHY0);
        assert_eq!(combo_phy0.bank, GateBank::Php);
        assert_eq!(combo_phy0.reg_idx, 0);
        assert_eq!(combo_phy0.bit, 5);
    }
}
