//! RK3588 GRF (General Register Files) 驱动
//!
//! ## 概述
//!
//! GRF 是 Rockchip SoC 的通用寄存器文件，用于配置和控制 SoC 内部的各个子系统。
//! 本模块实现了 USBDP PHY 相关的 GRF 寄存器访问。
//!
//! ## 参考来源
//!
//! - Linux: drivers/phy/rockchip/phy-rockchip-usbdp.c
//! - U-Boot: drivers/phy/phy-rockchip-usbdp.c
//! - 设备树: arch/arm/dts/rk3588s.dtsi
//!
//! ## 寄存器布局
//!
//! ### USBDP PHY GRF (0xfd5c8000 / 0xfd5cc000)
//! ```text
//! Offset 0x0004: LOW_PWRN [13] - 低功耗控制
//!               RX_LFPS  [14] - USB3 RX LFPS 使能
//! ```
//!
//! ### USB GRF (0xfd5ac000)
//! ```text
//! Offset 0x001c: USB3OTG0_CFG - USB3 OTG0 配置
//! Offset 0x0034: USB3OTG1_CFG - USB3 OTG1 配置
//! ```

use tock_registers::interfaces::*;
use tock_registers::registers::*;
use tock_registers::{register_bitfields, register_structs};

use crate::Mmio;

// =============================================================================
// 寄存器位字段定义
// =============================================================================

// USBDP PHY GRF 低功耗控制寄存器
register_bitfields![u32,
    USBDPPHY_LOW_PWRN [
        // RX LFPS enable (USB3)
        RX_LFPS OFFSET(14) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        // Low power mode control
        LOW_PWRN OFFSET(13) NUMBITS(1) [
            PowerDown = 0,
            PowerUp = 1
        ],
    ]
];

// USB3 OTG 配置寄存器
register_bitfields![u32,
    USB3OTG_CFG [
        // USB3 pipe enable
        PIPE_ENABLE OFFSET(15) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        // USB3 PHY disable
        PHY_DISABLE OFFSET(12) NUMBITS(1) [
            Enable = 0,
            Disable = 1
        ],

        // USB3 suspend enable
        SUSPEND_ENABLE OFFSET(10) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        // U3 port disable
        U3_PORT_DISABLE OFFSET(8) NUMBITS(1) [
            Enable = 0,
            Disable = 1
        ],
    ]
];

// USB2PHY GRF 寄存器位字段
register_bitfields![u32,
    USB2PHY0_CON [
        // USB2 PHY port 0 suspend enable
        PORT0_SUSPEND OFFSET(0) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        // USB2 PHY port 0 enable
        PORT0_ENABLE OFFSET(1) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],
    ]
];

// =============================================================================
// GRF 寄存器结构定义
// =============================================================================

register_structs! {
    /// USBDP PHY GRF 寄存器
    #[allow(non_snake_case)]
    pub UsbdpPhyGrfRegs {
        /// 0x00 - 保留
        (0x0000 => _reserved0),
        /// 0x04 - 低功耗控制寄存器
        (0x0004 => pub LOW_PWRN: ReadWrite<u32, USBDPPHY_LOW_PWRN::Register>),
        /// 0x4000 - 结构体结束 (16KB)
        (0x8 => @END),
    }
}

register_structs! {
    /// USB GRF 寄存器
    #[allow(non_snake_case)]
    pub UsbGrfRegs {
        /// 0x00 - 0x18: 保留
        (0x0000 => _reserved0),

        /// 0x1c - USB3 OTG0 配置寄存器
        (0x001c => pub USB3OTG0_CFG: ReadWrite<u32, USB3OTG_CFG::Register>),

        /// 0x20 - 0x30: 保留
        (0x0020 => _reserved1),

        /// 0x34 - USB3 OTG1 配置寄存器
        (0x0034 => pub USB3OTG1_CFG: ReadWrite<u32, USB3OTG_CFG::Register>),

        (0x0038 => @END),
    }
}

register_structs! {
    /// USB2PHY GRF 寄存器
    #[allow(non_snake_case)]
    pub Usb2PhyGrfRegs {
        /// 0x00 - USB2 PHY common configuration
        (0x0000 => pub CON: ReadWrite<u32, USB2PHY0_CON::Register>),
        (0x4 => @END),
    }
}

// =============================================================================
// GRF 类型定义
// =============================================================================

/// GRF 类型标识
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum GrfType {
    /// USBDP PHY GRF (用于 PHY 配置)
    UsbdpPhy,
    /// USB GRF (用于 USB 控制器配置)
    Usb,
    /// USB2PHY GRF (用于 USB2 PHY 配置)
    Usb2Phy,
}

/// GRF 驱动实例
#[allow(dead_code)]
pub struct Grf {
    /// GRF 寄存器基址
    base: usize,
    /// GRF 类型
    grf_type: GrfType,
}

#[allow(dead_code)]
impl Grf {
    /// 创建新的 GRF 实例
    ///
    /// # Safety
    ///
    /// 调用者必须确保 `mmio_base` 指向有效的内存映射寄存器区域
    pub unsafe fn new(mmio_base: Mmio, grf_type: GrfType) -> Self {
        Self {
            base: mmio_base.as_ptr() as usize,
            grf_type,
        }
    }

    /// 获取 GRF 寄存器基址
    #[inline]
    fn base(&self) -> usize {
        self.base
    }

    /// 获取 GRF 类型
    #[inline]
    pub fn grf_type(&self) -> GrfType {
        self.grf_type
    }

    // ========================================================================
    // USBDP PHY GRF 专用方法
    // ========================================================================

    /// 获取 USBDP PHY GRF 寄存器
    fn usbdpphy_regs(&self) -> &UsbdpPhyGrfRegs {
        unsafe { &*(self.base() as *const UsbdpPhyGrfRegs) }
    }

    /// 获取可变的 USBDP PHY GRF 寄存器
    fn usbdpphy_regs_mut(&mut self) -> &mut UsbdpPhyGrfRegs {
        unsafe { &mut *(self.base() as *mut UsbdpPhyGrfRegs) }
    }

    /// 退出低功耗模式
    ///
    /// 设置 LOW_PWRN = 1，使 PHY 退出低功耗模式
    ///
    /// 根据 RK3588 TRM 和 u-boot grfreg_write() 实现，Rockchip GRF 寄存器格式：
    /// ```text
    /// Bit[31:16] - 写使能位（每bit独立控制对应的数据位）
    /// Bit[15:0]  - 数据位
    /// ```
    ///
    /// 写入公式：`value = (data << bit) | (mask << 16)`
    /// 对于 LOW_PWRN (bit 13):
    ///   - data = 1 (PowerUp)
    ///   - mask = 1 << 13
    ///   - value = (1 << 13) | (1 << 29) = 0x20002000
    pub fn exit_low_power(&self) {
        log::debug!("GRF@{:x}: Exiting low power mode", self.base());

        // 直接写入，正确设置写使能位
        // Bit[13] = 1 (PowerUp), Bit[29] = 1 (Write Enable)
        const VALUE: u32 = (1 << 13) | (1 << 29);

        self.usbdpphy_regs().LOW_PWRN.set(VALUE);

        // 读取并验证
        let read_val: u32 = self.usbdpphy_regs().LOW_PWRN.extract().into();
        log::debug!(
            "GRF@{:x}: LOW_PWRN after write: 0x{:08x} (expected bit13=1)",
            self.base(),
            read_val
        );
    }

    /// 进入低功耗模式
    ///
    /// 设置 LOW_PWRN = 0，使 PHY 进入低功耗模式
    ///
    /// 写入值：`value = (0 << 13) | (1 << 29) = 0x20000000`
    pub fn enter_low_power(&self) {
        log::debug!("GRF@{:x}: Entering low power mode", self.base());

        // 直接写入，正确设置写使能位
        // Bit[13] = 0 (PowerDown), Bit[29] = 1 (Write Enable)
        const VALUE: u32 = 1 << 29;

        self.usbdpphy_regs().LOW_PWRN.set(VALUE);

        log::debug!("GRF@{:x}: LOW_PWRN set to power down", self.base());
    }

    /// 启用 USB3 RX LFPS
    ///
    /// 设置 RX_LFPS = 1，使能 USB3 Low Frequency Periodic Signaling 接收
    ///
    /// 写入值：`value = (1 << 14) | (1 << 30) = 0x40004000`
    pub fn enable_rx_lfps(&self) {
        log::debug!("GRF@{:x}: Enabling RX LFPS", self.base());

        // 直接写入，正确设置写使能位
        // Bit[14] = 1 (Enable), Bit[30] = 1 (Write Enable)
        const VALUE: u32 = (1 << 14) | (1 << 30);

        self.usbdpphy_regs().LOW_PWRN.set(VALUE);

        // 读取并验证
        let read_val: u32 = self.usbdpphy_regs().LOW_PWRN.extract().into();
        log::debug!(
            "GRF@{:x}: RX_LFPS after write: 0x{:08x} (expected bit14=1)",
            self.base(),
            read_val
        );
    }

    /// 禁用 USB3 RX LFPS
    ///
    /// 设置 RX_LFPS = 0，禁用 USB3 Low Frequency Periodic Signaling 接收
    ///
    /// 写入值：`value = (0 << 14) | (1 << 30) = 0x40000000`
    pub fn disable_rx_lfps(&self) {
        log::debug!("GRF@{:x}: Disabling RX LFPS", self.base());

        // 直接写入，正确设置写使能位
        // Bit[14] = 0 (Disable), Bit[30] = 1 (Write Enable)
        const VALUE: u32 = 1 << 30;

        self.usbdpphy_regs().LOW_PWRN.set(VALUE);

        log::debug!("GRF@{:x}: RX_LFPS disabled", self.base());
    }

    /// 检查是否在低功耗模式
    pub fn is_low_power(&self) -> bool {
        self.usbdpphy_regs()
            .LOW_PWRN
            .read(USBDPPHY_LOW_PWRN::LOW_PWRN)
            == 0
    }

    // ========================================================================
    // USB GRF 专用方法
    // ========================================================================

    /// 获取 USB GRF 寄存器
    fn usb_regs(&self) -> &UsbGrfRegs {
        unsafe { &*(self.base() as *const UsbGrfRegs) }
    }

    /// 获取可变的 USB GRF 寄存器
    fn usb_regs_mut(&mut self) -> &mut UsbGrfRegs {
        unsafe { &mut *(self.base() as *mut UsbGrfRegs) }
    }

    /// 启用 USB3 U3 端口
    ///
    /// 参考 U-Boot: udphy_u3_port_disable(udphy, false)
    ///
    /// 写入值（按 Rockchip GRF 格式）：
    /// - Bit[31:16]: 0xFFFF (写使能掩码，全1表示所有低16位都可写)
    /// - Bit[15:0]: 0x8800 (数据值)
    ///   - bit 15: PIPE_ENABLE = 1
    ///   - bit 12: PHY_DISABLE = 0
    ///   - bit 10: SUSPEND_ENABLE = 0
    ///   - bit 8: U3_PORT_DISABLE = 0
    pub fn enable_u3_port(&mut self, port: u8) {
        log::debug!("GRF@{:x}: Enabling USB3 U3 port {}", self.base(), port);

        // ⚠️ Rockchip GRF 格式: Bit[31:16] 是写使能掩码
        // 我们要写入 0x8800 到 Bit[15:0]，所以完整值是 0xFFFF8800
        //
        // 数据值 0x8800:
        // - bit 15: PIPE_ENABLE = 1 (0x8000)
        // - bit 12: PHY_DISABLE = 0 (需要清除)
        // - bit 10: SUSPEND_ENABLE = 0 (需要清除)
        // - bit 8: U3_PORT_DISABLE = 0 (需要清除)
        const GRF_VALUE: u32 = 0xFFFF8800; // 写使能掩码 + 数据值

        let base = self.base();
        let offset = if port == 0 {
            0x001c // USB3OTG0_CFG offset
        } else {
            0x0034 // USB3OTG1_CFG offset
        };
        let reg_name = if port == 0 {
            "USB3OTG0_CFG"
        } else {
            "USB3OTG1_CFG"
        };

        // 直接使用指针写入（绕过 tock-registers，使用 GRF 格式）
        let addr = (base + offset) as *mut u32;
        unsafe {
            addr.write_volatile(GRF_VALUE);
        }

        log::debug!(
            "GRF@{:x}: Wrote {} with GRF format: 0x{:08x} (data=0x8800)",
            base,
            reg_name,
            GRF_VALUE
        );

        // 读取并验证（注意：读取时只返回 Bit[15:0] 的数据值）
        let regs = self.usb_regs();
        let value = if port == 0 {
            regs.USB3OTG0_CFG.get()
        } else {
            regs.USB3OTG1_CFG.get()
        };

        // 检查关键位
        let pipe_enable = (value >> 15) & 0x1;
        let phy_disable = (value >> 12) & 0x1;
        let suspend_enable = (value >> 10) & 0x1;
        let u3_port_disable = (value >> 8) & 0x1;

        log::info!("GRF@{:x}: {} after enable: 0x{:08x}", base, reg_name, value);
        log::info!("  PIPE_ENABLE (bit15): {} (expected 1)", pipe_enable);
        log::info!("  PHY_DISABLE (bit12): {} (expected 0)", phy_disable);
        log::info!("  SUSPEND_ENABLE (bit10): {} (expected 0)", suspend_enable);
        log::info!("  U3_PORT_DISABLE (bit8): {} (expected 0)", u3_port_disable);

        if pipe_enable == 1 && phy_disable == 0 && suspend_enable == 0 && u3_port_disable == 0 {
            log::info!(
                "✓ GRF@{:x}: USB3 U3 port {} enabled successfully",
                base,
                port
            );
        } else {
            log::warn!(
                "⚠ GRF@{:x}: USB3 U3 port {} may not be configured correctly!",
                base,
                port
            );
        }
    }

    /// 禁用 USB3 U3 端口
    ///
    /// 参考 U-Boot: udphy_u3_port_disable(udphy, true)
    ///
    /// 写入 0x1100:
    /// - bit 15: PIPE_ENABLE = 1
    /// - bit 12: PHY_DISABLE = 1
    /// - bit 10: SUSPEND_ENABLE = 0
    /// - bit 8: U3_PORT_DISABLE = 0
    pub fn disable_u3_port(&mut self, port: u8) {
        log::debug!("GRF@{:x}: Disabling USB3 U3 port {}", self.base(), port);

        let regs = self.usb_regs_mut();
        if port == 0 {
            regs.USB3OTG0_CFG
                .modify(USB3OTG_CFG::PIPE_ENABLE::Enable + USB3OTG_CFG::PHY_DISABLE::Disable);
        } else {
            regs.USB3OTG1_CFG
                .modify(USB3OTG_CFG::PIPE_ENABLE::Enable + USB3OTG_CFG::PHY_DISABLE::Disable);
        }
    }

    /// 检查 USB3 U3 端口是否启用
    pub fn is_u3_port_enabled(&self, port: u8) -> bool {
        let regs = self.usb_regs();
        let value = if port == 0 {
            regs.USB3OTG0_CFG.get()
        } else {
            regs.USB3OTG1_CFG.get()
        };
        (value & (1 << 8)) == 0
    }

    // ========================================================================
    // USB2PHY GRF 专用方法
    // ========================================================================

    /// 获取 USB2PHY GRF 寄存器
    fn usb2phy_regs(&self) -> &Usb2PhyGrfRegs {
        unsafe { &*(self.base() as *const Usb2PhyGrfRegs) }
    }

    /// 获取可变的 USB2PHY GRF 寄存器
    fn usb2phy_regs_mut(&mut self) -> &mut Usb2PhyGrfRegs {
        unsafe { &mut *(self.base() as *mut Usb2PhyGrfRegs) }
    }

    /// 使能 USB2 PHY 端口
    ///
    /// 设置 PORT_ENABLE = 1, PORT_SUSPEND = 0
    ///
    /// 根据 Rockchip GRF 格式：
    /// ```text
    /// Bit[31:16] - 写使能位
    /// Bit[15:0]  - 数据位
    /// ```
    pub fn enable_usb2phy_port(&mut self) {
        log::debug!("GRF@{:x}: Enabling USB2 PHY port", self.base());

        // Bit[1] = 1 (Enable), Bit[17] = 1 (Write Enable)
        // Bit[0] = 0 (No Suspend), Bit[16] = 1 (Write Enable)
        const VALUE: u32 = ((1 << 1) | (1 << 17)) | (1 << 16);

        self.usb2phy_regs_mut().CON.set(VALUE);

        // 读取并验证
        let read_val: u32 = self.usb2phy_regs().CON.extract().into();
        log::debug!(
            "GRF@{:x}: USB2PHY CON after write: 0x{:08x} (expected bit1=1)",
            self.base(),
            read_val
        );
    }
}

// =============================================================================
// 测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grf_type() {
        assert_eq!(GrfType::UsbdpPhy, GrfType::UsbdpPhy);
        assert_eq!(GrfType::Usb, GrfType::Usb);
        assert_ne!(GrfType::UsbdpPhy, GrfType::Usb);
    }

    #[test]
    fn test_register_bitfields() {
        // 测试 USBDPPHY_LOW_PWRN 位字段
        let value =
            USBDPPHY_LOW_PWRN::LOW_PWRN::PowerUp.value | USBDPPHY_LOW_PWRN::RX_LFPS::Enable.value;
        assert_eq!(value, (1 << 13) | (1 << 14));

        // 测试 USB3OTG_CFG 位字段
        let value =
            USB3OTG_CFG::PIPE_ENABLE::Enable.value + USB3OTG_CFG::U3_PORT_DISABLE::Disable.value;
        assert_eq!(value, (1 << 15) | (0 << 8));
    }

    #[test]
    fn test_enable_u3_port_value() {
        // 0x0188 = bit 15 = 1, bit 8 = 0
        let expected: u32 = 0x0188;
        let value =
            USB3OTG_CFG::PIPE_ENABLE::Enable.value + USB3OTG_CFG::U3_PORT_DISABLE::Disable.value;
        assert_eq!(value, expected);
    }

    #[test]
    fn test_disable_u3_port_value() {
        // 0x1100 = bit 15 = 1, bit 12 = 1
        let expected: u32 = 0x1100;
        let value =
            USB3OTG_CFG::PIPE_ENABLE::Enable.value + USB3OTG_CFG::PHY_DISABLE::Disable.value;
        assert_eq!(value, expected);
    }
}
