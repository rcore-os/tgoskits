use alloc::boxed::Box;
#[cfg(any(kmod, umod))]
use alloc::vec::Vec;

#[cfg(kmod)]
use rdif_irq::{ContainmentCause, IrqCapture, MaskedSource};

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
    pub(crate) initialized: bool,
}

impl USBHost {
    /// 初始化主机控制器
    pub async fn init(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        self.backend.init().await?;
        self.initialized = true;
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

    #[cfg(kmod)]
    pub fn dwc2_transfer_stats(&self) -> Option<Dwc2TransferStats> {
        self.backend.dwc2_transfer_stats()
    }

    #[cfg(kmod)]
    pub fn reset_dwc2_transfer_stats(&self) {
        self.backend.reset_dwc2_transfer_stats();
    }

    pub async fn open_device(&mut self, dev: &DeviceInfo) -> Result<Device> {
        let device = self.backend.open_device(dev.inner.as_ref()).await?;
        let mut device: Device = device.into();
        device.init().await?;
        Ok(device)
    }
}

#[cfg(kmod)]
pub struct EventHandler {
    handler: Box<dyn EventHandlerOp>,
}

#[cfg(kmod)]
impl EventHandler {
    /// Captures and contains one hardware IRQ without advancing USB queues.
    pub fn capture_irq(&self) -> IrqCapture<UsbIrqEvent, UsbIrqFault> {
        self.handler.capture_irq()
    }

    /// Advances host events from one previously acknowledged IRQ snapshot.
    pub fn service_host_events(
        &self,
        event: UsbIrqEvent,
    ) -> core::result::Result<Event, UsbIrqFault> {
        self.handler.service_host_events(event)
    }

    /// Masks the exact USB interrupt source after publication cannot progress.
    pub fn contain_irq(
        &self,
        cause: ContainmentCause,
    ) -> core::result::Result<MaskedSource, UsbIrqFault> {
        self.handler.contain(cause)
    }

    /// Rearms a source after its matching event was consumed by the host owner.
    pub fn rearm_sources(&self, source: MaskedSource) -> core::result::Result<(), UsbIrqFault> {
        self.handler.rearm_sources(source)
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::{
        future::Future,
        pin::Pin,
        ptr,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    };

    use futures::{FutureExt, future::LocalBoxFuture};
    use usb_if::err::USBError;

    use super::*;
    use crate::backend::{
        BackendOp,
        ty::{DeviceOp, ProbedDeviceInfoOp},
    };

    #[derive(Default)]
    struct IrqCalls {
        init: AtomicUsize,
        enable: AtomicUsize,
        disable: AtomicUsize,
    }

    struct TestBackend {
        calls: Arc<IrqCalls>,
    }

    impl BackendOp for TestBackend {
        fn init<'a>(&'a mut self) -> futures::future::BoxFuture<'a, crate::err::Result> {
            self.calls.init.fetch_add(1, Ordering::Relaxed);
            async { Ok(()) }.boxed()
        }

        #[cfg(any(kmod, umod))]
        fn device_list<'a>(
            &'a mut self,
        ) -> futures::future::BoxFuture<'a, crate::err::Result<Vec<ProbedDeviceInfoOp>>> {
            async { Ok(Vec::new()) }.boxed()
        }

        fn open_device<'a>(
            &'a mut self,
            _dev: &'a dyn crate::backend::ty::DeviceInfoOp,
        ) -> LocalBoxFuture<'a, crate::err::Result<Box<dyn DeviceOp>>> {
            async { Err(USBError::NotSupported) }.boxed_local()
        }

        #[cfg(kmod)]
        fn create_event_handler(&mut self) -> Box<dyn crate::backend::ty::EventHandlerOp> {
            Box::new(TestEventHandler)
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

    #[cfg(kmod)]
    struct TestEventHandler;

    #[cfg(kmod)]
    impl crate::backend::ty::EventHandlerOp for TestEventHandler {
        fn capture_irq(
            &self,
        ) -> rdif_irq::IrqCapture<crate::backend::ty::UsbIrqEvent, crate::backend::ty::UsbIrqFault>
        {
            rdif_irq::IrqCapture::Unhandled
        }

        fn service_host_events(
            &self,
            _event: crate::backend::ty::UsbIrqEvent,
        ) -> core::result::Result<crate::backend::ty::Event, crate::backend::ty::UsbIrqFault>
        {
            Ok(crate::backend::ty::Event::Nothing)
        }

        fn contain(
            &self,
            _cause: rdif_irq::ContainmentCause,
        ) -> core::result::Result<rdif_irq::MaskedSource, crate::backend::ty::UsbIrqFault> {
            Err(crate::backend::ty::UsbIrqFault::StaleRearm)
        }

        fn rearm_sources(
            &self,
            _source: rdif_irq::MaskedSource,
        ) -> core::result::Result<(), crate::backend::ty::UsbIrqFault> {
            Err(crate::backend::ty::UsbIrqFault::StaleRearm)
        }
    }

    fn block_on_ready<F: Future>(mut future: F) -> F::Output {
        let waker = noop_waker();
        let mut context = Context::from_waker(&waker);
        match unsafe { Pin::new_unchecked(&mut future) }.poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("test future unexpectedly pending"),
        }
    }

    fn noop_waker() -> Waker {
        unsafe fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(ptr::null(), &VTABLE)
        }
        unsafe fn wake(_: *const ()) {}
        unsafe fn wake_by_ref(_: *const ()) {}
        unsafe fn drop(_: *const ()) {}

        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);

        unsafe { Waker::from_raw(RawWaker::new(ptr::null(), &VTABLE)) }
    }

    #[test]
    fn host_irq_control_forwards_to_backend() {
        let calls = Arc::new(IrqCalls::default());
        let mut host = USBHost {
            backend: Box::new(TestBackend {
                calls: calls.clone(),
            }),
            initialized: false,
        };

        host.enable_irq().unwrap();
        host.disable_irq().unwrap();

        assert_eq!(calls.enable.load(Ordering::Relaxed), 1);
        assert_eq!(calls.disable.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn host_init_is_idempotent() {
        let calls = Arc::new(IrqCalls::default());
        let mut host = USBHost {
            backend: Box::new(TestBackend {
                calls: calls.clone(),
            }),
            initialized: false,
        };

        block_on_ready(host.init()).unwrap();
        block_on_ready(host.init()).unwrap();

        assert_eq!(calls.init.load(Ordering::Relaxed), 1);
    }
}
