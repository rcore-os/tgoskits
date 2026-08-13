use alloc::{boxed::Box, collections::BTreeMap, string::String, sync::Arc, vec::Vec};
use core::{
    any::Any,
    fmt::{Debug, Display},
};

use ax_sync::SpinLock;
use usb_if::{
    descriptor::{
        ConfigurationDescriptor, DescriptorType, DeviceDescriptor, InterfaceDescriptor, LanguageId,
        decode_string_descriptor,
    },
    err::{TransferError, USBError},
    host::ControlSetup,
};

use crate::backend::ty::{DeviceInfoOp, DeviceOp, ep::EndpointHandle};

pub struct DeviceInfo {
    pub(crate) inner: Box<dyn DeviceInfoOp>,
}

pub struct HubDeviceInfo {
    pub(crate) inner: Box<dyn DeviceInfoOp>,
}

pub enum ProbedDevice {
    Device(DeviceInfo),
    Hub(HubDeviceInfo),
}

pub struct ProbeChanges {
    pub connected: Vec<ProbedDevice>,
    pub disconnected: Vec<usize>,
}

impl ProbedDevice {
    pub fn id(&self) -> usize {
        match self {
            Self::Device(info) => info.id(),
            Self::Hub(info) => info.id(),
        }
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        match self {
            Self::Device(info) => info.descriptor(),
            Self::Hub(info) => info.descriptor(),
        }
    }

    pub fn configurations(&self) -> &[ConfigurationDescriptor] {
        match self {
            Self::Device(info) => info.configurations(),
            Self::Hub(info) => info.configurations(),
        }
    }

    pub fn product_id(&self) -> u16 {
        self.descriptor().product_id
    }

    pub fn vendor_id(&self) -> u16 {
        self.descriptor().vendor_id
    }

    pub fn as_device_info(&self) -> Option<&DeviceInfo> {
        match self {
            Self::Device(info) => Some(info),
            Self::Hub(_) => None,
        }
    }

    pub fn into_device_info(self) -> Option<DeviceInfo> {
        match self {
            Self::Device(info) => Some(info),
            Self::Hub(_) => None,
        }
    }
}

impl Debug for ProbedDevice {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Device(info) => f.debug_tuple("ProbedDevice::Device").field(info).finish(),
            Self::Hub(info) => f.debug_tuple("ProbedDevice::Hub").field(info).finish(),
        }
    }
}

impl Display for ProbedDevice {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Device(info) => Display::fmt(info, f),
            Self::Hub(info) => Display::fmt(info, f),
        }
    }
}

impl DeviceInfo {
    pub fn id(&self) -> usize {
        self.inner.id()
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        self.inner.descriptor()
    }

    pub fn configurations(&self) -> &[ConfigurationDescriptor] {
        self.inner.configuration_descriptors()
    }

    pub fn interface_descriptors<'a>(
        &'a self,
    ) -> impl Iterator<Item = &'a InterfaceDescriptor> + 'a {
        self.configurations().iter().flat_map(|config| {
            config
                .interfaces
                .iter()
                .flat_map(|interface| interface.alt_settings.first())
        })
    }

    pub fn product_id(&self) -> u16 {
        self.descriptor().product_id
    }

    pub fn vendor_id(&self) -> u16 {
        self.descriptor().vendor_id
    }
}

impl HubDeviceInfo {
    pub fn id(&self) -> usize {
        self.inner.id()
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        self.inner.descriptor()
    }

    pub fn configurations(&self) -> &[ConfigurationDescriptor] {
        self.inner.configuration_descriptors()
    }

    pub fn product_id(&self) -> u16 {
        self.descriptor().product_id
    }

    pub fn vendor_id(&self) -> u16 {
        self.descriptor().vendor_id
    }
}

impl Debug for DeviceInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeviceInfo")
            .field("backend", &self.inner.backend_name())
            .field("vender_id", &self.inner.descriptor().vendor_id)
            .field("product_id", &self.inner.descriptor().product_id)
            .finish()
    }
}

impl Debug for HubDeviceInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HubDeviceInfo")
            .field("backend", &self.inner.backend_name())
            .field("vender_id", &self.inner.descriptor().vendor_id)
            .field("product_id", &self.inner.descriptor().product_id)
            .finish()
    }
}

impl Display for DeviceInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:04x}:{:04x}",
            self.inner.descriptor().vendor_id,
            self.inner.descriptor().product_id
        )
    }
}

impl Display for HubDeviceInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:04x}:{:04x}",
            self.inner.descriptor().vendor_id,
            self.inner.descriptor().product_id
        )
    }
}

pub struct Device {
    pub(crate) inner: Box<dyn DeviceOp>,
    lang_id: LanguageId,
    manufacturer: Option<String>,
    claimed_interfaces: BTreeMap<u8, InterfaceRegistration>,
    lifecycle: DeviceLifecycle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InterfaceSessionState {
    Active,
    Released,
    Broken,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeviceLifecycle {
    Active,
    Broken,
    Disconnected,
}

struct InterfaceRegistration {
    alternate: u8,
    endpoints: BTreeMap<u8, EndpointHandle>,
    state: Arc<SpinLock<InterfaceSessionState>>,
}

/// Owns the active alternate setting and endpoint capabilities for one USB interface.
pub struct InterfaceSession {
    interface: u8,
    alternate: u8,
    endpoints: BTreeMap<u8, EndpointHandle>,
    state: Arc<SpinLock<InterfaceSessionState>>,
}

impl InterfaceSession {
    pub fn interface_number(&self) -> u8 {
        self.interface
    }

    pub fn alternate_setting(&self) -> u8 {
        self.alternate
    }

    /// Returns a capability for an endpoint in the active alternate setting.
    pub fn endpoint(&self, address: u8) -> Result<EndpointHandle, USBError> {
        match *self.state.lock() {
            InterfaceSessionState::Active => {}
            InterfaceSessionState::Released => return Err(USBError::NotFound),
            InterfaceSessionState::Broken => return Err(USBError::InterfaceBroken),
            InterfaceSessionState::Disconnected => {
                return Err(TransferError::Disconnected.into());
            }
        }
        self.endpoints
            .get(&address)
            .cloned()
            .ok_or(USBError::NotFound)
    }

    /// Switches this interface to another alternate setting as one HCD transaction.
    pub async fn set_alternate(
        &mut self,
        device: &mut Device,
        alternate: u8,
    ) -> Result<(), USBError> {
        self.ensure_active()?;
        device.ensure_active()?;
        self.ensure_owned_by(device)?;
        if self.alternate == alternate {
            return Ok(());
        }

        let old_endpoints = self.endpoints.clone();
        for endpoint in old_endpoints.values() {
            endpoint.revoke();
        }
        match device
            .inner
            .claim_interface(self.interface, alternate)
            .await
        {
            Ok(endpoints) => {
                let Some(registration) = device.claimed_interfaces.get_mut(&self.interface) else {
                    for endpoint in endpoints.values() {
                        endpoint.revoke();
                    }
                    *self.state.lock() = InterfaceSessionState::Broken;
                    return Err(USBError::InterfaceBroken);
                };
                if !Arc::ptr_eq(&registration.state, &self.state) {
                    for endpoint in endpoints.values() {
                        endpoint.revoke();
                    }
                    *self.state.lock() = InterfaceSessionState::Broken;
                    return Err(USBError::InterfaceBroken);
                }
                registration.alternate = alternate;
                registration.endpoints = endpoints.clone();
                self.alternate = alternate;
                self.endpoints = endpoints;
                Ok(())
            }
            Err(USBError::TransferError(TransferError::Disconnected)) => {
                for endpoint in old_endpoints.values() {
                    endpoint.disconnect();
                }
                *self.state.lock() = InterfaceSessionState::Disconnected;
                device.lifecycle = DeviceLifecycle::Disconnected;
                Err(TransferError::Disconnected.into())
            }
            Err(err) => {
                if matches!(err, USBError::InterfaceBroken) {
                    *self.state.lock() = InterfaceSessionState::Broken;
                    device.lifecycle = DeviceLifecycle::Broken;
                } else {
                    for endpoint in old_endpoints.values() {
                        endpoint.reactivate();
                    }
                }
                Err(err)
            }
        }
    }

    /// Stops all transfers and releases this interface from the host backend.
    pub async fn release(&mut self, device: &mut Device) -> Result<(), USBError> {
        self.ensure_active()?;
        device.ensure_active()?;
        self.ensure_owned_by(device)?;
        for endpoint in self.endpoints.values() {
            endpoint.revoke();
        }
        match device.inner.release_interface(self.interface).await {
            Ok(()) => {
                let owns_registration = device
                    .claimed_interfaces
                    .get(&self.interface)
                    .is_some_and(|entry| Arc::ptr_eq(&entry.state, &self.state));
                if owns_registration {
                    device.claimed_interfaces.remove(&self.interface);
                }
                self.endpoints.clear();
                *self.state.lock() = InterfaceSessionState::Released;
                Ok(())
            }
            Err(USBError::TransferError(TransferError::Disconnected)) => {
                for endpoint in self.endpoints.values() {
                    endpoint.disconnect();
                }
                *self.state.lock() = InterfaceSessionState::Disconnected;
                device.lifecycle = DeviceLifecycle::Disconnected;
                Err(TransferError::Disconnected.into())
            }
            Err(err) => {
                if matches!(err, USBError::InterfaceBroken) {
                    *self.state.lock() = InterfaceSessionState::Broken;
                    device.lifecycle = DeviceLifecycle::Broken;
                } else {
                    for endpoint in self.endpoints.values() {
                        endpoint.reactivate();
                    }
                }
                Err(err)
            }
        }
    }

    fn ensure_active(&self) -> Result<(), USBError> {
        match *self.state.lock() {
            InterfaceSessionState::Active => Ok(()),
            InterfaceSessionState::Released => Err(USBError::InvalidParameter),
            InterfaceSessionState::Broken => Err(USBError::InterfaceBroken),
            InterfaceSessionState::Disconnected => Err(TransferError::Disconnected.into()),
        }
    }

    fn ensure_owned_by(&self, device: &Device) -> Result<(), USBError> {
        let Some(registration) = device.claimed_interfaces.get(&self.interface) else {
            return Err(USBError::InvalidParameter);
        };
        if Arc::ptr_eq(&registration.state, &self.state) && registration.alternate == self.alternate
        {
            Ok(())
        } else {
            Err(USBError::InvalidParameter)
        }
    }
}

impl Debug for Device {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Device")
            .field("backend", &self.inner.backend_name())
            .field("vender_id", &self.inner.descriptor().vendor_id)
            .field("product_id", &self.inner.descriptor().product_id)
            .finish()
    }
}

impl<T: DeviceOp> From<T> for Device {
    fn from(inner: T) -> Self {
        Self {
            inner: Box::new(inner),
            claimed_interfaces: BTreeMap::new(),
            lifecycle: DeviceLifecycle::Active,
            lang_id: LanguageId::default(),
            manufacturer: None,
        }
    }
}

impl From<Box<dyn DeviceOp>> for Device {
    fn from(inner: Box<dyn DeviceOp>) -> Self {
        Self {
            inner,
            claimed_interfaces: BTreeMap::new(),
            lifecycle: DeviceLifecycle::Active,
            lang_id: LanguageId::default(),
            manufacturer: None,
        }
    }
}

impl Device {
    pub(crate) async fn init(&mut self) -> Result<(), USBError> {
        self.manufacturer = self.read_manufacturer().await;
        Ok(())
    }

    pub fn product_id(&self) -> u16 {
        self.descriptor().product_id
    }

    pub fn vendor_id(&self) -> u16 {
        self.descriptor().vendor_id
    }

    pub fn slot_id(&self) -> u8 {
        self.inner.id() as _
    }

    pub async fn claim_interface(
        &mut self,
        interface: u8,
        alternate: u8,
    ) -> Result<InterfaceSession, USBError> {
        self.ensure_active()?;
        trace!("Claiming interface {interface}, alternate {alternate}");
        if self.claimed_interfaces.contains_key(&interface) {
            return Err(USBError::InvalidParameter);
        }
        let endpoints = match self.inner.claim_interface(interface, alternate).await {
            Ok(endpoints) => endpoints,
            Err(USBError::InterfaceBroken) => {
                self.lifecycle = DeviceLifecycle::Broken;
                return Err(USBError::InterfaceBroken);
            }
            Err(USBError::TransferError(TransferError::Disconnected)) => {
                self.ctrl_ep_ref().disconnect();
                self.lifecycle = DeviceLifecycle::Disconnected;
                return Err(TransferError::Disconnected.into());
            }
            Err(err) => return Err(err),
        };
        let state = Arc::new(SpinLock::new(InterfaceSessionState::Active));
        self.claimed_interfaces.insert(
            interface,
            InterfaceRegistration {
                alternate,
                endpoints: endpoints.clone(),
                state: state.clone(),
            },
        );
        Ok(InterfaceSession {
            interface,
            alternate,
            endpoints,
            state,
        })
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        self.inner.descriptor()
    }

    pub fn configurations(&self) -> &[ConfigurationDescriptor] {
        self.inner.configuration_descriptors()
    }

    pub fn manufacturer(&self) -> Option<&str> {
        self.manufacturer.as_deref()
    }

    pub async fn set_configuration(&mut self, configuration_value: u8) -> crate::err::Result {
        self.ensure_active()?;
        let registrations = self
            .claimed_interfaces
            .values()
            .map(|entry| (entry.state.clone(), entry.endpoints.clone()))
            .collect::<Vec<_>>();
        for (_, endpoints) in &registrations {
            for endpoint in endpoints.values() {
                endpoint.revoke();
            }
        }
        let result = self.inner.set_configuration(configuration_value).await;
        match &result {
            Ok(()) => {
                for (state, _) in registrations {
                    *state.lock() = InterfaceSessionState::Released;
                }
                self.claimed_interfaces.clear();
            }
            Err(USBError::InterfaceBroken) => {
                for (state, _) in registrations {
                    *state.lock() = InterfaceSessionState::Broken;
                }
                self.lifecycle = DeviceLifecycle::Broken;
            }
            Err(USBError::TransferError(TransferError::Disconnected)) => {
                for (state, endpoints) in registrations {
                    for endpoint in endpoints.values() {
                        endpoint.disconnect();
                    }
                    *state.lock() = InterfaceSessionState::Disconnected;
                }
                self.ctrl_ep_ref().disconnect();
                self.lifecycle = DeviceLifecycle::Disconnected;
            }
            Err(_) => {
                for (_, endpoints) in registrations {
                    for endpoint in endpoints.values() {
                        endpoint.reactivate();
                    }
                }
            }
        }
        result
    }

    /// Stops all HCD activity for this device and revokes every published endpoint.
    pub async fn disconnect(&mut self) -> crate::err::Result {
        match self.lifecycle {
            DeviceLifecycle::Disconnected => return Ok(()),
            DeviceLifecycle::Broken => return Err(USBError::InterfaceBroken),
            DeviceLifecycle::Active => {}
        }

        let registrations = self
            .claimed_interfaces
            .values()
            .map(|entry| (entry.state.clone(), entry.endpoints.clone()))
            .collect::<Vec<_>>();
        for (_, endpoints) in &registrations {
            for endpoint in endpoints.values() {
                endpoint.revoke();
            }
        }
        self.ctrl_ep_ref().revoke();

        match self.inner.disconnect().await {
            Ok(()) => {
                for (state, endpoints) in registrations {
                    for endpoint in endpoints.values() {
                        endpoint.disconnect();
                    }
                    *state.lock() = InterfaceSessionState::Disconnected;
                }
                self.ctrl_ep_ref().disconnect();
                self.claimed_interfaces.clear();
                self.lifecycle = DeviceLifecycle::Disconnected;
                Ok(())
            }
            Err(err) => {
                for (state, _) in registrations {
                    *state.lock() = InterfaceSessionState::Broken;
                }
                self.lifecycle = DeviceLifecycle::Broken;
                Err(err)
            }
        }
    }

    pub fn ctrl_ep_ref(&self) -> &EndpointHandle {
        self.inner.ctrl_ep_ref()
    }

    pub fn ctrl_ep_mut(&mut self) -> &mut EndpointHandle {
        self.inner.ctrl_ep_mut()
    }

    fn ensure_active(&self) -> Result<(), USBError> {
        match self.lifecycle {
            DeviceLifecycle::Active => Ok(()),
            DeviceLifecycle::Broken => Err(USBError::InterfaceBroken),
            DeviceLifecycle::Disconnected => Err(TransferError::Disconnected.into()),
        }
    }

    async fn read_manufacturer(&mut self) -> Option<String> {
        let idx = self.descriptor().manufacturer_string_index?;
        self.string_descriptor(idx.get()).await.ok()
    }

    pub fn lang_id(&self) -> LanguageId {
        self.lang_id
    }

    pub fn set_lang_id(&mut self, lang_id: LanguageId) {
        self.lang_id = lang_id;
    }

    pub async fn string_descriptor(&mut self, index: u8) -> Result<String, USBError> {
        let mut data = alloc::vec![0u8; 256];
        let lang_id = self.lang_id();
        let len = self
            .ctrl_ep_mut()
            .get_descriptor(DescriptorType::STRING, index, lang_id.into(), &mut data)
            .await?;
        let descriptor_len = data
            .first()
            .copied()
            .map(usize::from)
            .unwrap_or(0)
            .min(len)
            .min(data.len());
        decode_string_descriptor(&data[..descriptor_len]).map_err(USBError::from)
    }

    pub async fn control_in(
        &mut self,
        param: ControlSetup,
        buff: &mut [u8],
    ) -> Result<usize, TransferError> {
        self.ctrl_ep_mut().control_in(param, buff).await
    }

    pub async fn control_out(
        &mut self,
        param: ControlSetup,
        buff: &[u8],
    ) -> Result<usize, TransferError> {
        self.ctrl_ep_mut().control_out(param, buff).await
    }

    pub async fn update_hub(
        &mut self,
        params: crate::backend::ty::HubParams,
    ) -> Result<(), USBError> {
        self.inner.update_hub(params).await
    }

    pub async fn current_configuration_descriptor(
        &mut self,
    ) -> Result<ConfigurationDescriptor, USBError> {
        let value = self.ctrl_ep_mut().get_configuration().await?;
        if value == 0 {
            return Err(USBError::NotFound);
        }
        for config in self.configurations() {
            if config.configuration_value == value {
                return Ok(config.clone());
            }
        }
        Err(USBError::NotFound)
    }

    #[allow(unused)]
    pub(crate) fn as_raw<T: DeviceOp>(&self) -> &T {
        (self.inner.as_ref() as &dyn Any)
            .downcast_ref::<T>()
            .unwrap()
    }

    #[allow(unused)]
    pub(crate) fn as_raw_mut<T: DeviceOp>(&mut self) -> &mut T {
        (self.inner.as_mut() as &mut dyn Any)
            .downcast_mut::<T>()
            .unwrap()
    }
}

impl Display for Device {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:04x}:{:04x}",
            self.inner.descriptor().vendor_id,
            self.inner.descriptor().product_id
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::BTreeMap, vec::Vec};
    use core::{
        future::Future,
        pin::Pin,
        ptr,
        task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    };

    use futures::{FutureExt, future::BoxFuture};
    use usb_if::{
        descriptor::{ConfigurationDescriptor, DeviceDescriptor, EndpointType},
        endpoint::{EndpointAddress, EndpointInfo, RequestId, TransferCompletion, TransferRequest},
        transfer::Direction,
    };

    use super::{Device, InterfaceSession};
    use crate::{
        backend::ty::{
            DeviceOp, HubParams,
            ep::{EndpointHandle, EndpointOp},
        },
        err::{TransferError, USBError},
    };

    struct TestEndpoint;

    impl EndpointOp for TestEndpoint {
        fn submit_request(
            &mut self,
            _request: TransferRequest,
        ) -> Result<RequestId, TransferError> {
            Ok(RequestId::new(1))
        }

        fn reclaim_request(
            &mut self,
            _id: RequestId,
        ) -> Option<Result<TransferCompletion, TransferError>> {
            None
        }

        fn register_waker(&self, _id: RequestId, _cx: &mut Context<'_>) {}
    }

    struct TestDevice {
        descriptor: DeviceDescriptor,
        configurations: Vec<ConfigurationDescriptor>,
        control: EndpointHandle,
        rejected_alternate: Option<u8>,
    }

    impl TestDevice {
        fn new(rejected_alternate: Option<u8>) -> Self {
            Self {
                descriptor: DeviceDescriptor {
                    usb_version: 0x0200,
                    class: 0,
                    subclass: 0,
                    protocol: 0,
                    max_packet_size_0: 64,
                    vendor_id: 1,
                    product_id: 1,
                    device_version: 0x0100,
                    manufacturer_string_index: None,
                    product_string_index: None,
                    serial_number_string_index: None,
                    num_configurations: 0,
                },
                configurations: Vec::new(),
                control: EndpointHandle::new(EndpointInfo::control(), TestEndpoint),
                rejected_alternate,
            }
        }

        fn data_endpoint() -> EndpointHandle {
            EndpointHandle::new(
                EndpointInfo {
                    address: EndpointAddress::new(1),
                    transfer_type: EndpointType::Bulk,
                    direction: Direction::Out,
                    max_packet_size: 512,
                    packets_per_microframe: 1,
                    interval: 0,
                },
                TestEndpoint,
            )
        }
    }

    impl DeviceOp for TestDevice {
        fn id(&self) -> usize {
            1
        }

        fn backend_name(&self) -> &str {
            "test"
        }

        fn descriptor(&self) -> &DeviceDescriptor {
            &self.descriptor
        }

        fn configuration_descriptors(&self) -> &[ConfigurationDescriptor] {
            &self.configurations
        }

        fn ctrl_ep_ref(&self) -> &EndpointHandle {
            &self.control
        }

        fn ctrl_ep_mut(&mut self) -> &mut EndpointHandle {
            &mut self.control
        }

        fn claim_interface<'a>(
            &'a mut self,
            _interface: u8,
            alternate: u8,
        ) -> BoxFuture<'a, Result<BTreeMap<u8, EndpointHandle>, USBError>> {
            let rejected = self.rejected_alternate == Some(alternate);
            async move {
                if rejected {
                    return Err(USBError::InvalidParameter);
                }
                Ok(BTreeMap::from([(1, Self::data_endpoint())]))
            }
            .boxed()
        }

        fn release_interface<'a>(
            &'a mut self,
            _interface: u8,
        ) -> BoxFuture<'a, Result<(), USBError>> {
            async { Ok(()) }.boxed()
        }

        fn set_configuration<'a>(
            &'a mut self,
            _configuration_value: u8,
        ) -> BoxFuture<'a, Result<(), USBError>> {
            async { Ok(()) }.boxed()
        }

        fn disconnect(&mut self) -> BoxFuture<'_, Result<(), USBError>> {
            async { Ok(()) }.boxed()
        }

        fn update_hub(&mut self, _params: HubParams) -> BoxFuture<'_, Result<(), USBError>> {
            async { Ok(()) }.boxed()
        }
    }

    #[test]
    fn alternate_commit_revokes_old_handle_and_publishes_new_endpoint() {
        let (mut device, mut session) = claimed_session(None);
        let old_endpoint = session.endpoint(1).unwrap();

        block_on_ready(session.set_alternate(&mut device, 1)).unwrap();

        assert!(matches!(
            old_endpoint.submit(TransferRequest::bulk_out(&[])),
            Err(TransferError::EndpointRevoked)
        ));
        assert!(
            session
                .endpoint(1)
                .unwrap()
                .submit(TransferRequest::bulk_out(&[]))
                .is_ok()
        );
    }

    #[test]
    fn alternate_failure_reactivates_old_handle() {
        let (mut device, mut session) = claimed_session(Some(2));
        let old_endpoint = session.endpoint(1).unwrap();

        assert!(matches!(
            block_on_ready(session.set_alternate(&mut device, 2)),
            Err(USBError::InvalidParameter)
        ));

        assert!(old_endpoint.submit(TransferRequest::bulk_out(&[])).is_ok());
        assert_eq!(session.alternate_setting(), 0);
    }

    #[test]
    fn session_rejects_a_different_device_before_freezing_endpoints() {
        let (_owner, mut session) = claimed_session(None);
        let endpoint = session.endpoint(1).unwrap();
        let mut different_device = Device::from(TestDevice::new(None));

        assert!(matches!(
            block_on_ready(session.set_alternate(&mut different_device, 1)),
            Err(USBError::InvalidParameter)
        ));
        assert!(endpoint.submit(TransferRequest::bulk_out(&[])).is_ok());
    }

    #[test]
    fn disconnect_marks_session_and_old_handle_disconnected() {
        let (mut device, session) = claimed_session(None);
        let old_endpoint = session.endpoint(1).unwrap();

        block_on_ready(device.disconnect()).unwrap();

        assert!(matches!(
            old_endpoint.submit(TransferRequest::bulk_out(&[])),
            Err(TransferError::Disconnected)
        ));
        assert!(matches!(
            session.endpoint(1),
            Err(USBError::TransferError(TransferError::Disconnected))
        ));
    }

    fn claimed_session(rejected_alternate: Option<u8>) -> (Device, InterfaceSession) {
        let mut device = Device::from(TestDevice::new(rejected_alternate));
        let session = block_on_ready(device.claim_interface(0, 0)).unwrap();
        (device, session)
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
}
