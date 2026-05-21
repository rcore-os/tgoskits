use alloc::{boxed::Box, string::String, vec::Vec};

use ax_errno::AxError;
use rdif_input::Interface;
use rdrive::{Device, DriverGeneric};

pub struct PlatformInputDevice {
    name: String,
    input: Option<Box<dyn Interface>>,
}

impl PlatformInputDevice {
    fn new(name: String, input: Box<dyn Interface>) -> Self {
        Self {
            name,
            input: Some(input),
        }
    }
}

impl DriverGeneric for PlatformInputDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

pub trait PlatformDeviceInput {
    fn register_input<T>(self, dev: T)
    where
        T: Interface + 'static;
}

impl PlatformDeviceInput for rdrive::PlatformDevice {
    fn register_input<T>(self, dev: T)
    where
        T: Interface + 'static,
    {
        let name = dev.name().into();
        self.register(PlatformInputDevice::new(name, Box::new(dev)));
    }
}

pub fn take_input_devices() -> Result<Vec<Box<dyn Interface>>, AxError> {
    let mut devices = Vec::new();
    for dev in rdrive::get_list::<PlatformInputDevice>() {
        devices.push(take_input_device(dev)?);
    }
    Ok(devices)
}

fn take_input_device(device: Device<PlatformInputDevice>) -> Result<Box<dyn Interface>, AxError> {
    let mut device = device.lock().map_err(|_| AxError::BadState)?;
    device.input.take().ok_or(AxError::BadState)
}
