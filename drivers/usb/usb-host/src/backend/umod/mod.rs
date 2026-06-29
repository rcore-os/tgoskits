use std::{sync::Arc, thread};

use futures::FutureExt;
use usb_if::err::USBError;

use crate::{
    USBHost,
    backend::{
        BackendOp,
        ty::{DeviceInfoOp, ProbedDeviceInfoOp},
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
        };
        Ok(host)
    }
}

pub struct Libusb {
    ctx: Arc<context::Context>,
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

        Self { ctx }
    }

    async fn device_list(&mut self) -> Result<Vec<ProbedDeviceInfoOp>, USBError> {
        let ctx = self.ctx.clone();
        let devices = ctx.device_list()?;
        let mut infos = Vec::new();
        for dev in devices {
            let info = device::DeviceInfo::new(dev)?;
            let is_hub = info.descriptor().class == 0x09;
            let info = Box::new(info) as Box<dyn super::ty::DeviceInfoOp>;
            let info = if is_hub {
                ProbedDeviceInfoOp::Hub(info)
            } else {
                ProbedDeviceInfoOp::Device(info)
            };
            infos.push(info);
        }
        Ok(infos)
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
    ) -> futures::future::BoxFuture<'a, Result<Vec<ProbedDeviceInfoOp>, USBError>> {
        self.device_list().boxed()
    }

    fn open_device<'a>(
        &'a mut self,
        dev: &'a dyn super::ty::DeviceInfoOp,
    ) -> futures::future::LocalBoxFuture<'a, Result<Box<dyn super::ty::DeviceOp>, USBError>> {
        async move { self._open_device(dev).await }.boxed_local()
    }
}
