use crate::{
    GpioDirection, Mmio, PinId, PinctrlResult,
    pinctrl::{Iomux, PinctrlError},
};

mod reg;

use reg::*;
use tock_registers::interfaces::{Readable, Writeable};

#[derive(Debug, Clone, Copy)]
pub(crate) struct IomuxReg {
    pub ty: Iomux,
    pub offset: usize,
}

pub struct GpioBank {
    base: usize,
    pub(crate) iomux: [IomuxReg; 4],
}

impl GpioBank {
    pub fn new(base: Mmio, iomux: [Iomux; 4]) -> Self {
        let iomux_regs: [IomuxReg; 4] = core::array::from_fn(|i| {
            let ty = iomux[i];
            let offset = i * if ty.contains(Iomux::WIDTH_4BIT)
                || ty.contains(Iomux::WIDTH_3BIT)
                || ty.contains(Iomux::WIDTH_8_2BIT)
            {
                8
            } else {
                4
            };
            IomuxReg { ty, offset }
        });

        GpioBank {
            base: base.as_ptr() as usize,
            iomux: iomux_regs,
        }
    }

    fn reg(&self) -> &Registers {
        unsafe { &*(self.base as *const Registers) }
    }

    pub fn verify_mux(&self, pin: PinId, mux: Iomux) -> PinctrlResult<()> {
        let pin_in_bank = pin.pin_in_bank();
        if pin_in_bank >= 32 {
            return Err(PinctrlError::InvalidPinId(pin));
        }
        let iomux_num = pin_in_bank / 8;

        if self.iomux[iomux_num as usize].ty.contains(Iomux::UNROUTED) {
            debug!("verify_mux: pin {:?} does not support routing", pin);
            return Err(PinctrlError::Unsupported);
        }

        if self.iomux[iomux_num as usize].ty.contains(Iomux::GPIO_ONLY) && mux != Iomux::GPIO_ONLY {
            debug!("verify_mux: pin {:?} only supports GPIO function", pin);
            return Err(PinctrlError::Unsupported);
        }

        Ok(())
    }

    pub fn iomux_gpio_only(&self, pin: PinId) -> bool {
        let iomux_num = pin.pin_in_bank() / 8;
        self.iomux[iomux_num as usize].ty.contains(Iomux::GPIO_ONLY)
    }

    /// 设置引脚方向（统一接口）
    ///
    /// # 参数
    ///
    /// * `pin_in_bank` - Bank 内的引脚编号 (0-31)
    /// * `config` - 方向配置（输入或输出带初始值）
    ///
    /// # 示例
    ///
    /// ```ignore
    /// bank.set_direction(5, DirectionConfig::Input)?;
    /// bank.set_direction(5, DirectionConfig::Output(true))?;  // 输出，初始值 HIGH
    /// ```
    pub fn set_direction(&self, pin: PinId, direction: GpioDirection) -> PinctrlResult<()> {
        match direction {
            GpioDirection::Input => self.set_direction_input(pin),
            GpioDirection::Output(value) => self.set_direction_output(pin, value),
        }
    }

    /// 设置引脚为输入方向
    ///
    /// # 参数
    ///
    /// * `pin_in_bank` - Bank 内的引脚编号 (0-31)
    #[inline]
    pub fn set_direction_input(&self, pin: PinId) -> PinctrlResult<()> {
        let pin_in_bank = pin.pin_in_bank();
        if pin_in_bank >= 32 {
            return Err(PinctrlError::InvalidPinId(pin));
        }
        set_bit(
            &self.reg().swport_ddr_l,
            &self.reg().swport_ddr_h,
            pin_in_bank,
            false,
        );

        Ok(())
    }

    /// 设置引脚为输出方向并设置初始值
    ///
    /// # 参数
    ///
    /// * `pin_in_bank` - Bank 内的引脚编号 (0-31)
    /// * `value` - 初始输出值
    #[inline]
    pub fn set_direction_output(&self, pin: PinId, value: bool) -> PinctrlResult<()> {
        let pin_in_bank = pin.pin_in_bank();
        if pin_in_bank >= 32 {
            return Err(PinctrlError::InvalidPinId(pin));
        }

        set_bit(
            &self.reg().swport_dr_l,
            &self.reg().swport_dr_h,
            pin_in_bank,
            value,
        );

        set_bit(
            &self.reg().swport_ddr_l,
            &self.reg().swport_ddr_h,
            pin_in_bank,
            true,
        );

        Ok(())
    }

    /// 读取引脚值
    ///
    /// # 参数
    ///
    /// * `pin_in_bank` - Bank 内的引脚编号 (0-31)
    pub fn read(&self, pin: PinId) -> PinctrlResult<bool> {
        let pin_in_bank = pin.pin_in_bank();
        if pin_in_bank >= 32 {
            return Err(PinctrlError::InvalidPinId(pin));
        }
        let value = read_bit(
            &self.reg().swport_dr_l,
            &self.reg().swport_dr_h,
            pin_in_bank,
        );
        Ok(value)
    }

    /// 写入引脚值
    ///
    /// 引脚必须已配置为输出方向。
    ///
    /// # 参数
    ///
    /// * `pin_in_bank` - Bank 内的引脚编号 (0-31)
    /// * `value` - 输出值
    pub fn write(&self, pin: PinId, value: bool) -> PinctrlResult<()> {
        let pin_in_bank = pin.pin_in_bank();
        if pin_in_bank >= 32 {
            return Err(PinctrlError::InvalidPinId(pin));
        }

        set_bit(
            &self.reg().swport_dr_l,
            &self.reg().swport_dr_h,
            pin_in_bank,
            value,
        );

        Ok(())
    }

    /// 获取引脚方向配置
    ///
    /// 如果引脚配置为输出，同时返回当前输出值。
    ///
    /// # 参数
    ///
    /// * `pin_in_bank` - Bank 内的引脚编号 (0-31)
    ///
    /// # 返回
    ///
    /// 返回 `DirectionConfig`：
    /// - `Input` - 引脚配置为输入
    /// - `Output(value)` - 引脚配置为输出，value 为当前输出值
    pub fn get_direction(&self, pin: PinId) -> PinctrlResult<GpioDirection> {
        let pin_in_bank = pin.pin_in_bank();
        if pin_in_bank >= 32 {
            return Err(PinctrlError::InvalidPinId(pin));
        }

        if read_bit(
            &self.reg().swport_ddr_l,
            &self.reg().swport_ddr_h,
            pin_in_bank,
        ) {
            // 输出方向：同时读取输出值
            let dr_value = read_bit(
                &self.reg().swport_dr_l,
                &self.reg().swport_dr_h,
                pin_in_bank,
            );
            Ok(GpioDirection::Output(dr_value))
        } else {
            // 输入方向
            Ok(GpioDirection::Input)
        }
    }
}

fn read_value(reg_l: &impl Readable<T = u32>, reg_h: &impl Readable<T = u32>) -> u32 {
    reg_l.get() & 0xffff | (reg_h.get() & 0xffff) << 16
}

fn write_bit(reg_l: &impl Writeable<T = u32>, reg_h: &impl Writeable<T = u32>, value: u32) {
    reg_l.set(((value) & 0xFFFF) | 0xFFFF0000);
    reg_h.set((((value) & 0xFFFF0000) >> 16) | 0xFFFF0000);
}

fn read_bit(
    reg_l: &impl Readable<T = u32>,
    reg_h: &impl Readable<T = u32>,
    pin_in_bank: u32,
) -> bool {
    read_value(reg_l, reg_h) & (1 << pin_in_bank) != 0
}

fn set_bit<V>(reg_l: &V, reg_h: &V, pin_in_bank: u32, value: bool)
where
    V: Readable<T = u32> + Writeable<T = u32>,
{
    let mut current = read_value(reg_l, reg_h);
    if value {
        current |= 1 << pin_in_bank;
    } else {
        current &= !(1 << pin_in_bank);
    }
    write_bit(reg_l, reg_h, current);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iomux_offset_gpio0() {
        // GPIO0 的 iomux 只存储组内偏移，由 Pinctrl::set_mux() 加上 PMU1_IOC 基地址
        let base = unsafe { Mmio::new_unchecked(0xfd8a0000 as *mut u8) };
        let iomux = [Iomux::WIDTH_4BIT; 4];
        let bank = GpioBank::new(base, iomux);

        // 验证组内偏移（不包含基地址）
        assert_eq!(bank.iomux[0].offset, 0x00);
        assert_eq!(bank.iomux[1].offset, 0x08);
        assert_eq!(bank.iomux[2].offset, 0x10);
        assert_eq!(bank.iomux[3].offset, 0x18);
    }

    #[test]
    fn test_iomux_offset_gpio1() {
        // GPIO1-4 的 iomux 只存储组内偏移，由 Pinctrl::set_mux() 加上 BUS_IOC 基地址 (0x8000)
        let base = unsafe { Mmio::new_unchecked(0xfec20000 as *mut u8) };
        let iomux = [Iomux::WIDTH_4BIT; 4];
        let bank = GpioBank::new(base, iomux);

        // 验证组内偏移（不包含基地址）
        assert_eq!(bank.iomux[0].offset, 0x00);
        assert_eq!(bank.iomux[1].offset, 0x08);
        assert_eq!(bank.iomux[2].offset, 0x10);
        assert_eq!(bank.iomux[3].offset, 0x18);
    }

    #[test]
    fn test_offset_increment() {
        // 验证每个 iomux 组占用 8 字节
        let base = unsafe { Mmio::new_unchecked(0xfec20000 as *mut u8) };
        let iomux = [Iomux::WIDTH_4BIT; 4];
        let bank = GpioBank::new(base, iomux);

        assert_eq!(bank.iomux[1].offset - bank.iomux[0].offset, 0x8);
        assert_eq!(bank.iomux[2].offset - bank.iomux[1].offset, 0x8);
        assert_eq!(bank.iomux[3].offset - bank.iomux[2].offset, 0x8);
    }
}
