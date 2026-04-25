use alloc::{borrow::ToOwned, format, string::String};

use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use ax_driver_input::{Event, EventType, InputDeviceId, InputDriverOps};
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
    /// Creates a new driver instance and initializes the device, or returns
    /// an error if any step fails.
    pub fn try_new(transport: T) -> DevResult<Self> {
        let mut virtio = InnerDev::new(transport).map_err(as_dev_err)?;
        let name = virtio.name().unwrap_or_else(|_| "<unknown>".to_owned());
        let ids = virtio.ids().map_err(as_dev_err)?;
        let device_id = InputDeviceId {
            bus_type: ids.bustype,
            vendor: ids.vendor,
            product: ids.product,
            version: ids.version,
        };

        // The VirtIO input configuration does not provide a globally unique
        // physical path, so we derive deterministic identifiers from device IDs.
        let (physical_location, unique_id) = build_identifiers(device_id);

        Ok(Self {
            inner: virtio,
            device_id,
            name,
            physical_location,
            unique_id,
        })
    }
}

fn build_identifiers(device_id: InputDeviceId) -> (String, String) {
    let physical_location = format!(
        "virtio/input/{:04x}:{:04x}:{:04x}:{:04x}",
        device_id.bus_type,
        device_id.vendor,
        device_id.product,
        device_id.version
    );
    let unique_id = format!(
        "virtio-input-{:04x}-{:04x}-{:04x}-{:04x}",
        device_id.bus_type,
        device_id.vendor,
        device_id.product,
        device_id.version
    );
    (physical_location, unique_id)
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
        let read = self
            .inner
            .query_config_select(InputConfigSelect::EvBits, ty as u8, out)
            .map_err(as_dev_err)?;
        Ok(read != 0)
    }

    fn read_event(&mut self) -> DevResult<Event> {
        self.inner.ack_interrupt();
        self.inner
            .pop_pending_event()
            .map(|e| Event {
                event_type: e.event_type,
                code: e.code,
                value: e.value,
            })
            .ok_or(DevError::Again)
    }
}

