use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::backend::BackendOp;
use crate::backend::ty::*;
use crate::err::Result;

#[cfg(kmod)]
pub use super::backend::kmod::*;

#[cfg(umod)]
pub use super::backend::umod::*;

pub use crate::device::{Device, DeviceInfo, HubDeviceInfo, ProbedDevice};

/// USB 主机控制器
pub struct USBHost {
    pub(crate) backend: Box<dyn BackendOp>,
}

impl USBHost {
    /// 初始化主机控制器
    pub async fn init(&mut self) -> Result<()> {
        self.backend.init().await?;
        Ok(())
    }

    pub async fn probe_devices(&mut self) -> Result<Vec<ProbedDevice>> {
        let device_infos = self.backend.device_list().await?;
        let mut devices = Vec::new();
        for dev in device_infos {
            let dev_info = match dev {
                ProbedDeviceInfoOp::Device(inner) => ProbedDevice::Device(DeviceInfo { inner }),
                ProbedDeviceInfoOp::Hub(inner) => ProbedDevice::Hub(HubDeviceInfo { inner }),
            };
            devices.push(dev_info);
        }
        Ok(devices)
    }

    #[cfg(kmod)]
    pub fn create_event_handler(&mut self) -> EventHandler {
        let handler = self.backend.create_event_handler();
        EventHandler { handler }
    }

    pub async fn open_device(&mut self, dev: &DeviceInfo) -> Result<Device> {
        let device = self.backend.open_device(dev.inner.as_ref()).await?;
        let mut device: Device = device.into();
        device.init().await?;
        Ok(device)
    }
}

pub struct EventHandler {
    handler: Box<dyn EventHandlerOp>,
}

impl EventHandler {
    /// 处理事件
    pub fn handle_event(&self) -> Event {
        self.handler.handle_event()
    }
}
