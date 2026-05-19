extern crate alloc;

use alloc::{boxed::Box, string::String, vec::Vec};

use ax_driver_base::DevError;
use ax_driver_display::DisplayDriverOps;
use rdif_display::{DisplayError, DisplayInfo, FrameBuffer, Interface, PixelFormat};
use rdrive::{Device, DriverGeneric, PlatformDevice};

pub struct DisplayDevice {
    name: String,
    display: Option<Box<dyn Interface>>,
}

impl DisplayDevice {
    fn new(name: String, display: Box<dyn Interface>) -> Self {
        Self {
            name,
            display: Some(display),
        }
    }
}

impl DriverGeneric for DisplayDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

pub trait PlatformDeviceDisplay {
    fn register_display<T>(self, dev: T)
    where
        T: Interface + 'static;
}

impl PlatformDeviceDisplay for PlatformDevice {
    fn register_display<T>(self, dev: T)
    where
        T: Interface + 'static,
    {
        let name = dev.name().into();
        self.register(DisplayDevice::new(name, Box::new(dev)));
    }
}

pub fn register_legacy_display<D>(plat_dev: PlatformDevice, driver: D)
where
    D: DisplayDriverOps + 'static,
{
    plat_dev.register_display(LegacyDisplayDevice { driver });
}

pub fn take_display_devices() -> Result<Vec<Box<dyn Interface>>, axklib::AxError> {
    let mut devices = Vec::new();
    for dev in rdrive::get_list::<DisplayDevice>() {
        devices.push(take_display_device(dev)?);
    }
    Ok(devices)
}

fn take_display_device(
    device: Device<DisplayDevice>,
) -> Result<Box<dyn Interface>, axklib::AxError> {
    let mut device = device.lock().map_err(|_| axklib::AxError::BadState)?;
    device.display.take().ok_or(axklib::AxError::BadState)
}

struct LegacyDisplayDevice<D> {
    driver: D,
}

impl<D: DisplayDriverOps + 'static> DriverGeneric for LegacyDisplayDevice<D> {
    fn name(&self) -> &str {
        self.driver.device_name()
    }
}

impl<D: DisplayDriverOps + 'static> Interface for LegacyDisplayDevice<D> {
    fn info(&self) -> DisplayInfo {
        let info = self.driver.info();
        let stride = if info.height == 0 {
            0
        } else {
            info.fb_size / info.height as usize
        };
        DisplayInfo {
            width: info.width,
            height: info.height,
            stride,
            format: PixelFormat::Xrgb8888,
            fb_size: info.fb_size,
        }
    }

    fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError> {
        let info = self.driver.info();
        if info.fb_base_vaddr == 0 || info.fb_size == 0 {
            return Err(DisplayError::InvalidFramebuffer);
        }
        Ok(unsafe { FrameBuffer::from_raw_parts_mut(info.fb_base_vaddr as *mut u8, info.fb_size) })
    }

    fn need_flush(&self) -> bool {
        self.driver.need_flush()
    }

    fn flush(&mut self) -> Result<(), DisplayError> {
        self.driver.flush().map_err(map_display_error)
    }
}

fn map_display_error(err: DevError) -> DisplayError {
    match err {
        DevError::Unsupported => DisplayError::NotSupported,
        DevError::Again | DevError::ResourceBusy | DevError::BadState => DisplayError::NotAvailable,
        _ => DisplayError::Other(Box::new(rdif_display::KError::Unknown(
            "legacy display error",
        ))),
    }
}
