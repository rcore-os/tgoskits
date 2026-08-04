use alloc::{boxed::Box, collections::btree_map::BTreeMap, vec::Vec};

use futures::{
    FutureExt,
    future::{BoxFuture, LocalBoxFuture},
};
use id_arena::{Arena, Id};
use usb_if::{
    descriptor::{ConfigurationDescriptor, DeviceDescriptor},
    err::USBError,
};

use super::osal::Kernel;
use crate::{
    Device, DeviceAddressInfo,
    backend::{
        BackendOp,
        kmod::hub::{Hub, HubDevice, HubInfo, HubOp, PortChangeInfo},
        ty::{DeviceInfoOp, DeviceOp, EventHandlerOp, ProbedDeviceInfoOp},
    },
};

pub trait CoreOp: Send + 'static {
    /// Prepares and starts the controller while keeping its IRQ source masked.
    fn prepare_controller<'a>(&'a mut self) -> BoxFuture<'a, Result<(), USBError>>;

    fn root_hub(&mut self) -> Box<dyn HubOp>;

    fn new_addressed_device<'a>(
        &'a mut self,
        addr: DeviceAddressInfo,
    ) -> BoxFuture<'a, Result<Box<dyn DeviceOp>, USBError>>;

    fn create_event_handler(&mut self) -> Box<dyn EventHandlerOp>;

    fn enable_irq(&mut self) -> Result<(), USBError>;

    fn disable_irq(&mut self) -> Result<(), USBError>;

    fn dwc2_transfer_stats(&self) -> Option<crate::Dwc2TransferStats> {
        None
    }

    fn reset_dwc2_transfer_stats(&self) {}

    fn kernel(&self) -> &Kernel;
}

pub struct Core {
    pub(crate) backend: Box<dyn CoreOp>,
    hubs: Arena<Hub>,
    root_hub: Option<Id<Hub>>,
    pending_root_hub: Option<Box<dyn HubOp>>,
    inited_devices: BTreeMap<usize, Box<dyn DeviceOp>>,
}

impl Core {
    pub(crate) fn new(backend: impl CoreOp) -> Self {
        Self {
            root_hub: None,
            pending_root_hub: None,
            backend: Box::new(backend),
            hubs: Arena::new(),
            inited_devices: BTreeMap::new(),
        }
    }

    fn hub_infos(&self) -> BTreeMap<Id<Hub>, HubInfo> {
        let mut out = BTreeMap::new();
        for (id, hub) in self.hubs.iter() {
            let info = hub.info.clone();
            out.insert(id, info);
        }
        out
    }

    async fn _probe_devices(&mut self) -> Result<(bool, Vec<ProbedDeviceInfoOp>), USBError> {
        let mut is_have_new_hub = false;
        let mut out = Vec::new();

        let hub_ids: Vec<Id<Hub>> = self.hubs.iter().map(|(id, _)| id).collect();

        for id in hub_ids {
            let addr_infos = self.hub_changed_ports(id).await?;
            let parent_hub_id = self.hubs.get(id).unwrap().backend.slot_id();
            for addr_info in addr_infos {
                let info = DeviceAddressInfo {
                    root_port_id: addr_info.root_port_id,
                    port_speed: addr_info.port_speed,
                    parent_hub: Some(id),
                    port_id: addr_info.port_id,
                    infos: self.hub_infos(),
                };

                let device = self.backend.new_addressed_device(info).await?;

                let device_id = device.id();

                if let Some(hub_settings) =
                    HubDevice::is_hub(device.descriptor(), device.configuration_descriptors())
                {
                    let desc = device.descriptor().clone();
                    let configs = device.configuration_descriptors().to_vec();
                    let device_inner: Device = device.into();

                    let hub_device = HubDevice::new(
                        device_inner,
                        hub_settings,
                        addr_info.root_port_id,
                        parent_hub_id,
                        self.backend.kernel(),
                    )
                    .await?;
                    let mut hub = Hub::new(
                        Box::new(hub_device),
                        &self.hub_infos(),
                        addr_info.port_id,
                        Some(id),
                    );
                    let info = hub.backend.init(hub.info.clone()).await?;
                    hub.info = info;

                    let hub_id = self.hubs.alloc(hub);
                    is_have_new_hub = true;

                    let hub_info = Box::new(DeviceInfo::new(device_id, desc, &configs))
                        as Box<dyn DeviceInfoOp>;
                    out.push(ProbedDeviceInfoOp::Hub(hub_info));

                    info!("Added new hub with id {:?}", hub_id);
                } else {
                    let desc = device.descriptor().clone();
                    let configs = device.configuration_descriptors().to_vec();

                    self.inited_devices.insert(device_id, device);

                    let device_info = Box::new(DeviceInfo::new(device_id, desc, &configs))
                        as Box<dyn DeviceInfoOp>;

                    out.push(ProbedDeviceInfoOp::Device(device_info));
                }
            }
        }

        Ok((is_have_new_hub, out))
    }

    async fn hub_changed_ports(
        &mut self,
        hub_id: Id<Hub>,
    ) -> Result<Vec<PortChangeInfo>, USBError> {
        let hub = self.hubs.get_mut(hub_id).expect("Hub id should be valid");
        hub.backend.changed_ports().await
    }

    async fn probe_devices(&mut self) -> Result<Vec<ProbedDeviceInfoOp>, USBError> {
        let mut result = Vec::new();

        loop {
            let (is_have_new_hub, mut devices) = self._probe_devices().await?;
            result.append(&mut devices);
            if !is_have_new_hub {
                break;
            }
        }
        Ok(result)
    }
}

impl BackendOp for Core {
    fn init<'a>(&'a mut self) -> BoxFuture<'a, Result<(), USBError>> {
        async {
            self.backend.prepare_controller().await?;
            if let Err(error) = self.backend.enable_irq() {
                let _rollback_result = self.backend.disable_irq();
                return Err(error);
            }

            let root_hub_backend = self
                .pending_root_hub
                .take()
                .unwrap_or_else(|| self.backend.root_hub());
            let mut root_hub = Hub::new(root_hub_backend, &self.hub_infos(), 0, None);
            let info = match root_hub.backend.init(root_hub.info.clone()).await {
                Ok(info) => info,
                Err(error) => {
                    self.pending_root_hub = Some(root_hub.backend);
                    let _rollback_result = self.backend.disable_irq();
                    return Err(error);
                }
            };
            root_hub.info = info;

            let id = self.hubs.alloc(root_hub);
            self.root_hub = Some(id);
            Ok(())
        }
        .boxed()
    }

    fn device_list<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<ProbedDeviceInfoOp>, USBError>> {
        self.probe_devices().boxed()
    }

    fn open_device<'a>(
        &'a mut self,
        dev: &'a dyn crate::backend::ty::DeviceInfoOp,
    ) -> LocalBoxFuture<'a, Result<Box<dyn DeviceOp>, USBError>> {
        async {
            let device = self.inited_devices.remove(&dev.id()).unwrap_or_else(|| {
                panic!("Device id {} not found in inited_devices", dev.id());
            });

            Ok(device)
        }
        .boxed()
    }

    fn create_event_handler(&mut self) -> Box<dyn EventHandlerOp> {
        self.backend.create_event_handler()
    }

    fn enable_irq(&mut self) -> Result<(), USBError> {
        self.backend.enable_irq()
    }

    fn disable_irq(&mut self) -> Result<(), USBError> {
        self.backend.disable_irq()
    }

    fn dwc2_transfer_stats(&self) -> Option<crate::Dwc2TransferStats> {
        self.backend.dwc2_transfer_stats()
    }

    fn reset_dwc2_transfer_stats(&self) {
        self.backend.reset_dwc2_transfer_stats();
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    id: usize,
    desc: DeviceDescriptor,
    config_desc: Vec<ConfigurationDescriptor>,
}

impl DeviceInfo {
    pub fn new(id: usize, desc: DeviceDescriptor, config_desc: &[ConfigurationDescriptor]) -> Self {
        Self {
            id,
            desc,
            config_desc: config_desc.to_vec(),
        }
    }
}

impl DeviceInfoOp for DeviceInfo {
    fn id(&self) -> usize {
        self.id
    }

    fn backend_name(&self) -> &str {
        "kernel"
    }

    fn descriptor(&self) -> &DeviceDescriptor {
        &self.desc
    }

    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor] {
        &self.config_desc
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::{sync::Arc, vec, vec::Vec};
    use core::{
        future::Future,
        pin::Pin,
        ptr,
        task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    };
    use std::sync::Mutex;

    use futures::FutureExt;

    use super::*;
    use crate::backend::ty::{Event, EventHandlerOp};

    struct TestCore {
        calls: Arc<Mutex<Vec<&'static str>>>,
        fail_root_hub: bool,
    }

    impl CoreOp for TestCore {
        fn prepare_controller<'a>(&'a mut self) -> BoxFuture<'a, Result<(), USBError>> {
            self.calls.lock().unwrap().push("prepare");
            async { Ok(()) }.boxed()
        }

        fn root_hub(&mut self) -> Box<dyn HubOp> {
            Box::new(TestRootHub {
                calls: self.calls.clone(),
                fail_init: self.fail_root_hub,
            })
        }

        fn new_addressed_device<'a>(
            &'a mut self,
            _addr: DeviceAddressInfo,
        ) -> BoxFuture<'a, Result<Box<dyn DeviceOp>, USBError>> {
            async { Err(USBError::NotSupported) }.boxed()
        }

        fn create_event_handler(&mut self) -> Box<dyn EventHandlerOp> {
            Box::new(TestEventHandler)
        }

        fn enable_irq(&mut self) -> Result<(), USBError> {
            self.calls.lock().unwrap().push("enable");
            Ok(())
        }

        fn disable_irq(&mut self) -> Result<(), USBError> {
            self.calls.lock().unwrap().push("disable");
            Ok(())
        }

        fn kernel(&self) -> &Kernel {
            unreachable!("the lifecycle test does not probe devices")
        }
    }

    struct TestRootHub {
        calls: Arc<Mutex<Vec<&'static str>>>,
        fail_init: bool,
    }

    impl HubOp for TestRootHub {
        fn init<'a>(&'a mut self, info: HubInfo) -> BoxFuture<'a, Result<HubInfo, USBError>> {
            self.calls.lock().unwrap().push("root-hub-init");
            async move {
                if self.fail_init {
                    Err(USBError::NotInitialized)
                } else {
                    Ok(info)
                }
            }
            .boxed()
        }

        fn changed_ports<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<PortChangeInfo>, USBError>> {
            async { Ok(Vec::new()) }.boxed()
        }

        fn slot_id(&self) -> u8 {
            0
        }
    }

    struct TestEventHandler;

    impl EventHandlerOp for TestEventHandler {
        fn acknowledge_irq(&self) -> bool {
            false
        }

        fn drain_event(&self) -> Event {
            Event::Nothing
        }

        fn rearm_irq(&self) {}
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
    fn core_arms_irq_before_root_hub_commands() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut core = Core::new(TestCore {
            calls: calls.clone(),
            fail_root_hub: false,
        });

        block_on_ready(core.init()).unwrap();

        assert_eq!(
            *calls.lock().unwrap(),
            vec!["prepare", "enable", "root-hub-init"]
        );
    }

    #[test]
    fn root_hub_failure_masks_controller_once() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut core = Core::new(TestCore {
            calls: calls.clone(),
            fail_root_hub: true,
        });

        assert!(block_on_ready(core.init()).is_err());

        assert_eq!(
            *calls.lock().unwrap(),
            vec!["prepare", "enable", "root-hub-init", "disable"]
        );
    }
}
