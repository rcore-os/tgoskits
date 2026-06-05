use alloc::{boxed::Box, string::String, vec::Vec};

use ax_errno::AxError;
use rdif_vsock::Interface;
use rdrive::{Device, DriverGeneric};

pub struct PlatformVsockDevice {
    name: String,
    vsock: Option<Box<dyn Interface>>,
}

impl PlatformVsockDevice {
    fn new(name: String, vsock: Box<dyn Interface>) -> Self {
        Self {
            name,
            vsock: Some(vsock),
        }
    }
}

impl DriverGeneric for PlatformVsockDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

pub trait PlatformDeviceVsock {
    fn register_vsock<T>(self, dev: T)
    where
        T: Interface + 'static;
}

impl PlatformDeviceVsock for rdrive::PlatformDevice {
    fn register_vsock<T>(self, dev: T)
    where
        T: Interface + 'static,
    {
        let name = dev.name().into();
        self.register(PlatformVsockDevice::new(name, Box::new(dev)));
    }
}

pub fn take_vsock_devices() -> Result<Vec<Box<dyn Interface>>, AxError> {
    let mut devices = Vec::new();
    for dev in rdrive::get_list::<PlatformVsockDevice>() {
        devices.push(take_vsock_device(dev)?);
    }
    Ok(devices)
}

fn take_vsock_device(device: Device<PlatformVsockDevice>) -> Result<Box<dyn Interface>, AxError> {
    let mut device = device.lock().map_err(|_| AxError::BadState)?;
    device.vsock.take().ok_or(AxError::BadState)
}
