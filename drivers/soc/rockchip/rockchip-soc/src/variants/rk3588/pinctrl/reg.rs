use core::ptr::NonNull;

use super::super::syscon::IocBase;
use crate::{
    Mmio, PinId, PinctrlResult, Pull,
    pinctrl::{Iomux, PinctrlError, gpio::IomuxReg},
};

pub(crate) struct PinctrlReg {
    /// IOC 基地址
    ioc_base: NonNull<u8>,
}

unsafe impl Send for PinctrlReg {}

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
        let mut reg = reg.offset;
        let mut data;

        if pin % 8 >= 4 {
            reg += 0x4; // 每组寄存器占用 8 字节，后4个引脚在高4字节
        }

        let bit = (pin % 4) * 4;
        let mask = 0xfu32;

        if id.bank().raw() == 0 {
            if (12..=31).contains(&pin) {
                if mux < 8 {
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
                    data |= 8 << bit;
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
        let value = (pull as u32) << bit_offset;

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
        // 每个 drive 配置占 8 位（但实际只使用低 2 位）
        let mask = 0x3u32 << bit_offset;
        let value = drive << bit_offset;

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
        let mut reg = reg.offset; // 组内偏移

        if pin % 8 >= 4 {
            reg += 0x4; // 每组寄存器占用 8 字节，后4个引脚在高4字节
        }

        let bit = (pin % 4) * 4;
        let mask = 0xfu32;

        // GPIO0 的特殊处理
        if id.bank().raw() == 0 {
            // GPIO0: 直接使用组内偏移（不加基地址）
            let reg_value = unsafe {
                let reg_ptr = self.ioc_base.as_ptr().add(reg) as *const u32;
                reg_ptr.read_volatile()
            };

            debug!(
                "get_mux: pin={id}, reg_offset={:#x}, bit={}, reg_value={:#x}",
                reg, bit, reg_value
            );

            let func_num = (reg_value & (mask << bit)) >> bit;
            return Iomux::from_bits(func_num as u8).ok_or(PinctrlError::InvalidConfig);
        } else {
            // GPIO1-4: 加上 BUS_IOC 基地址
            reg += IocBase::Bus.offset();
        }
        let reg_ptr = unsafe { self.ioc_base.as_ptr().add(reg) as *const u32 };
        // 读取寄存器值
        let reg_value = unsafe { reg_ptr.read_volatile() };

        debug!(
            "get_mux: pin={id}, reg: {reg_ptr:#p} reg_offset={:#x}, bit={}, reg_value={:#x}",
            reg, bit, reg_value
        );

        // 提取功能配置字段（每个引脚占 4 位）
        let func_num = (reg_value & (mask << bit)) >> bit;
        Iomux::from_bits(func_num as u8).ok_or(PinctrlError::InvalidConfig)
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

        // 转换为 Pull 枚举
        match pull_value {
            0 => Ok(Pull::Disabled),
            1 => Ok(Pull::PullUp),
            2 => Ok(Pull::PullDown),
            _ => {
                log::warn!("Invalid pull value {} for pin {}", pull_value, pin.raw());
                Err(PinctrlError::InvalidConfig)
            }
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

        // 提取 drive 配置字段（每个 drive 占 2 位）
        let mask = 0x3u32 << bit_offset;
        let drive_value = (reg_value & mask) >> bit_offset;

        debug!("get_drive: drive_value={}, mask={:#x}", drive_value, mask);

        Ok(drive_value)
    }
}
