//! UVC V4L2 camera driver — kernel-side glue.

use crab_usb::{
    err::{TransferError, USBError},
    usb_if::{
        endpoint::TransferRequest,
        host::ControlSetup,
        transfer::{Recipient, RequestType},
    },
};
use media_uvc::{IsoPending, UvcDevice, UvcHandle};

use crate::{
    StarryError, StarryResult,
    pseudofs::usbfs::{self, SubmittedTransferInner, UsbDeviceHandle, UsbDeviceSnapshotInfo},
};

/// 将 usbfs 层错误映射为 [`USBError`]（uvc 的错误类型）。
fn map_usb_error(e: StarryError) -> USBError {
    use StarryError::*;
    match e {
        InvalidInput => USBError::InvalidParameter,
        NotFound | NoSuchDevice | NoSuchDeviceOrAddress => USBError::NotFound,
        ResourceBusy => USBError::SlotLimitReached,
        Unsupported | NotATty => USBError::NotSupported,
        TimedOut => USBError::Timeout,
        NoMemory => USBError::NoMemory,
        Errno(crate::Errno::ENOENT) => {
            USBError::TransferError(crab_usb::usb_if::err::TransferError::Cancelled)
        }
        OperationNotPermitted | PermissionDenied => {
            USBError::Other(anyhow::anyhow!("usbfs: operation not permitted: {e}"))
        }
        other => USBError::Other(anyhow::anyhow!("usbfs: {other}")),
    }
}

// ── UvcHandle impl for UsbDeviceHandle ────────────────────────────────

impl UvcHandle for UsbDeviceHandle {
    fn claim_interface(&self, interface: u8, alternate: u8) -> Result<(), USBError> {
        UsbDeviceHandle::claim_interface(self, interface, alternate).map_err(map_usb_error)
    }

    fn release_interface(&self, interface: u8) -> Result<(), USBError> {
        UsbDeviceHandle::release_interface(self, interface).map_err(map_usb_error)
    }

    fn control_in(&self, param: ControlSetup, data: &mut [u8]) -> Result<usize, USBError> {
        let bmrt = control_setup_to_bmrequesttype(&param) | 0x80; // IN
        let req = control_setup_to_brequest(&param);
        self.control_transfer(bmrt, req, param.value, param.index, data)
            .map_err(map_usb_error)
    }

    fn control_out(&self, param: ControlSetup, data: &[u8]) -> Result<(), USBError> {
        let bmrt = control_setup_to_bmrequesttype(&param) & !0x80; // OUT
        let req = control_setup_to_brequest(&param);
        let mut buf = [0u8; 64];
        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);
        let _ = self
            .control_transfer(bmrt, req, param.value, param.index, &mut buf[..len])
            .map_err(map_usb_error)?;
        Ok(())
    }

    fn submit_endpoint_transfer(
        &self,
        endpoint: u8,
        request: TransferRequest,
    ) -> Result<IsoPending, USBError> {
        let submitted = self
            .submit_endpoint_transfer(endpoint, request)
            .map_err(map_usb_error)?;
        match submitted.inner {
            SubmittedTransferInner::Endpoint {
                endpoint,
                request_id,
            } => Ok(IsoPending::new(endpoint, request_id)),
            SubmittedTransferInner::Control { .. } => Err(USBError::InvalidParameter),
        }
    }
}

fn control_setup_to_bmrequesttype(setup: &ControlSetup) -> u8 {
    use Recipient::*;
    use RequestType::*;
    let ty_bits = match setup.request_type {
        Standard => 0x00,
        Class => 0x20,
        Vendor => 0x40,
        Reserved => 0x60,
    };
    let recip_bits = match setup.recipient {
        Device => 0x00,
        Interface => 0x01,
        Endpoint => 0x02,
        Other => 0x03,
    };
    ty_bits | recip_bits
}

fn control_setup_to_brequest(setup: &ControlSetup) -> u8 {
    setup.request.into()
}

// ── Camera driver creation ───────────────────────────────────────────

pub type CameraDriver = UvcDevice<UsbDeviceHandle>;

/// 将 UVC 驱动错误映射为 [`StarryError`]。
fn map_uvc_error(err: USBError) -> StarryError {
    match err {
        USBError::InvalidParameter => StarryError::InvalidInput,
        USBError::NotFound => StarryError::NotFound,
        USBError::NotSupported => StarryError::Unsupported,
        USBError::Timeout => StarryError::TimedOut,
        USBError::NoMemory => StarryError::NoMemory,
        USBError::SlotLimitReached => StarryError::ResourceBusy,
        USBError::NotInitialized | USBError::ConfigurationNotSet => StarryError::BadState,
        USBError::InterfaceBroken => StarryError::Io,
        USBError::TransferError(err) => map_transfer_error(err),
        USBError::Other(_) => StarryError::Io,
    }
}

/// 将 UVC 传输错误映射为 [`StarryError`]。
fn map_transfer_error(err: TransferError) -> StarryError {
    match err {
        TransferError::Timeout => StarryError::TimedOut,
        TransferError::Cancelled | TransferError::EndpointRevoked => {
            StarryError::from(crate::Errno::ENOENT)
        }
        TransferError::Stall => StarryError::BrokenPipe,
        TransferError::QueueFull => StarryError::ResourceBusy,
        TransferError::InvalidEndpoint => StarryError::InvalidInput,
        TransferError::NoDevice | TransferError::Disconnected => StarryError::NoSuchDevice,
        TransferError::NotSupported => StarryError::Unsupported,
        TransferError::Other(_) => StarryError::Io,
    }
}

pub fn collect_uvc_snapshots() -> alloc::vec::Vec<UsbDeviceSnapshotInfo> {
    usbfs::usb_device_snapshots()
        .into_iter()
        .filter(|snap| CameraDriver::check(&snap.descriptor_blob))
        .collect()
}

pub fn create_camera_driver(snap: &UsbDeviceSnapshotInfo) -> StarryResult<CameraDriver> {
    let handle = usbfs::acquire_usb_device(snap.bus_num, snap.device_num)?;
    UvcDevice::new(handle, &snap.descriptor_blob).map_err(map_uvc_error)
}
