use core::ptr::NonNull;

use super::super::syscon::IocBase;
use crate::{
    Mmio, PinId, PinctrlResult, Pull,
    pinctrl::{Iomux, PinctrlError, gpio::IomuxReg},
};

const RK3588_GPIO0_HIGH_MUX_SELECTOR: u32 = 8;

pub(crate) struct PinctrlReg {
    /// IOC 基地址
    ioc_base: NonNull<u8>,
}

unsafe impl Send for PinctrlReg {}

fn rk3588_pull_to_reg_value(pull: Pull) -> Option<u32> {
    match pull {
        Pull::Disabled => Some(0),
        Pull::PullDown => Some(1),
        Pull::PullUp => Some(3),
        Pull::BusHold | Pull::PullPinDefault => None,
    }
}

fn rk3588_reg_value_to_pull(value: u32) -> Option<Pull> {
    match value {
        0 | 2 => Some(Pull::Disabled),
        1 => Some(Pull::PullDown),
        3 => Some(Pull::PullUp),
        _ => None,
    }
}

impl PinctrlReg {
    /// 创建新的 pinctrl 实例
    ///
    /// # 参数
    ///
    /// * `ioc_base` - IOC 寄存器基地址
    ///
    /// # Safety
    ///
    /// `ioc_base` 必须是有效的 IOC 寄存器基地址，并且在整个生命周期内保持有效。
    pub unsafe fn new(ioc_base: Mmio) -> Self {
        Self { ioc_base }
    }

    /// 设置引脚功能（pinmux）
    ///
    /// 配置引脚的复用功能（GPIO、UART、SPI 等）。
    ///
    /// # 参数
    ///
    /// * `pin` - 引脚 ID
    /// * `function` - 引脚功能
    ///
    /// # 参考
    ///
    /// u-boot: `drivers/pinctrl/rockchip/pinctrl-rk3588.c:rk3588_set_mux()`
    pub(crate) fn set_mux(&self, id: PinId, mux: Iomux, reg: IomuxReg) -> PinctrlResult<()> {
        let mux = mux.bits() as u32;
        let pin = id.pin_in_bank();
        let mut reg = iomux_reg_offset(id, reg.offset);
        let mut data;

        let bit = mux_bit(pin);
        let mask = mux_mask();

        if id.bank().raw() == 0 {
            if (12..=31).contains(&pin) {
                if mux < RK3588_GPIO0_HIGH_MUX_SELECTOR {
                    // 写 PMU2_IOC 寄存器（带 mux 值）
                    let reg0 = reg + IocBase::Pmu2.offset() - 0xC;
                    data = mask << (bit + 16);
                    data |= mux << bit;

                    unsafe {
                        let reg_ptr = self.ioc_base.as_ptr().add(reg0) as *mut u32;
                        reg_ptr.write_volatile(data);
                    }

                    // 写 BUS_IOC 寄存器（只写掩码，不写 mux 值）
                    // 参考 u-boot: drivers/pinctrl/rockchip/pinctrl-rk3588.c:58-60
                    let reg1 = reg + IocBase::Bus.offset();
                    data = mask << (bit + 16);

                    unsafe {
                        let reg_ptr = self.ioc_base.as_ptr().add(reg1) as *mut u32;
                        reg_ptr.write_volatile(data);
                    }
                } else {
                    let reg0 = reg + IocBase::Pmu2.offset() - 0xC;
                    data = mask << (bit + 16);
                    data |= RK3588_GPIO0_HIGH_MUX_SELECTOR << bit;
                    unsafe {
                        let reg_ptr = self.ioc_base.as_ptr().add(reg0) as *mut u32;
                        reg_ptr.write_volatile(data);
                    }

                    let reg1 = reg + IocBase::Bus.offset();
                    data = mask << (bit + 16);
                    data |= mux << bit;
                    unsafe {
                        let reg_ptr = self.ioc_base.as_ptr().add(reg1) as *mut u32;
                        reg_ptr.write_volatile(data);
                    }
                }
            } else {
                data = mask << (bit + 16);
                data |= (mux & mask) << bit;

                unsafe {
                    let reg_ptr = self.ioc_base.as_ptr().add(reg) as *mut u32;
                    reg_ptr.write_volatile(data);
                }
            }
            return Ok(());
        } else {
            reg += IocBase::Bus.offset();
        }

        data = mask << (bit + 16);
        data |= (mux & mask) << bit;

        unsafe {
            let reg_ptr = self.ioc_base.as_ptr().add(reg) as *mut u32;
            reg_ptr.write_volatile(data);
        }

        Ok(())
    }

    /// 设置 pull 配置
    ///
    /// 配置引脚的上下拉电阻。
    ///
    /// # 参数
    ///
    /// * `pin` - 引脚 ID
    /// * `pull` - 上下拉配置
    ///
    /// # 参考
    ///
    /// u-boot: `drivers/pinctrl/rockchip/pinctrl-rk3588.c:rk3588_set_pull()`
    pub fn set_pull(&self, pin: PinId, pull: Pull) -> PinctrlResult<()> {
        use crate::variants::rk3588::pinctrl::pinconf_regs::find_pull_entry;

        let (reg_offset, bit_offset) =
            find_pull_entry(pin).ok_or(PinctrlError::InvalidPinId(pin))?;

        // Rockchip 写掩码机制
        // 每个 pull 配置占 2 位，掩码为 0x3
        let mask = 0x3u32 << bit_offset;
        let value =
            rk3588_pull_to_reg_value(pull).ok_or(PinctrlError::InvalidConfig)? << bit_offset;

        unsafe {
            let reg_ptr = self.ioc_base.as_ptr().add(reg_offset) as *mut u32;
            reg_ptr.write_volatile((mask << 16) | value);
        }

        Ok(())
    }

    /// 设置 drive strength
    ///
    /// 配置引脚输出驱动强度。
    ///
    /// # 参数
    ///
    /// * `pin` - 引脚 ID
    /// * `drive` - 驱动强度配置
    ///
    /// # 参考
    ///
    /// u-boot: `drivers/pinctrl/rockchip/pinctrl-rk3588.c:rk3588_set_drive()`
    pub fn set_drive(&self, pin: PinId, drive: u32) -> PinctrlResult<()> {
        use crate::variants::rk3588::pinctrl::pinconf_regs::find_drive_entry;

        let (reg_offset, bit_offset) =
            find_drive_entry(pin).ok_or(PinctrlError::InvalidPinId(pin))?;

        // Rockchip 写掩码机制
        // 每个 drive 字段占 4 位
        let mask = drive_mask() << bit_offset;
        let value = (drive & drive_mask()) << bit_offset;

        unsafe {
            let reg_ptr = self.ioc_base.as_ptr().add(reg_offset) as *mut u32;
            reg_ptr.write_volatile((mask << 16) | value);
        }

        Ok(())
    }

    /// 读取引脚功能（pinmux）
    ///
    /// 读取引脚当前的复用功能配置。
    ///
    /// # 参数
    ///
    /// * `id` - 引脚 ID
    /// * `reg` - IOMUX 寄存器信息（组内偏移）
    ///
    /// # 返回
    ///
    /// 返回引脚当前的功能配置
    ///
    /// # 参考
    ///
    /// u-boot: `drivers/pinctrl/rockchip/pinctrl-rockchip-core.c:rockchip_get_mux()`
    pub(crate) fn get_mux(&self, id: PinId, reg: IomuxReg) -> PinctrlResult<Iomux> {
        let pin = id.pin_in_bank();
        let mut reg = iomux_reg_offset(id, reg.offset);

        let bit = mux_bit(pin);
        let mask = mux_mask();

        if id.bank().raw() == 0 {
            if let Some((pmu2_reg, bus_reg)) = gpio0_high_mux_regs(pin, reg) {
                let pmu2_value = self.read_u32(pmu2_reg);
                let pmu2_mux = (pmu2_value & (mask << bit)) >> bit;
                if pmu2_mux != RK3588_GPIO0_HIGH_MUX_SELECTOR {
                    return Iomux::from_bits(pmu2_mux as u8).ok_or(PinctrlError::InvalidConfig);
                }

                let bus_value = self.read_u32(bus_reg);
                let bus_mux = (bus_value & (mask << bit)) >> bit;
                return Iomux::from_bits(bus_mux as u8).ok_or(PinctrlError::InvalidConfig);
            }

            let reg_value = self.read_u32(reg);
            debug!("get_mux: pin={id}, reg_offset={reg:#x}, bit={bit}, reg_value={reg_value:#x}");

            let func_num = (reg_value & (mask << bit)) >> bit;
            return Iomux::from_bits(func_num as u8).ok_or(PinctrlError::InvalidConfig);
        } else {
            // GPIO1-4: 加上 BUS_IOC 基地址
            reg += IocBase::Bus.offset();
        }
        let reg_value = self.read_u32(reg);

        debug!("get_mux: pin={id}, reg_offset={reg:#x}, bit={bit}, reg_value={reg_value:#x}");

        // 提取功能配置字段（每个引脚占 4 位）
        let func_num = (reg_value & (mask << bit)) >> bit;
        Iomux::from_bits(func_num as u8).ok_or(PinctrlError::InvalidConfig)
    }

    fn read_u32(&self, offset: usize) -> u32 {
        unsafe { (self.ioc_base.as_ptr().add(offset) as *const u32).read_volatile() }
    }

    /// 读取 pull 配置
    ///
    /// 读取引脚当前的上下拉配置。
    ///
    /// # 参数
    ///
    /// * `pin` - 引脚 ID
    ///
    /// # 返回
    ///
    /// 返回引脚当前的上下拉配置
    pub fn get_pull(&self, pin: PinId) -> PinctrlResult<Pull> {
        use crate::variants::rk3588::pinctrl::pinconf_regs::find_pull_entry;

        let (reg_offset, bit_offset) =
            find_pull_entry(pin).ok_or(PinctrlError::InvalidPinId(pin))?;

        // 读取寄存器值
        let reg_value = unsafe {
            let reg_ptr = self.ioc_base.as_ptr().add(reg_offset) as *const u32;
            reg_ptr.read_volatile()
        };

        debug!(
            "get_pull: pin={}, reg_offset={:#x}, bit_offset={}, reg_value={:#x}",
            pin, reg_offset, bit_offset, reg_value
        );

        // 提取 pull 配置字段（每个 pull 占 2 位）
        let mask = 0x3u32 << bit_offset;
        let pull_value = (reg_value & mask) >> bit_offset;

        debug!("get_pull: pull_value={}, mask={:#x}", pull_value, mask);

        if let Some(pull) = rk3588_reg_value_to_pull(pull_value) {
            Ok(pull)
        } else {
            log::warn!("Invalid pull value {} for pin {}", pull_value, pin.raw());
            Err(PinctrlError::InvalidConfig)
        }
    }

    /// 读取 drive strength
    ///
    /// 读取引脚当前的驱动强度配置。
    ///
    /// # 参数
    ///
    /// * `pin` - 引脚 ID
    ///
    /// # 返回
    ///
    /// 返回引脚当前的驱动强度配置
    pub fn get_drive(&self, pin: PinId) -> PinctrlResult<u32> {
        use crate::variants::rk3588::pinctrl::pinconf_regs::find_drive_entry;

        let (reg_offset, bit_offset) =
            find_drive_entry(pin).ok_or(PinctrlError::InvalidPinId(pin))?;

        // 读取寄存器值
        let reg_value = unsafe {
            let reg_ptr = self.ioc_base.as_ptr().add(reg_offset) as *const u32;
            reg_ptr.read_volatile()
        };

        debug!(
            "get_drive: pin={}, reg_offset={:#x}, bit_offset={}, reg_value={:#x}",
            pin, reg_offset, bit_offset, reg_value
        );

        // 提取 drive 配置字段（每个 drive 占 4 位）
        let mask = drive_mask() << bit_offset;
        let drive_value = (reg_value & mask) >> bit_offset;

        debug!("get_drive: drive_value={}, mask={:#x}", drive_value, mask);

        Ok(drive_value)
    }
}

fn mux_mask() -> u32 {
    0xf
}

fn drive_mask() -> u32 {
    0xf
}

fn mux_bit(pin_in_bank: u32) -> u32 {
    (pin_in_bank % 4) * 4
}

fn gpio0_high_mux_regs(pin_in_bank: u32, iomux_reg: usize) -> Option<(usize, usize)> {
    // GPIO0_B4..GPIO0_D7 使用拆分 mux 路径：
    // PMU2_IOC 选择扩展功能范围，BUS_IOC 保存实际 mux 值。
    (12..=31).contains(&pin_in_bank).then_some((
        iomux_reg + IocBase::Pmu2.offset() - 0x0c,
        iomux_reg + IocBase::Bus.offset(),
    ))
}

fn iomux_reg_offset(id: PinId, bank_iomux_reg: usize) -> usize {
    let pin = id.pin_in_bank();
    // RK3588 的 IOMUX 偏移跨 bank 连续累加。每 8 个引脚一组，
    // 每组使用两个 32-bit 寄存器：GPIO0 从 0x00 开始，
    // GPIO1 从 0x20 开始，依此类推。
    let mut reg = id.bank().raw() as usize * 0x20 + bank_iomux_reg;
    if pin % 8 >= 4 {
        reg += 0x04;
    }
    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rk3588_pull_encoding_matches_linux_1v8_only_table() {
        assert_eq!(rk3588_pull_to_reg_value(Pull::Disabled), Some(0));
        assert_eq!(rk3588_pull_to_reg_value(Pull::PullDown), Some(1));
        assert_eq!(rk3588_pull_to_reg_value(Pull::PullUp), Some(3));
        assert_eq!(rk3588_pull_to_reg_value(Pull::BusHold), None);
        assert_eq!(rk3588_pull_to_reg_value(Pull::PullPinDefault), None);

        assert_eq!(rk3588_reg_value_to_pull(0), Some(Pull::Disabled));
        assert_eq!(rk3588_reg_value_to_pull(1), Some(Pull::PullDown));
        assert_eq!(rk3588_reg_value_to_pull(2), Some(Pull::Disabled));
        assert_eq!(rk3588_reg_value_to_pull(3), Some(Pull::PullUp));
    }

    #[test]
    fn gpio0_high_mux_uses_pmu2_selector_and_bus_mux_windows() {
        // GPIO0_B7 位于第二个 8-pin IOMUX 组的高半段，
        // 调用方加上高半段偏移后传入的 iomux_reg 为 0x0c。
        assert_eq!(
            gpio0_high_mux_regs(15, 0x0c),
            Some((IocBase::Pmu2.offset(), IocBase::Bus.offset() + 0x0c))
        );

        assert_eq!(
            gpio0_high_mux_regs(16, 0x10),
            Some((IocBase::Pmu2.offset() + 0x04, IocBase::Bus.offset() + 0x10))
        );
    }

    #[test]
    fn low_gpio0_pins_do_not_use_split_mux_windows() {
        assert_eq!(gpio0_high_mux_regs(11, 0x08), None);
    }

    #[test]
    fn iomux_offsets_follow_rk3588_bank_group_layout() {
        assert_eq!(iomux_reg_offset(PinId::new(0).unwrap(), 0), 0x00);
        assert_eq!(iomux_reg_offset(PinId::new(4).unwrap(), 0), 0x04);
        assert_eq!(iomux_reg_offset(PinId::new(8).unwrap(), 0x08), 0x08);
        assert_eq!(iomux_reg_offset(PinId::new(15).unwrap(), 0x08), 0x0c);
        assert_eq!(iomux_reg_offset(PinId::new(31).unwrap(), 0x18), 0x1c);

        assert_eq!(iomux_reg_offset(PinId::new(32).unwrap(), 0), 0x20);
        assert_eq!(iomux_reg_offset(PinId::new(63).unwrap(), 0x18), 0x3c);
        assert_eq!(iomux_reg_offset(PinId::new(64).unwrap(), 0), 0x40);
        assert_eq!(iomux_reg_offset(PinId::new(95).unwrap(), 0x18), 0x5c);
        assert_eq!(iomux_reg_offset(PinId::new(96).unwrap(), 0), 0x60);
        assert_eq!(iomux_reg_offset(PinId::new(127).unwrap(), 0x18), 0x7c);
        assert_eq!(iomux_reg_offset(PinId::new(128).unwrap(), 0), 0x80);
        assert_eq!(iomux_reg_offset(PinId::new(159).unwrap(), 0x18), 0x9c);
    }

    #[test]
    fn drive_fields_are_four_bits_wide() {
        assert_eq!(drive_mask(), 0xf);
    }
}
