extern crate alloc;

use alloc::{boxed::Box, string::String, vec::Vec};

use ax_driver_base::DevError;
use ax_driver_vsock::VsockDriverOps;
use rdif_vsock::{Interface, VsockAddr, VsockConnId, VsockError, VsockEvent};
use rdrive::{Device, DriverGeneric, PlatformDevice};

pub struct VsockDevice {
    name: String,
    vsock: Option<Box<dyn Interface>>,
}

impl VsockDevice {
    fn new(name: String, vsock: Box<dyn Interface>) -> Self {
        Self {
            name,
            vsock: Some(vsock),
        }
    }
}

impl DriverGeneric for VsockDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

pub trait PlatformDeviceVsock {
    fn register_vsock<T>(self, dev: T)
    where
        T: Interface + 'static;
}

impl PlatformDeviceVsock for PlatformDevice {
    fn register_vsock<T>(self, dev: T)
    where
        T: Interface + 'static,
    {
        let name = dev.name().into();
        self.register(VsockDevice::new(name, Box::new(dev)));
    }
}

pub fn register_legacy_vsock<D>(plat_dev: PlatformDevice, driver: D)
where
    D: VsockDriverOps + 'static,
{
    plat_dev.register_vsock(LegacyVsockDevice { driver });
}

pub fn take_vsock_devices() -> Result<Vec<Box<dyn Interface>>, axklib::AxError> {
    let mut devices = Vec::new();
    for dev in rdrive::get_list::<VsockDevice>() {
        devices.push(take_vsock_device(dev)?);
    }
    Ok(devices)
}

fn take_vsock_device(device: Device<VsockDevice>) -> Result<Box<dyn Interface>, axklib::AxError> {
    let mut device = device.lock().map_err(|_| axklib::AxError::BadState)?;
    device.vsock.take().ok_or(axklib::AxError::BadState)
}

struct LegacyVsockDevice<D> {
    driver: D,
}

impl<D: VsockDriverOps + 'static> DriverGeneric for LegacyVsockDevice<D> {
    fn name(&self) -> &str {
        self.driver.device_name()
    }
}

impl<D: VsockDriverOps + 'static> Interface for LegacyVsockDevice<D> {
    fn guest_cid(&self) -> u64 {
        self.driver.guest_cid()
    }

    fn listen(&mut self, port: u32) -> Result<(), VsockError> {
        self.driver.listen(port);
        Ok(())
    }

    fn connect(&mut self, id: VsockConnId) -> Result<(), VsockError> {
        self.driver
            .connect(to_legacy_conn(id))
            .map_err(map_vsock_error)
    }

    fn send(&mut self, id: VsockConnId, buf: &[u8]) -> Result<usize, VsockError> {
        self.driver
            .send(to_legacy_conn(id), buf)
            .map_err(map_vsock_error)
    }

    fn recv(&mut self, id: VsockConnId, buf: &mut [u8]) -> Result<usize, VsockError> {
        self.driver
            .recv(to_legacy_conn(id), buf)
            .map_err(map_vsock_error)
    }

    fn recv_avail(&mut self, id: VsockConnId) -> Result<usize, VsockError> {
        self.driver
            .recv_avail(to_legacy_conn(id))
            .map_err(map_vsock_error)
    }

    fn disconnect(&mut self, id: VsockConnId) -> Result<(), VsockError> {
        self.driver
            .disconnect(to_legacy_conn(id))
            .map_err(map_vsock_error)
    }

    fn abort(&mut self, id: VsockConnId) -> Result<(), VsockError> {
        self.driver
            .abort(to_legacy_conn(id))
            .map_err(map_vsock_error)
    }

    fn poll_event(&mut self) -> Result<Option<VsockEvent>, VsockError> {
        self.driver
            .poll_event()
            .map(|event| event.map(from_legacy_event))
            .map_err(map_vsock_error)
    }
}

fn to_legacy_conn(id: VsockConnId) -> ax_driver_vsock::VsockConnId {
    ax_driver_vsock::VsockConnId {
        peer_addr: ax_driver_vsock::VsockAddr {
            cid: id.peer_addr.cid,
            port: id.peer_addr.port,
        },
        local_port: id.local_port,
    }
}

fn from_legacy_conn(id: ax_driver_vsock::VsockConnId) -> VsockConnId {
    VsockConnId {
        peer_addr: VsockAddr {
            cid: id.peer_addr.cid,
            port: id.peer_addr.port,
        },
        local_port: id.local_port,
    }
}

fn from_legacy_event(event: ax_driver_vsock::VsockDriverEvent) -> VsockEvent {
    match event {
        ax_driver_vsock::VsockDriverEvent::ConnectionRequest(id) => {
            VsockEvent::ConnectionRequest(from_legacy_conn(id))
        }
        ax_driver_vsock::VsockDriverEvent::Connected(id) => {
            VsockEvent::Connected(from_legacy_conn(id))
        }
        ax_driver_vsock::VsockDriverEvent::Received(id, len) => {
            VsockEvent::Received(from_legacy_conn(id), len)
        }
        ax_driver_vsock::VsockDriverEvent::Disconnected(id) => {
            VsockEvent::Disconnected(from_legacy_conn(id))
        }
        ax_driver_vsock::VsockDriverEvent::CreditUpdate(id) => {
            VsockEvent::CreditUpdate(from_legacy_conn(id))
        }
        ax_driver_vsock::VsockDriverEvent::Unknown => VsockEvent::Unknown,
    }
}

fn map_vsock_error(err: DevError) -> VsockError {
    match err {
        DevError::Again | DevError::ResourceBusy => VsockError::Retry,
        DevError::AlreadyExists => VsockError::AlreadyExists,
        DevError::BadState => VsockError::NotConnected,
        DevError::Unsupported => VsockError::NotSupported,
        _ => VsockError::Other(Box::new(rdif_vsock::KError::Unknown("legacy vsock error"))),
    }
}
