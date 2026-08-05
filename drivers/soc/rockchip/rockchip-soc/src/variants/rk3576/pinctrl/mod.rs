mod reg;

use reg::PinctrlReg;

use crate::{
    GpioDirection, Mmio, PinConfig, PinId,
    pinctrl::{Iomux, PinCtrlOp, PinctrlResult, gpio::GpioBank},
};

pub struct PinCtrl {
    pinctrl: PinctrlReg,
    gpio_banks: [GpioBank; 5],
}

// SAFETY: Moving the driver does not change the validity or aliasing of its MMIO
// mappings. Register access synchronization remains the responsibility of the owner.
unsafe impl Send for PinCtrl {}

impl PinCtrl {
    /// Creates an RK3576 pin controller from mapped IOC and GPIO regions.
    ///
    /// The mappings must remain valid for the lifetime of the controller.
    pub fn new(ioc: Mmio, sys_grf: Option<Mmio>, gpio: &[Mmio]) -> Self {
        assert_eq!(gpio.len(), 5, "RK3576 PinCtrl requires 5 GPIO banks");

        let iomux = [Iomux::WIDTH_4BIT; 4];
        Self {
            // SAFETY: The platform probe supplies the mapped RK3576 IOC GRF and
            // optional SYS GRF regions and keeps them alive for the driver lifetime.
            pinctrl: unsafe { PinctrlReg::new(ioc, sys_grf) },
            gpio_banks: [
                GpioBank::new(gpio[0], iomux),
                GpioBank::new(gpio[1], iomux),
                GpioBank::new(gpio[2], iomux),
                GpioBank::new(gpio[3], iomux),
                GpioBank::new(gpio[4], iomux),
            ],
        }
    }

    fn bank(&self, pin: PinId) -> &GpioBank {
        &self.gpio_banks[pin.bank().raw() as usize]
    }

    fn set_mux(&self, config: &PinConfig) -> PinctrlResult<()> {
        self.bank(config.id).verify_mux(config.id, config.mux)?;
        self.pinctrl.set_mux(config.id, config.mux)
    }

    pub fn set_config(&mut self, config: PinConfig) -> PinctrlResult<()> {
        self.set_mux(&config)?;
        self.pinctrl.set_pull(config.id, config.pull)?;
        if let Some(drive) = config.drive {
            self.pinctrl.set_drive(config.id, drive)?;
        }
        Ok(())
    }

    pub fn set_pull(&mut self, pin: PinId, pull: crate::Pull) -> PinctrlResult<()> {
        self.pinctrl.set_pull(pin, pull)
    }

    pub fn set_drive(&mut self, pin: PinId, drive: u32) -> PinctrlResult<()> {
        self.pinctrl.set_drive(pin, drive)
    }

    pub fn get_config(&self, pin: PinId) -> PinctrlResult<PinConfig> {
        Ok(PinConfig {
            id: pin,
            mux: self.pinctrl.get_mux(pin)?,
            pull: self.pinctrl.get_pull(pin)?,
            drive: Some(self.pinctrl.get_drive(pin)?),
        })
    }

    pub fn gpio_direction(&self, pin: PinId) -> PinctrlResult<GpioDirection> {
        self.bank(pin).get_direction(pin)
    }

    pub fn set_gpio_direction(&self, pin: PinId, direction: GpioDirection) -> PinctrlResult<()> {
        self.bank(pin).set_direction(pin, direction)
    }

    pub fn read_gpio(&self, pin: PinId) -> PinctrlResult<bool> {
        self.bank(pin).read(pin)
    }

    pub fn write_gpio(&self, pin: PinId, value: bool) -> PinctrlResult<()> {
        self.bank(pin).write(pin, value)
    }
}

impl PinCtrlOp for PinCtrl {
    fn set_config(&mut self, config: PinConfig) -> PinctrlResult<()> {
        self.set_config(config)
    }

    fn set_pull(&mut self, pin: PinId, pull: crate::Pull) -> PinctrlResult<()> {
        self.set_pull(pin, pull)
    }

    fn set_drive(&mut self, pin: PinId, drive: u32) -> PinctrlResult<()> {
        self.set_drive(pin, drive)
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
