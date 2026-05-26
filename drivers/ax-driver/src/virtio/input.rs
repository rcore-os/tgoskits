extern crate alloc;

use alloc::{borrow::ToOwned, format, string::String};

use rdif_input::{AbsInfo, EventType, InputDeviceId, InputError, InputEvent};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(probe = "pci")]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    Error as VirtIoError,
    device::input::{InputConfigSelect, VirtIOInput},
    transport::Transport,
};

use crate::{input::PlatformDeviceInput, virtio::VirtIoHalImpl};

#[cfg(probe = "pci")]
crate::model_register!(
    name: "VirtIO Input",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

#[cfg(probe = "pci")]
fn probe_pci(
    endpoint: &mut rdrive::probe::pci::EndpointRc,
    plat_dev: PlatformDevice,
) -> Result<(), OnProbeError> {
    let transport = crate::pci::take_virtio_transport(endpoint, DeviceType::Input)?;
    register_transport(plat_dev, transport)
}

pub fn register_transport<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    let dev = VirtIoInputDevice::new(transport).map_err(|err| {
        OnProbeError::other(format!("failed to initialize virtio-input: {err:?}"))
    })?;
    plat_dev.register_input(dev);
    log::info!("registered virtio input device");
    Ok(())
}

struct VirtIoInputDevice<T: Transport + 'static> {
    raw: VirtIOInput<VirtIoHalImpl, T>,
    device_id: InputDeviceId,
    name: String,
    physical_location: String,
    unique_id: String,
}

unsafe impl<T: Transport + 'static> Send for VirtIoInputDevice<T> {}

impl<T: Transport + 'static> VirtIoInputDevice<T> {
    fn new(transport: T) -> Result<Self, VirtIoError> {
        let mut raw = VirtIOInput::new(transport)?;
        let name = raw.name().unwrap_or_else(|_| "<unknown>".to_owned());
        let id = raw.ids()?;
        let device_id = InputDeviceId {
            bus_type: id.bustype,
            vendor: id.vendor,
            product: id.product,
            version: id.version,
        };
        let physical_location = format!(
            "virtio-input/{:04x}:{:04x}:{:04x}:{:04x}",
            device_id.bus_type, device_id.vendor, device_id.product, device_id.version
        );
        let unique_id = format!(
            "virtio-{:04x}-{:04x}-{:04x}-{:04x}-{}",
            device_id.bus_type, device_id.vendor, device_id.product, device_id.version, name
        );
        Ok(Self {
            raw,
            device_id,
            name,
            physical_location,
            unique_id,
        })
    }
}

impl<T: Transport + 'static> DriverGeneric for VirtIoInputDevice<T> {
    fn name(&self) -> &str {
        &self.name
    }
}

impl<T: Transport + 'static> rdif_input::Interface for VirtIoInputDevice<T> {
    fn device_id(&self) -> InputDeviceId {
        self.device_id
    }

    fn physical_location(&self) -> &str {
        &self.physical_location
    }

    fn unique_id(&self) -> &str {
        &self.unique_id
    }

    fn get_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> Result<bool, InputError> {
        self.raw
            .query_config_select(InputConfigSelect::EvBits, ty as u8, out)
            .map(|read| read != 0)
            .map_err(map_input_err)
    }

    fn read_event(&mut self) -> Result<InputEvent, InputError> {
        self.raw.ack_interrupt();
        let event = self.raw.pop_pending_event().ok_or(InputError::Again)?;
        Ok(InputEvent {
            event_type: event.event_type,
            code: event.code,
            value: event.value as i32,
        })
    }

    fn get_prop_bits(&mut self, out: &mut [u8]) -> Result<usize, InputError> {
        let bits = self.raw.prop_bits().map_err(map_input_err)?;
        let len = bits.len().min(out.len());
        out[..len].copy_from_slice(&bits[..len]);
        Ok(len)
    }

    fn get_abs_info(&mut self, axis: u8) -> Result<AbsInfo, InputError> {
        let info = self.raw.abs_info(axis).map_err(map_input_err)?;
        Ok(AbsInfo {
            min: info.min as i32,
            max: info.max as i32,
            fuzz: info.fuzz as i32,
            flat: info.flat as i32,
            res: info.res as i32,
        })
    }
}

fn map_input_err(err: VirtIoError) -> InputError {
    match err {
        VirtIoError::Unsupported => InputError::NotSupported,
        VirtIoError::NotReady => InputError::Again,
        _ => InputError::Other(alloc::boxed::Box::new(err)),
    }
}
