//! RK3588 Pinctrl 模块
//!
//! 提供引脚复用和引脚配置功能。

use crate::{
    GpioDirection, Mmio, PinConfig, PinId,
    pinctrl::{Iomux, PinCtrlOp, PinctrlResult, gpio::GpioBank},
};

mod pinconf_regs;
mod reg;

use reg::*;

pub struct PinCtrl {
    /// Pinctrl 驱动（引脚功能配置）
    pinctrl: PinctrlReg,

    /// 5 个 GPIO Bank（GPIO 数据操作）
    gpio_banks: [GpioBank; 5],
}

unsafe impl Send for PinCtrl {}

impl PinCtrl {
    /// 创建新的 PinManager
    ///
    /// IOC 和 GPIO 寄存器地址必须有效且在生命周期内保持可访问
    ///
    /// 寄存器地址参考设备树：
    /// - IOC: 0xfd5f0000 (syscon@fd5f0000)
    /// - GPIO0-4: 0xfd8a0000, 0xfec20000, 0xfec30000, 0xfec40000, 0xfec50000
    pub fn new(ioc: Mmio, gpio: &[Mmio]) -> Self {
        if gpio.len() != 5 {
            panic!("RK3588 PinCtrl requires 5 GPIO banks");
        }

        let iomux = [Iomux::WIDTH_4BIT; 4];
        Self {
            pinctrl: unsafe { PinctrlReg::new(ioc) },
            gpio_banks: [
                GpioBank::new(gpio[0], iomux), // GPIO0 (Pin 0-31) - PMU1_IOC
                GpioBank::new(gpio[1], iomux), // GPIO1 (Pin 32-63) - BUS_IOC
                GpioBank::new(gpio[2], iomux), // GPIO2 (Pin 64-95) - BUS_IOC
                GpioBank::new(gpio[3], iomux), // GPIO3 (Pin 96-127) - BUS_IOC
                GpioBank::new(gpio[4], iomux), // GPIO4 (Pin 128-159) - BUS_IOC
            ],
        }
    }

    /// 读取 GPIO 引脚值
    ///
    /// 引脚必须已配置为 GPIO 功能。
    ///
    /// # 参数
    ///
    /// * `pin` - 引脚 ID
    ///
    /// # 返回
    ///
    /// 引脚电平状态（true = 高电平，false = 低电平）
    pub fn read_gpio(&self, pin: PinId) -> PinctrlResult<bool> {
        let bank_id = pin.bank().raw() as usize;
        self.gpio_banks[bank_id].read(pin)
    }

    /// 写入 GPIO 引脚值
    ///
    /// 引脚必须已配置为 GPIO 输出功能。
    ///
    /// # 参数
    ///
    /// * `pin` - 引脚 ID
    /// * `value` - 输出值（true = 高电平，false = 低电平）
    pub fn write_gpio(&self, pin: PinId, value: bool) -> PinctrlResult<()> {
        let bank_id = pin.bank().raw() as usize;
        self.gpio_banks[bank_id].write(pin, value)
    }

    fn bank(&self, pin: PinId) -> &GpioBank {
        &self.gpio_banks[pin.bank().raw() as usize]
    }

    fn set_mux(&self, config: &PinConfig) -> PinctrlResult<()> {
        self.bank(config.id).verify_mux(config.id, config.mux)?;
        if self.bank(config.id).iomux_gpio_only(config.id) {
            return Ok(());
        }

        let iomux_reg = self.bank(config.id).iomux[config.id.pin_in_bank() as usize / 8];

        self.pinctrl.set_mux(config.id, config.mux, iomux_reg)?;

        Ok(())
    }

    pub fn set_config(&mut self, config: PinConfig) -> PinctrlResult<()> {
        debug!("set_config: {:?}", config);
        self.set_mux(&config)?;
        self.pinctrl.set_pull(config.id, config.pull)?;

        if let Some(drive) = config.drive {
            self.pinctrl.set_drive(config.id, drive)?;
        }

        Ok(())
    }

    pub fn get_config(&self, pin: PinId) -> PinctrlResult<PinConfig> {
        // 获取 IomuxReg（组内偏移）
        let iomux_reg = self.bank(pin).iomux[pin.pin_in_bank() as usize / 8];

        let function = self.pinctrl.get_mux(pin, iomux_reg)?;

        let pull = self.pinctrl.get_pull(pin)?;

        let drive = self.pinctrl.get_drive(pin)?;

        Ok(PinConfig {
            id: pin,
            mux: function,
            pull,
            drive: Some(drive),
        })
    }

    pub fn gpio_direction(&self, pin: PinId) -> PinctrlResult<GpioDirection> {
        self.bank(pin).get_direction(pin)
    }

    pub fn set_gpio_direction(&self, pin: PinId, direction: GpioDirection) -> PinctrlResult<()> {
        self.bank(pin).set_direction(pin, direction)
    }
}

impl PinCtrlOp for PinCtrl {
    fn set_config(&mut self, config: PinConfig) -> PinctrlResult<()> {
        self.set_config(config)
    }

    fn get_config(&self, pin: PinId) -> PinctrlResult<PinConfig> {
        self.get_config(pin)
    }

    fn gpio_direction(&self, pin: PinId) -> PinctrlResult<GpioDirection> {
        self.gpio_direction(pin)
    }

    fn set_gpio_direction(&self, pin: PinId, direction: GpioDirection) -> PinctrlResult<()> {
        self.set_gpio_direction(pin, direction)
    }

    fn read_gpio(&self, pin: PinId) -> PinctrlResult<bool> {
        self.read_gpio(pin)
    }

    fn write_gpio(&self, pin: PinId, value: bool) -> PinctrlResult<()> {
        self.write_gpio(pin, value)
    }
}
