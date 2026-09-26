//! UVC V4L2 camera driver — kernel-side glue.

use alloc::{boxed::Box, collections::BTreeSet, format, sync::Arc, vec::Vec};
use core::{future::Future, pin::Pin};

use ax_sync::Mutex;
use crab_usb::{
    err::{TransferError, USBError},
    usb_if::{
        endpoint::TransferRequest,
        host::ControlSetup,
        transfer::{Recipient, RequestType},
    },
};
use media_uvc::{IsoPending, UvcDevice, UvcHandle, UvcRuntime, UvcWorker};

use super::video_allocator::VirtualAllocator;
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

/// A camera node retains only identity. Its USB interfaces are owned while a V4L2
/// file is open, so an idle camera can be operated through usbfs/libusb.
pub struct StarryUvcHandle {
    snapshot: UsbDeviceSnapshotInfo,
    state: Mutex<HandleState>,
}

struct HandleState {
    handle: Option<UsbDeviceHandle>,
    claimed: BTreeSet<u8>,
}

impl StarryUvcHandle {
    fn new(snapshot: UsbDeviceSnapshotInfo) -> Self {
        Self {
            snapshot,
            state: Mutex::new(HandleState {
                handle: None,
                claimed: BTreeSet::new(),
            }),
        }
    }

    fn with_handle<T>(
        &self,
        f: impl FnOnce(&UsbDeviceHandle) -> Result<T, USBError>,
    ) -> Result<T, USBError> {
        self.ensure_current()?;
        let state = self.state.lock();
        f(state.handle.as_ref().ok_or(USBError::NotInitialized)?)
    }

    fn ensure_current(&self) -> Result<(), USBError> {
        let current = usbfs::usb_device_snapshots()
            .into_iter()
            .find(|snap| {
                snap.bus_num == self.snapshot.bus_num && snap.device_num == self.snapshot.device_num
            })
            .ok_or(USBError::NotFound)?;
        if current.generation != self.snapshot.generation
            || current.descriptor_blob != self.snapshot.descriptor_blob
        {
            return Err(USBError::NotFound);
        }
        Ok(())
    }
}

impl UvcHandle for StarryUvcHandle {
    fn claim_interface(&self, interface: u8, alternate: u8) -> Result<(), USBError> {
        self.ensure_current()?;
        let mut state = self.state.lock();
        if state.handle.is_none() {
            state.handle = Some(
                usbfs::acquire_usb_device_generation(
                    self.snapshot.bus_num,
                    self.snapshot.device_num,
                    self.snapshot.generation,
                )
                .map_err(map_usb_error)?,
            );
        }
        let result = state
            .handle
            .as_ref()
            .unwrap()
            .claim_interface(interface, alternate);
        if result.is_ok() {
            state.claimed.insert(interface);
        } else if state.claimed.is_empty() {
            state.handle = None;
        }
        result.map_err(map_usb_error)
    }

    fn release_interface(&self, interface: u8) -> Result<(), USBError> {
        let mut state = self.state.lock();
        if self.ensure_current().is_err() {
            state.claimed.clear();
            state.handle = None;
            return Ok(());
        }
        if !state.claimed.contains(&interface) {
            return Ok(());
        }
        state
            .handle
            .as_ref()
            .ok_or(USBError::NotInitialized)?
            .release_interface(interface)
            .map_err(map_usb_error)?;
        state.claimed.remove(&interface);
        if state.claimed.is_empty() {
            state.handle = None;
        }
        Ok(())
    }

    fn control_in(&self, param: ControlSetup, data: &mut [u8]) -> Result<usize, USBError> {
        let bmrt = control_setup_to_bmrequesttype(&param) | 0x80;
        let req = control_setup_to_brequest(&param);
        self.with_handle(|handle| {
            handle
                .control_transfer(bmrt, req, param.value, param.index, data)
                .map_err(map_usb_error)
        })
    }

    fn control_out(&self, param: ControlSetup, data: &[u8]) -> Result<(), USBError> {
        let bmrt = control_setup_to_bmrequesttype(&param) & !0x80;
        let req = control_setup_to_brequest(&param);
        let mut buf = Vec::from(data);
        self.with_handle(|handle| {
            handle
                .control_transfer(bmrt, req, param.value, param.index, &mut buf)
                .map(|_| ())
                .map_err(map_usb_error)
        })
    }

    fn submit_endpoint_transfer(
        &self,
        endpoint: u8,
        request: TransferRequest,
    ) -> Result<IsoPending, USBError> {
        self.with_handle(|handle| {
            let submitted = handle
                .submit_endpoint_transfer(endpoint, request)
                .map_err(map_usb_error)?;
            match submitted.inner {
                SubmittedTransferInner::Endpoint {
                    endpoint,
                    request_id,
                } => Ok(IsoPending::new(endpoint, request_id)),
                SubmittedTransferInner::Control { .. } => Err(USBError::InvalidParameter),
            }
        })
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

pub type CameraDriver = UvcDevice<StarryUvcHandle, VirtualAllocator>;

struct StarryUvcRuntime;

impl UvcRuntime for StarryUvcRuntime {
    fn spawn(
        &self,
        future: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Result<Box<dyn UvcWorker>, USBError> {
        let thread = crate::task::kernel_thread_builder("uvc-stream".into())
            .spawn(move || crate::task::future::block_on(future))
            .map_err(|error| {
                USBError::Other(anyhow::anyhow!("failed to spawn UVC worker: {error}"))
            })?;
        Ok(Box::new(move || {
            let _ = thread.join();
        }))
    }
}

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
    let mut snapshots: alloc::vec::Vec<_> = usbfs::usb_device_snapshots()
        .into_iter()
        .filter(|snap| CameraDriver::check(&snap.descriptor_blob))
        .collect();
    snapshots.sort_by_key(|snap| (snap.bus_num, snap.device_num));
    snapshots
}

pub fn create_camera_driver(snap: &UsbDeviceSnapshotInfo) -> StarryResult<CameraDriver> {
    let handle = StarryUvcHandle::new(snap.clone());
    let bus_info = format!("usb-{:03}-{:03}", snap.bus_num, snap.device_num);
    UvcDevice::new(
        handle,
        VirtualAllocator::new(),
        Arc::new(StarryUvcRuntime),
        ax_runtime::hal::time::monotonic_time_nanos,
        &snap.descriptor_blob,
        &bus_info,
    )
    .map_err(map_uvc_error)
}
