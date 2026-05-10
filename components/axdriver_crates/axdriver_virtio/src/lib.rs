//! Wrappers of some devices in the [`virtio-drivers`][1] crate, that implement
//! traits in the [`ax-driver-base`][2] series crates.
//!
//! Like the [`virtio-drivers`][1] crate, you must implement the [`VirtIoHal`]
//! trait (alias of [`virtio-drivers::Hal`][3]), to allocate DMA regions and
//! translate between physical addresses (as seen by devices) and virtual
//! addresses (as seen by your program).
//!
//! [1]: https://docs.rs/virtio-drivers/latest/virtio_drivers/
//! [2]: https://github.com/arceos-org/axdriver_crates/tree/main/axdriver_base
//! [3]: https://docs.rs/virtio-drivers/latest/virtio_drivers/trait.Hal.html

#![no_std]
#![cfg_attr(doc, feature(doc_cfg))]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "block")]
mod blk;
#[cfg(feature = "block")]
pub use self::blk::VirtIoBlkDev;

#[cfg(feature = "gpu")]
mod gpu;
#[cfg(feature = "gpu")]
pub use self::gpu::VirtIoGpuDev;

#[cfg(feature = "input")]
mod input;
#[cfg(feature = "input")]
pub use self::input::VirtIoInputDev;

#[cfg(feature = "net")]
mod net;
#[cfg(feature = "net")]
pub use self::net::VirtIoNetDev;

#[cfg(feature = "socket")]
mod socket;
use ax_driver_base::{DevError, DeviceType};
use virtio_drivers::transport::DeviceType as VirtIoDevType;
pub use virtio_drivers::{
    BufferDirection, Hal as VirtIoHal, PhysAddr,
    transport::{
        Transport,
        pci::{PciTransport, bus as pci},
    },
};
pub type MmioTransport = virtio_drivers::transport::mmio::MmioTransport<'static>;

use self::pci::{ConfigurationAccess, DeviceFunction, DeviceFunctionInfo, PciRoot};
#[cfg(feature = "socket")]
pub use self::socket::VirtIoSocketDev;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MmioProbeRegion {
    base: *mut u8,
    size: usize,
}

impl MmioProbeRegion {
    fn try_new(base: *mut u8, size: usize) -> Option<Self> {
        if base.is_null() || size == 0 {
            return None;
        }
        Some(Self { base, size })
    }

    fn header(self) -> Option<core::ptr::NonNull<virtio_drivers::transport::mmio::VirtIOHeader>> {
        use core::ptr::NonNull;

        use virtio_drivers::transport::mmio::VirtIOHeader;

        NonNull::new(self.base as *mut VirtIOHeader)
    }

    fn try_transport(self) -> Option<MmioTransport> {
        let header = self.header()?;
        unsafe { MmioTransport::new(header, self.size) }.ok()
    }
}

const fn as_socket_dev_err(e: virtio_drivers::device::socket::SocketError) -> DevError {
    use virtio_drivers::device::socket::SocketError::*;

    match e {
        ConnectionExists => DevError::AlreadyExists,
        NotConnected => DevError::BadState,
        InvalidOperation | InvalidNumber | UnknownOperation(_) => DevError::InvalidParam,
        OutputBufferTooShort(_) | BufferTooShort | BufferTooLong(..) => DevError::InvalidParam,
        UnexpectedDataInPacket | PeerSocketShutdown => DevError::Io,
        InsufficientBufferSpaceInPeer => DevError::Again,
        RecycledWrongBuffer => DevError::BadState,
    }
}

/// Try to probe a VirtIO MMIO device from the given memory region.
///
/// If the device is recognized, returns the device type and a transport object
/// for later operations. Otherwise, returns [`None`].
pub fn probe_mmio_device(
    reg_base: *mut u8,
    reg_size: usize,
) -> Option<(DeviceType, MmioTransport)> {
    let region = MmioProbeRegion::try_new(reg_base, reg_size)?;
    let transport = region.try_transport()?;
    let dev_type = as_dev_type(transport.device_type())?;
    Some((dev_type, transport))
}

/// Try to probe a VirtIO PCI device from the given PCI address.
///
/// If the device is recognized, returns the device type and a transport object
/// for later operations. Otherwise, returns [`None`].
pub fn probe_pci_device<H: VirtIoHal, C: ConfigurationAccess>(
    root: &mut PciRoot<C>,
    bdf: DeviceFunction,
    dev_info: &DeviceFunctionInfo,
) -> Option<(DeviceType, PciTransport)> {
    use virtio_drivers::transport::pci::virtio_device_type;

    let dev_type = virtio_device_type(dev_info).and_then(as_dev_type)?;
    let transport = PciTransport::new::<H, C>(root, bdf).ok()?;
    Some((dev_type, transport))
}

const fn as_dev_type(t: VirtIoDevType) -> Option<DeviceType> {
    use VirtIoDevType::*;
    match t {
        Block => Some(DeviceType::Block),
        Network => Some(DeviceType::Net),
        GPU => Some(DeviceType::Display),
        Input => Some(DeviceType::Input),
        Socket => Some(DeviceType::Vsock),
        _ => None,
    }
}

#[allow(dead_code)]
const fn as_dev_err(e: virtio_drivers::Error) -> DevError {
    use virtio_drivers::Error::*;
    match e {
        QueueFull => DevError::BadState,
        NotReady => DevError::Again,
        WrongToken => DevError::BadState,
        AlreadyUsed => DevError::AlreadyExists,
        InvalidParam => DevError::InvalidParam,
        DmaError => DevError::NoMemory,
        IoError => DevError::Io,
        Unsupported => DevError::Unsupported,
        ConfigSpaceTooSmall => DevError::BadState,
        ConfigSpaceMissing => DevError::BadState,
        SocketDeviceError(e) => as_socket_dev_err(e),
    }
}

#[cfg(test)]
mod tests {
    use ax_driver_base::{DevError, DeviceType};
    use virtio_drivers::{
        Error, device::socket::SocketError, transport::DeviceType as VirtIoDevType,
    };

    use super::{as_dev_err, as_dev_type, probe_mmio_device};

    #[test]
    fn as_dev_type_maps_supported_devices() {
        assert_eq!(as_dev_type(VirtIoDevType::Block), Some(DeviceType::Block));
        assert_eq!(as_dev_type(VirtIoDevType::Network), Some(DeviceType::Net));
        assert_eq!(as_dev_type(VirtIoDevType::GPU), Some(DeviceType::Display));
        assert_eq!(as_dev_type(VirtIoDevType::Input), Some(DeviceType::Input));
        assert_eq!(as_dev_type(VirtIoDevType::Socket), Some(DeviceType::Vsock));
    }

    #[test]
    fn as_dev_type_rejects_unsupported_devices() {
        assert_eq!(as_dev_type(VirtIoDevType::Console), None);
        assert_eq!(as_dev_type(VirtIoDevType::EntropySource), None);
    }

    #[test]
    fn probe_mmio_device_returns_none_for_null_base() {
        assert!(probe_mmio_device(core::ptr::null_mut(), 0x1000).is_none());
    }

    #[test]
    fn as_dev_err_maps_common_errors() {
        assert!(matches!(as_dev_err(Error::QueueFull), DevError::BadState));
        assert!(matches!(
            as_dev_err(Error::InvalidParam),
            DevError::InvalidParam
        ));
        assert!(matches!(as_dev_err(Error::DmaError), DevError::NoMemory));
        assert!(matches!(as_dev_err(Error::IoError), DevError::Io));
        assert!(matches!(
            as_dev_err(Error::Unsupported),
            DevError::Unsupported
        ));
    }

    #[test]
    fn as_dev_err_maps_socket_errors() {
        assert!(matches!(
            as_dev_err(Error::SocketDeviceError(SocketError::ConnectionExists)),
            DevError::AlreadyExists
        ));
        assert!(matches!(
            as_dev_err(Error::SocketDeviceError(SocketError::NotConnected)),
            DevError::BadState
        ));
        assert!(matches!(
            as_dev_err(Error::SocketDeviceError(SocketError::InvalidNumber)),
            DevError::InvalidParam
        ));
        assert!(matches!(
            as_dev_err(Error::SocketDeviceError(
                SocketError::InsufficientBufferSpaceInPeer
            )),
            DevError::Again
        ));
        assert!(matches!(
            as_dev_err(Error::SocketDeviceError(SocketError::PeerSocketShutdown)),
            DevError::Io
        ));
    }
}
