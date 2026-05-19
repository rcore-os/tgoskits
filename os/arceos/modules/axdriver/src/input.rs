extern crate alloc;

use alloc::{boxed::Box, string::String, vec::Vec};

use ax_driver_base::DevError;
use ax_driver_input::InputDriverOps;
use rdif_input::{AbsInfo, EventType, InputDeviceId, InputError, InputEvent, Interface};
use rdrive::{Device, DriverGeneric, PlatformDevice};

pub struct InputDevice {
    name: String,
    input: Option<Box<dyn Interface>>,
}

impl InputDevice {
    fn new(name: String, input: Box<dyn Interface>) -> Self {
        Self {
            name,
            input: Some(input),
        }
    }
}

impl DriverGeneric for InputDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

pub trait PlatformDeviceInput {
    fn register_input<T>(self, dev: T)
    where
        T: Interface + 'static;
}

impl PlatformDeviceInput for PlatformDevice {
    fn register_input<T>(self, dev: T)
    where
        T: Interface + 'static,
    {
        let name = dev.name().into();
        self.register(InputDevice::new(name, Box::new(dev)));
    }
}

pub fn register_legacy_input<D>(plat_dev: PlatformDevice, driver: D)
where
    D: InputDriverOps + 'static,
{
    plat_dev.register_input(LegacyInputDevice { driver });
}

pub fn take_input_devices() -> Result<Vec<Box<dyn Interface>>, axklib::AxError> {
    let mut devices = Vec::new();
    for dev in rdrive::get_list::<InputDevice>() {
        devices.push(take_input_device(dev)?);
    }
    Ok(devices)
}

fn take_input_device(device: Device<InputDevice>) -> Result<Box<dyn Interface>, axklib::AxError> {
    let mut device = device.lock().map_err(|_| axklib::AxError::BadState)?;
    device.input.take().ok_or(axklib::AxError::BadState)
}

struct LegacyInputDevice<D> {
    driver: D,
}

impl<D: InputDriverOps + 'static> DriverGeneric for LegacyInputDevice<D> {
    fn name(&self) -> &str {
        self.driver.device_name()
    }
}

impl<D: InputDriverOps + 'static> Interface for LegacyInputDevice<D> {
    fn device_id(&self) -> InputDeviceId {
        let id = self.driver.device_id();
        InputDeviceId {
            bus_type: id.bus_type,
            vendor: id.vendor,
            product: id.product,
            version: id.version,
        }
    }

    fn physical_location(&self) -> &str {
        self.driver.physical_location()
    }

    fn unique_id(&self) -> &str {
        self.driver.unique_id()
    }

    fn get_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> Result<bool, InputError> {
        let ty = map_event_type(ty)?;
        self.driver.get_event_bits(ty, out).map_err(map_input_error)
    }

    fn read_event(&mut self) -> Result<InputEvent, InputError> {
        let event = self.driver.read_event().map_err(map_input_error)?;
        Ok(InputEvent {
            event_type: event.event_type,
            code: event.code,
            value: event.value as i32,
        })
    }

    fn get_prop_bits(&mut self, out: &mut [u8]) -> Result<usize, InputError> {
        self.driver.get_prop_bits(out).map_err(map_input_error)
    }

    fn get_abs_info(&mut self, axis: u8) -> Result<AbsInfo, InputError> {
        let info = self.driver.get_abs_info(axis).map_err(map_input_error)?;
        Ok(AbsInfo {
            min: info.min as i32,
            max: info.max as i32,
            fuzz: info.fuzz as i32,
            flat: info.flat as i32,
            res: info.res as i32,
        })
    }
}

fn map_event_type(ty: EventType) -> Result<ax_driver_input::EventType, InputError> {
    ax_driver_input::EventType::from_repr(ty as u8).ok_or(InputError::NotSupported)
}

fn map_input_error(err: DevError) -> InputError {
    match err {
        DevError::Again => InputError::Again,
        DevError::Unsupported => InputError::NotSupported,
        DevError::BadState | DevError::ResourceBusy => InputError::NotAvailable,
        _ => InputError::Other(Box::new(rdif_input::KError::Unknown("legacy input error"))),
    }
}
