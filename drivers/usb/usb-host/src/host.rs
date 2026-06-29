use alloc::boxed::Box;
#[cfg(any(kmod, umod))]
use alloc::vec::Vec;

#[cfg(kmod)]
pub use super::backend::kmod::*;
#[cfg(umod)]
pub use super::backend::umod::*;
pub use crate::device::{Device, DeviceInfo, HubDeviceInfo, ProbedDevice};
use crate::{
    backend::{BackendOp, ty::*},
    err::Result,
};

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

    #[cfg(any(kmod, umod))]
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

    pub fn enable_irq(&mut self) -> Result {
        self.backend.enable_irq()
    }

    pub fn disable_irq(&mut self) -> Result {
        self.backend.disable_irq()
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

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use futures::{FutureExt, future::LocalBoxFuture};
    use usb_if::err::USBError;

    use super::*;
    use crate::backend::{BackendOp, ty::DeviceOp};

    #[derive(Default)]
    struct IrqCalls {
        enable: AtomicUsize,
        disable: AtomicUsize,
    }

    struct TestBackend {
        calls: Arc<IrqCalls>,
    }

    impl BackendOp for TestBackend {
        fn init<'a>(&'a mut self) -> futures::future::BoxFuture<'a, crate::err::Result> {
            async { Ok(()) }.boxed()
        }

        fn open_device<'a>(
            &'a mut self,
            _dev: &'a dyn crate::backend::ty::DeviceInfoOp,
        ) -> LocalBoxFuture<'a, crate::err::Result<Box<dyn DeviceOp>>> {
            async { Err(USBError::NotSupported) }.boxed_local()
        }

        fn enable_irq(&mut self) -> crate::err::Result {
            self.calls.enable.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn disable_irq(&mut self) -> crate::err::Result {
            self.calls.disable.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn host_irq_control_forwards_to_backend() {
        let calls = Arc::new(IrqCalls::default());
        let mut host = USBHost {
            backend: Box::new(TestBackend {
                calls: calls.clone(),
            }),
        };

        host.enable_irq().unwrap();
        host.disable_irq().unwrap();

        assert_eq!(calls.enable.load(Ordering::Relaxed), 1);
        assert_eq!(calls.disable.load(Ordering::Relaxed), 1);
    }
}
