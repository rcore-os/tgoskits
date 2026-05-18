use alloc::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};
use core::{
    any::Any,
    fmt::{Debug, Display},
};

use usb_if::{
    descriptor::{
        ConfigurationDescriptor, DescriptorType, DeviceDescriptor, InterfaceDescriptor, LanguageId,
        decode_string_descriptor,
    },
    err::{TransferError, USBError},
    host::ControlSetup,
};

use crate::backend::ty::{DeviceInfoOp, DeviceOp, ep::Endpoint};

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
    claimed_interfaces: BTreeMap<u8, u8>,
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

    pub async fn claim_interface(&mut self, interface: u8, alternate: u8) -> Result<(), USBError> {
        trace!("Claiming interface {interface}, alternate {alternate}");
        self.inner.claim_interface(interface, alternate).await?;
        self.claimed_interfaces.insert(interface, alternate);
        Ok(())
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
        let result = self.inner.set_configuration(configuration_value).await;
        if result.is_ok() {
            self.claimed_interfaces.clear();
        }
        result
    }

    pub fn ctrl_ep_ref(&self) -> &Endpoint {
        self.inner.ctrl_ep_ref()
    }

    pub fn ctrl_ep_mut(&mut self) -> &mut Endpoint {
        self.inner.ctrl_ep_mut()
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

    pub fn endpoint(&mut self, address: u8) -> Result<Endpoint, USBError> {
        if address == 0 {
            return Err(USBError::NotFound);
        }
        let ep_desc = self.find_ep_desc(address)?.clone();
        self.inner.endpoint(&ep_desc)
    }

    pub fn take_endpoints_for_interface(
        &mut self,
        interface: u8,
    ) -> Result<BTreeMap<u8, Endpoint>, USBError> {
        let descriptors = self.current_endpoint_descriptors(interface)?;
        let mut endpoints = BTreeMap::new();
        for desc in descriptors {
            let address = desc.address;
            endpoints.insert(address, self.inner.endpoint(&desc)?);
        }
        Ok(endpoints)
    }

    pub fn take_endpoints(&mut self) -> Result<BTreeMap<u8, Endpoint>, USBError> {
        let mut endpoints = BTreeMap::new();
        let interfaces = self.claimed_interfaces.keys().copied().collect::<Vec<_>>();
        for interface in interfaces {
            endpoints.extend(self.take_endpoints_for_interface(interface)?);
        }
        Ok(endpoints)
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

    fn find_ep_desc(
        &self,
        address: u8,
    ) -> core::result::Result<&usb_if::descriptor::EndpointDescriptor, USBError> {
        for interface in self.claimed_interfaces.keys().copied() {
            if let Ok(desc) =
                self.current_endpoint_descriptors_ref(interface)
                    .and_then(|descriptors| {
                        descriptors
                            .iter()
                            .find(|ep| ep.address == address)
                            .ok_or(USBError::NotFound)
                    })
            {
                return Ok(desc);
            }
        }
        Err(USBError::NotFound)
    }

    fn current_endpoint_descriptors(
        &self,
        interface_number: u8,
    ) -> core::result::Result<Vec<usb_if::descriptor::EndpointDescriptor>, USBError> {
        Ok(self
            .current_endpoint_descriptors_ref(interface_number)?
            .to_vec())
    }

    fn current_endpoint_descriptors_ref(
        &self,
        interface_number: u8,
    ) -> core::result::Result<&[usb_if::descriptor::EndpointDescriptor], USBError> {
        let alternate_setting = self
            .claimed_interfaces
            .get(&interface_number)
            .ok_or(USBError::NotFound)?;
        for config in self.configurations() {
            for interface in &config.interfaces {
                if interface.interface_number == interface_number {
                    for alt in &interface.alt_settings {
                        if alt.alternate_setting == *alternate_setting {
                            return Ok(&alt.endpoints);
                        }
                    }
                }
            }
        }
        Err(USBError::NotFound)
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
