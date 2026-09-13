use std::{collections::BTreeSet, sync::Arc, thread};

use futures::FutureExt;
use usb_if::err::USBError;

use crate::{
    USBHost,
    backend::{
        BackendOp,
        ty::{DeviceInfoOp, ProbeChangesOp, ProbedDeviceInfoOp},
    },
};

#[macro_use]
mod err;

mod context;
mod device;
mod endpoint;

impl USBHost {
    pub fn new_libusb() -> Result<USBHost, USBError> {
        let host = USBHost {
            backend: Box::new(Libusb::new()),
            initialized: false,
        };
        Ok(host)
    }
}

pub struct Libusb {
    ctx: Arc<context::Context>,
    known_devices: BTreeSet<usize>,
}

impl Libusb {
    pub fn new() -> Self {
        let ctx = context::Context::new().expect("Failed to create libusb context");
        let handle = Arc::downgrade(&ctx);

        thread::spawn(move || {
            trace!("Libusb event handling thread started");
            while let Some(ctx) = handle.upgrade() {
                if let Err(e) = ctx.handle_events() {
                    error!("Libusb handle events error: {:?}", e);
                }

                trace!("Libusb event handling iteration complete");
            }
        });

        Self {
            ctx,
            known_devices: BTreeSet::new(),
        }
    }

    async fn device_list(&mut self) -> Result<ProbeChangesOp, USBError> {
        let ctx = self.ctx.clone();
        let devices = ctx.device_list()?;
        let mut infos = Vec::new();
        let mut current_devices = BTreeSet::new();
        for dev in devices {
            let info = device::DeviceInfo::new(dev)?;
            let device_id = info.id();
            current_devices.insert(device_id);
            if self.known_devices.contains(&device_id) {
                continue;
            }
            let is_hub = info.descriptor().class == 0x09;
            let info = Box::new(info) as Box<dyn super::ty::DeviceInfoOp>;
            let info = if is_hub {
                ProbedDeviceInfoOp::Hub(info)
            } else {
                ProbedDeviceInfoOp::Device(info)
            };
            infos.push(info);
        }
        let disconnected = self
            .known_devices
            .difference(&current_devices)
            .copied()
            .collect();
        self.known_devices = current_devices;
        Ok(ProbeChangesOp {
            connected: infos,
            disconnected,
        })
    }

    async fn _open_device(
        &mut self,
        dev: &dyn super::ty::DeviceInfoOp,
    ) -> Result<Box<dyn super::ty::DeviceOp>, USBError> {
        let dev_info = (dev as &dyn core::any::Any)
            .downcast_ref::<device::DeviceInfo>()
            .unwrap();

        let device = device::Device::new(dev_info, self.ctx.clone())?;
        Ok(Box::new(device) as Box<dyn super::ty::DeviceOp>)
    }
}

impl Default for Libusb {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendOp for Libusb {
    fn init<'a>(&'a mut self) -> futures::future::BoxFuture<'a, Result<(), USBError>> {
        async { Ok(()) }.boxed()
    }

    fn device_list<'a>(
        &'a mut self,
    ) -> futures::future::BoxFuture<'a, Result<ProbeChangesOp, USBError>> {
        self.device_list().boxed()
    }

    fn open_device<'a>(
        &'a mut self,
        dev: &'a dyn super::ty::DeviceInfoOp,
    ) -> futures::future::LocalBoxFuture<'a, Result<Box<dyn super::ty::DeviceOp>, USBError>> {
        async move { self._open_device(dev).await }.boxed_local()
    }
}
