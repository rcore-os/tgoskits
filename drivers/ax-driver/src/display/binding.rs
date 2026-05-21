use alloc::{boxed::Box, string::String, vec::Vec};

use ax_errno::AxError;
use rdif_display::Interface;
use rdrive::{Device, DriverGeneric};

pub struct PlatformDisplayDevice {
    name: String,
    display: Option<Box<dyn Interface>>,
}

impl PlatformDisplayDevice {
    fn new(name: String, display: Box<dyn Interface>) -> Self {
        Self {
            name,
            display: Some(display),
        }
    }
}

impl DriverGeneric for PlatformDisplayDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

pub trait PlatformDeviceDisplay {
    fn register_display<T>(self, dev: T)
    where
        T: Interface + 'static;
}

impl PlatformDeviceDisplay for rdrive::PlatformDevice {
    fn register_display<T>(self, dev: T)
    where
        T: Interface + 'static,
    {
        let name = dev.name().into();
        self.register(PlatformDisplayDevice::new(name, Box::new(dev)));
    }
}

pub fn take_display_devices() -> Result<Vec<Box<dyn Interface>>, AxError> {
    let mut devices = Vec::new();
    for dev in rdrive::get_list::<PlatformDisplayDevice>() {
        let display = take_display_device(dev)?;
        devices.push(display);
    }
    Ok(devices)
}

fn take_display_device(
    device: Device<PlatformDisplayDevice>,
) -> Result<Box<dyn Interface>, AxError> {
    let mut device = device.lock().map_err(|_| AxError::BadState)?;
    device.display.take().ok_or(AxError::BadState)
}
