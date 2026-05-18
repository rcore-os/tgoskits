use alloc::{borrow::ToOwned, format, string::String};

use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use ax_driver_input::{AbsInfo, Event, EventType, InputDeviceId, InputDriverOps};
use virtio_drivers::{
    Hal,
    device::input::{InputConfigSelect, VirtIOInput as InnerDev},
    transport::Transport,
};

use crate::as_dev_err;

/// The VirtIO Input device driver.
pub struct VirtIoInputDev<H: Hal, T: Transport> {
    inner: InnerDev<H, T>,
    device_id: InputDeviceId,
    name: String,
    physical_location: String,
    unique_id: String,
}

unsafe impl<H: Hal, T: Transport> Send for VirtIoInputDev<H, T> {}
unsafe impl<H: Hal, T: Transport> Sync for VirtIoInputDev<H, T> {}

impl<H: Hal, T: Transport> VirtIoInputDev<H, T> {
    fn normalize_name(name: String) -> String {
        if name.is_empty() {
            "<unknown>".to_owned()
        } else {
            name
        }
    }

    fn read_device_name(virtio: &mut InnerDev<H, T>) -> String {
        let name = virtio.name().unwrap_or_else(|_| "<unknown>".to_owned());
        Self::normalize_name(name)
    }

    fn read_device_id(virtio: &mut InnerDev<H, T>) -> DevResult<InputDeviceId> {
        let device_id = virtio.ids().map_err(as_dev_err)?;
        Ok(InputDeviceId {
            bus_type: device_id.bustype,
            vendor: device_id.vendor,
            product: device_id.product,
            version: device_id.version,
        })
    }

    fn build_physical_location(device_id: InputDeviceId) -> String {
        format!(
            "virtio-input/{:04x}:{:04x}:{:04x}:{:04x}",
            device_id.bus_type, device_id.vendor, device_id.product, device_id.version
        )
    }

    fn build_unique_id(device_id: InputDeviceId, name: &str) -> String {
        format!(
            "virtio-{:04x}-{:04x}-{:04x}-{:04x}-{}",
            device_id.bus_type, device_id.vendor, device_id.product, device_id.version, name
        )
    }

    fn query_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> DevResult<usize> {
        self.inner
            .query_config_select(InputConfigSelect::EvBits, ty as u8, out)
            .map(|read| read as usize)
            .map_err(as_dev_err)
    }

    fn map_pending_event(event: virtio_drivers::device::input::InputEvent) -> Event {
        Event {
            event_type: event.event_type,
            code: event.code,
            value: event.value,
        }
    }

    fn pop_pending_event(&mut self) -> DevResult<Event> {
        self.inner
            .pop_pending_event()
            .map(Self::map_pending_event)
            .ok_or(DevError::Again)
    }

    /// Creates a new driver instance and initializes the device, or returns
    /// an error if any step fails.
    pub fn try_new(transport: T) -> DevResult<Self> {
        let mut virtio = InnerDev::new(transport).map_err(as_dev_err)?;
        let name = Self::read_device_name(&mut virtio);
        let device_id = Self::read_device_id(&mut virtio)?;
        let physical_location = Self::build_physical_location(device_id);
        let unique_id = Self::build_unique_id(device_id, &name);

        Ok(Self {
            inner: virtio,
            device_id,
            name,
            physical_location,
            unique_id,
        })
    }
}

impl<H: Hal, T: Transport> BaseDriverOps for VirtIoInputDev<H, T> {
    fn device_name(&self) -> &str {
        &self.name
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Input
    }
}

impl<H: Hal, T: Transport> InputDriverOps for VirtIoInputDev<H, T> {
    fn device_id(&self) -> InputDeviceId {
        self.device_id
    }

    fn physical_location(&self) -> &str {
        &self.physical_location
    }

    fn unique_id(&self) -> &str {
        &self.unique_id
    }

    fn get_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> DevResult<bool> {
        let read = self.query_event_bits(ty, out)?;
        Ok(read != 0)
    }

    fn read_event(&mut self) -> DevResult<Event> {
        self.inner.ack_interrupt();
        self.pop_pending_event()
    }

    fn get_prop_bits(&mut self, out: &mut [u8]) -> DevResult<usize> {
        let bits = self.inner.prop_bits().map_err(as_dev_err)?;
        let len = bits.len().min(out.len());
        out[..len].copy_from_slice(&bits[..len]);
        Ok(len)
    }

    fn get_abs_info(&mut self, axis: u8) -> DevResult<AbsInfo> {
        let info = self.inner.abs_info(axis).map_err(as_dev_err)?;
        Ok(AbsInfo {
            min: info.min,
            max: info.max,
            fuzz: info.fuzz,
            flat: info.flat,
            res: info.res,
        })
    }
}
