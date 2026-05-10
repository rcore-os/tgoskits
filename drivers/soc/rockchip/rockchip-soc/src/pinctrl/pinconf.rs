//! Pinconf 配置类型
//!
//! 定义引脚电气属性配置，包括上下拉、驱动强度等。

use core::ptr::NonNull;

use crate::PinId;

/// 引脚上下拉配置
///
/// 定义引脚的上下拉电阻配置。
///
/// # 示例
///
/// ```
/// use rockchip_soc::pinctrl::Pull;
///
/// // 配置为上拉
/// let pull = Pull::PullUp;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Pull {
    Disabled       = 0,
    BusHold        = 2,
    PullUp         = 3,
    PullDown       = 4,
    PullPinDefault = 5,
}

bitflags::bitflags! {
    /// IOMUX 配置标志
    ///
    /// 定义引脚复用控制的属性和特性,对应 Rockchip pinctrl 驱动中的 iomux 标志。
    ///
    /// # 标志说明
    ///
    /// - `GPIO_ONLY`: 引脚仅支持 GPIO 模式,不支持复用功能
    /// - `WIDTH_4BIT`: 功能选择位宽为 4 位
    /// - `SOURCE_PMU`: 寄存器位于 PMU (Power Management Unit) 地址空间
    /// - `UNROUTED`: 未路由的引脚(无实际连接)
    /// - `WIDTH_3BIT`: 功能选择位宽为 3 位
    /// - `8WIDTH_2BIT`: 8 个引脚共享 2 位宽度的功能选择
    /// - `WRITABLE_32BIT`: 使用 32 位写操作(而非 16 位)
    /// - `L_SOURCE_PMU`: 低 16 位位于 PMU 地址空间
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Iomux: u8 {
        /// 仅 GPIO 模式(无复用功能)
        const GPIO_ONLY = 1;
        /// 功能选择位宽为 4 位
        const WIDTH_4BIT = 1 << 1;
        /// 寄存器位于 PMU 地址空间
        const SOURCE_PMU = 1 << 2;
        /// 未路由的引脚
        const UNROUTED = 1 << 3;
        /// 功能选择位宽为 3 位
        const WIDTH_3BIT = 1 << 4;
        /// 8 引脚共享 2 位功能选择
        const WIDTH_8_2BIT = 1 << 5;
        /// 使用 32 位写操作
        const WRITABLE_32BIT = 1 << 6;
        /// 低 16 位位于 PMU 地址空间
        const L_SOURCE_PMU = 1 << 7;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinConfig {
    pub id: PinId,
    /// 引脚功能
    pub mux: Iomux,
    pub pull: Pull,
    /// 可选的驱动强度配置
    pub drive: Option<u32>,
}

impl PinConfig {
    /// `rockchip,pins` property
    pub fn new_with_fdt(cells: &[u32], fdt_addr: NonNull<u8>) -> Self {
        let bank = cells[0];
        let pin = cells[1];
        let mux = cells[2];
        let conf_phandle = cells[3];
        let id = PinId::from_bank_pin(bank.into(), pin).unwrap();

        let fdt = unsafe { fdt_edit::Fdt::from_ptr(fdt_addr.as_ptr()).unwrap() };

        let conf_node = fdt.get_by_phandle(conf_phandle.into()).unwrap();

        let mut pull = Pull::Disabled;
        let mut drive = None;

        for prop in conf_node.as_node().properties() {
            match prop.name() {
                "bias-disable" => {
                    pull = Pull::Disabled;
                }
                "bias-bus-hold" => {
                    pull = Pull::BusHold;
                }
                "bias-pull-up" => {
                    pull = Pull::PullUp;
                }
                "bias-pull-down" => {
                    pull = Pull::PullDown;
                }
                "bias-pull-pin-default" => {
                    pull = Pull::PullPinDefault;
                }
                "drive-strength" => {
                    let value = prop.get_u32().unwrap_or(1);
                    drive = Some(value);
                }
                "phandle" => {}
                n => {
                    warn!("Unknown pinconf property: {}", n);
                }
            }
        }

        Self {
            id,
            pull,
            drive,
            mux: Iomux::from_bits_truncate(mux as _),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pull_values() {
        assert_eq!(Pull::Disabled as u32, 0);
        assert_eq!(Pull::PullUp as u32, 3);
        assert_eq!(Pull::PullDown as u32, 4);
    }
}
