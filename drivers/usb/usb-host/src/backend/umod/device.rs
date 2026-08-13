use core::{mem::MaybeUninit, num::NonZero};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    sync::Arc,
    task::Poll,
};

use futures::{FutureExt, future::poll_fn};
use libusb1_sys::{
    constants::{LIBUSB_ERROR_NO_DEVICE, LIBUSB_SUCCESS},
    *,
};
use usb_if::{
    descriptor::{
        ConfigurationDescriptor, DeviceDescriptor, InterfaceDescriptor, InterfaceDescriptors,
    },
    endpoint::EndpointInfo,
};

use super::{context::Context, endpoint::EndpointImpl};
use crate::{
    backend::ty::{
        DeviceInfoOp, DeviceOp,
        ep::{EndpointHandle, EndpointOp},
    },
    err::*,
};

pub struct DeviceInfo {
    pub(crate) raw: *mut libusb_device,
    desc: DeviceDescriptor,
    configs: Vec<ConfigurationDescriptor>,
}

impl Debug for DeviceInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeviceInfo").finish()
    }
}

unsafe impl Send for DeviceInfo {}
unsafe impl Sync for DeviceInfo {}

impl DeviceInfo {
    pub(crate) fn new(raw: *mut libusb_device) -> Result<Self> {
        let raw = unsafe { libusb_ref_device(raw) };
        let mut desc: MaybeUninit<libusb_device_descriptor> = MaybeUninit::uninit();
        usb!(libusb_get_device_descriptor(raw, desc.as_mut_ptr()))?;
        let desc = unsafe { desc.assume_init() };
        let desc = libusb_device_desc_to_desc(&desc)?;
        let mut configs = Vec::new();
        for i in 0..desc.num_configurations {
            let config_desc = libusb_get_configuration_descriptors(raw, i)?;
            configs.push(config_desc);
        }
        Ok(Self { raw, desc, configs })
    }
}

impl Drop for DeviceInfo {
    fn drop(&mut self) {
        unsafe {
            libusb_unref_device(self.raw);
        }
    }
}

impl DeviceInfoOp for DeviceInfo {
    fn id(&self) -> usize {
        self.raw as usize
    }

    fn backend_name(&self) -> &str {
        "libusb"
    }

    fn descriptor(&self) -> &DeviceDescriptor {
        &self.desc
    }

    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor] {
        &self.configs
    }
}

fn libusb_get_configuration_descriptors(
    raw: *mut libusb_device,
    index: u8,
) -> Result<ConfigurationDescriptor> {
    let mut desc: MaybeUninit<*const libusb_config_descriptor> = MaybeUninit::uninit();
    usb!(libusb_get_config_descriptor(raw, index, desc.as_mut_ptr()))?;
    let desc = unsafe { desc.assume_init() };

    if desc.is_null() {
        Err(anyhow!("Failed to get configuration descriptor",))?;
    }

    let desc = unsafe { &*desc };

    let interface_num = desc.bNumInterfaces as usize;
    let mut interfaces = Vec::with_capacity(interface_num);

    for iface_num in 0..interface_num {
        let iface_desc = unsafe { &*desc.interface.add(iface_num) };
        let alt_setting_num = iface_desc.num_altsetting as usize;
        let mut alt_settings = Vec::with_capacity(alt_setting_num);

        for alt_idx in 0..alt_setting_num {
            let alt_desc = unsafe { &*iface_desc.altsetting.add(alt_idx) };
            let endpoint_num = alt_desc.bNumEndpoints as usize;
            let mut endpoints = Vec::with_capacity(endpoint_num);

            for ep_idx in 0..endpoint_num {
                let ep_desc = unsafe { &*alt_desc.endpoint.add(ep_idx) };
                let direction = if ep_desc.bEndpointAddress & 0x80 != 0 {
                    usb_if::transfer::Direction::In
                } else {
                    usb_if::transfer::Direction::Out
                };

                let transfer_type = match ep_desc.bmAttributes & 0x03 {
                    0 => usb_if::descriptor::EndpointType::Control,
                    1 => usb_if::descriptor::EndpointType::Isochronous,
                    2 => usb_if::descriptor::EndpointType::Bulk,
                    3 => usb_if::descriptor::EndpointType::Interrupt,
                    _ => unreachable!(),
                };

                let packets_per_microframe = match transfer_type {
                    usb_if::descriptor::EndpointType::Isochronous
                    | usb_if::descriptor::EndpointType::Interrupt => {
                        (((ep_desc.wMaxPacketSize >> 11) & 0x03) + 1) as usize
                    }
                    _ => 1,
                };

                endpoints.push(usb_if::descriptor::EndpointDescriptor {
                    address: ep_desc.bEndpointAddress, // 保留完整的端点地址（包括方向位）
                    max_packet_size: ep_desc.wMaxPacketSize & 0x7FF,
                    transfer_type,
                    direction,
                    packets_per_microframe,
                    interval: ep_desc.bInterval,
                });
            }

            alt_settings.push(InterfaceDescriptor {
                interface_number: alt_desc.bInterfaceNumber,
                alternate_setting: alt_desc.bAlternateSetting,
                class: alt_desc.bInterfaceClass,
                subclass: alt_desc.bInterfaceSubClass,
                protocol: alt_desc.bInterfaceProtocol,
                string_index: NonZero::new(alt_desc.iInterface),
                string: None,
                num_endpoints: alt_desc.bNumEndpoints,
                endpoints,
            });
        }

        interfaces.push(InterfaceDescriptors {
            interface_number: unsafe {
                if !iface_desc.altsetting.is_null() {
                    (*iface_desc.altsetting).bInterfaceNumber
                } else {
                    iface_num as u8
                }
            },
            alt_settings,
        });
    }

    let out = ConfigurationDescriptor {
        num_interfaces: desc.bNumInterfaces,
        configuration_value: desc.bConfigurationValue,
        attributes: desc.bmAttributes,
        max_power: desc.bMaxPower,
        string_index: NonZero::new(desc.iConfiguration),
        string: None,
        interfaces,
        raw: Vec::new(),
    };
    unsafe { libusb_free_config_descriptor(desc) };
    Ok(out)
}

pub struct Device {
    id: usize,
    handle: Arc<DeviceHandle>,
    desc: DeviceDescriptor,
    configs: Vec<ConfigurationDescriptor>,
    ctrl_ep: EndpointHandle,
    claimed_interfaces: BTreeMap<u8, u8>,
    detached_interfaces: BTreeSet<u8>,
    eps: BTreeMap<u8, EndpointHandle>,
    ep_interfaces: BTreeMap<u8, u8>,
}

unsafe impl Send for Device {}

impl Device {
    pub(crate) fn new(info: &DeviceInfo, ctx: Arc<Context>) -> Result<Self> {
        let raw = info.raw;
        let mut handle = std::ptr::null_mut();
        usb!(libusb_open(raw, &mut handle))?;

        let desc = info.desc.clone();
        let configs = info.configs.clone();

        let handle = Arc::new(DeviceHandle {
            raw: handle,
            _ctx: ctx,
        });

        // 创建控制端点（endpoint address 0）
        let ctrl_ep_impl = EndpointImpl::new(handle.clone(), 0);
        let ctrl_ep = EndpointHandle::new(EndpointInfo::control(), ctrl_ep_impl);

        Ok(Self {
            id: info.raw as usize,
            handle,
            desc,
            configs,
            ctrl_ep,
            claimed_interfaces: BTreeMap::new(),
            detached_interfaces: BTreeSet::new(),
            eps: BTreeMap::new(),
            ep_interfaces: BTreeMap::new(),
        })
    }

    async fn _claim_interface(
        &mut self,
        interface: u8,
        alternate: u8,
    ) -> Result<BTreeMap<u8, EndpointHandle>> {
        let pending = self.prepare_endpoints(interface, alternate)?;
        let first_claim = !self.claimed_interfaces.contains_key(&interface);
        let mut detached_now = false;
        if first_claim {
            let res = usb!(libusb_kernel_driver_active(
                self.handle.raw(),
                interface as _
            ))?;
            if res == 1 {
                usb!(libusb_detach_kernel_driver(
                    self.handle.raw(),
                    interface as _
                ))?;
                detached_now = true;
                debug!("Kernel driver detached for interface {interface}");
            }
            if let Err(err) = usb!(libusb_claim_interface(self.handle.raw(), interface as _)) {
                if detached_now {
                    let _ =
                        unsafe { libusb_attach_kernel_driver(self.handle.raw(), interface as _) };
                }
                return Err(err.into());
            }
            debug!("Interface {interface} claimed successfully");
        }

        let old_endpoints = self.interface_endpoints(interface);
        for endpoint in &old_endpoints {
            endpoint.revoke();
        }
        if Self::quiesce_endpoints(old_endpoints.iter()).await.is_err() {
            if first_claim {
                self.rollback_first_claim(interface, detached_now)?;
            }
            return Err(USBError::InterfaceBroken);
        }

        if self.claimed_interfaces.get(&interface).copied() != Some(alternate)
            && let Err(err) = usb!(libusb_set_interface_alt_setting(
                self.handle.raw(),
                interface as _,
                alternate as _
            ))
        {
            for endpoint in old_endpoints {
                endpoint.reactivate();
            }
            if first_claim {
                self.rollback_first_claim(interface, detached_now)?;
            }
            return Err(err.into());
        }
        self.eps
            .retain(|address, _| self.ep_interfaces.get(address).copied() != Some(interface));
        self.ep_interfaces.retain(|_, owner| *owner != interface);
        self.ep_interfaces
            .extend(pending.keys().copied().map(|address| (address, interface)));
        self.eps.extend(pending.clone());
        if detached_now {
            self.detached_interfaces.insert(interface);
        }
        self.claimed_interfaces.insert(interface, alternate);
        Ok(pending)
    }

    fn rollback_first_claim(&self, interface: u8, detached_now: bool) -> Result<()> {
        let release = unsafe { libusb_release_interface(self.handle.raw(), interface as _) };
        if release != LIBUSB_SUCCESS && release != LIBUSB_ERROR_NO_DEVICE {
            return Err(USBError::InterfaceBroken);
        }
        if detached_now {
            let attach = unsafe { libusb_attach_kernel_driver(self.handle.raw(), interface as _) };
            if attach != LIBUSB_SUCCESS && attach != LIBUSB_ERROR_NO_DEVICE {
                return Err(USBError::InterfaceBroken);
            }
        }
        if release == LIBUSB_ERROR_NO_DEVICE {
            return Err(usb_if::err::TransferError::Disconnected.into());
        }
        Ok(())
    }

    fn prepare_endpoints(
        &self,
        interface: u8,
        alternate: u8,
    ) -> Result<BTreeMap<u8, EndpointHandle>> {
        let descriptors = self
            .configs
            .iter()
            .flat_map(|config| &config.interfaces)
            .filter(|descriptors| descriptors.interface_number == interface)
            .flat_map(|descriptors| &descriptors.alt_settings)
            .find(|descriptor| descriptor.alternate_setting == alternate)
            .ok_or(USBError::NotFound)?;
        Ok(descriptors
            .endpoints
            .iter()
            .map(|descriptor| {
                let info = EndpointInfo::from(descriptor);
                (
                    descriptor.address,
                    EndpointHandle::new(
                        info,
                        EndpointImpl::new(self.handle.clone(), descriptor.address),
                    ),
                )
            })
            .collect())
    }

    fn interface_endpoints(&self, interface: u8) -> Vec<EndpointHandle> {
        self.eps
            .iter()
            .filter(|(address, _)| self.ep_interfaces.get(address).copied() == Some(interface))
            .map(|(_, endpoint)| endpoint.clone())
            .collect()
    }

    async fn quiesce_endpoints<'a>(
        endpoints: impl Iterator<Item = &'a EndpointHandle>,
    ) -> Result<()> {
        for endpoint in endpoints {
            let ids = endpoint.with_raw_mut::<EndpointImpl, _>(|raw| raw.pending_request_ids());
            for id in ids {
                endpoint
                    .with_raw_mut::<EndpointImpl, _>(|raw| raw.cancel_request(id))
                    .map_err(USBError::from)?;
                poll_fn(|cx| {
                    endpoint.with_raw_mut::<EndpointImpl, _>(|raw| {
                        if let Some(result) = raw.reclaim_request(id) {
                            return Poll::Ready(match result {
                                Ok(_)
                                | Err(usb_if::err::TransferError::Cancelled)
                                | Err(usb_if::err::TransferError::Disconnected) => Ok(()),
                                Err(err) => Err(USBError::from(err)),
                            });
                        }
                        raw.register_waker(id, cx);
                        Poll::Pending
                    })
                })
                .await?;
            }
        }
        Ok(())
    }

    async fn _release_interface(&mut self, interface: u8) -> Result<()> {
        let endpoints = self.interface_endpoints(interface);
        for endpoint in &endpoints {
            endpoint.revoke();
        }
        if Self::quiesce_endpoints(endpoints.iter()).await.is_err() {
            return Err(USBError::InterfaceBroken);
        }
        usb!(libusb_release_interface(self.handle.raw(), interface as _))?;
        if self.detached_interfaces.remove(&interface) {
            let _ = unsafe { libusb_attach_kernel_driver(self.handle.raw(), interface as _) };
        }
        self.eps
            .retain(|address, _| self.ep_interfaces.get(address).copied() != Some(interface));
        self.ep_interfaces.retain(|_, owner| *owner != interface);
        self.claimed_interfaces.remove(&interface);
        Ok(())
    }

    async fn _disconnect(&mut self) -> Result<()> {
        let mut endpoints = self.eps.values().cloned().collect::<Vec<_>>();
        endpoints.push(self.ctrl_ep.clone());
        for endpoint in &endpoints {
            endpoint.revoke();
        }
        Self::quiesce_endpoints(endpoints.iter()).await?;

        let claimed = self.claimed_interfaces.keys().copied().collect::<Vec<_>>();
        for interface in claimed {
            let status = unsafe { libusb_release_interface(self.handle.raw(), interface as _) };
            if status != LIBUSB_SUCCESS && status != LIBUSB_ERROR_NO_DEVICE {
                let Err(err) = super::err::libusb_error_to_usb_error(status) else {
                    return Err(USBError::InvalidParameter);
                };
                return Err(err.into());
            }
        }
        self.eps.clear();
        self.ep_interfaces.clear();
        self.claimed_interfaces.clear();
        self.detached_interfaces.clear();
        Ok(())
    }
}

impl DeviceOp for Device {
    fn id(&self) -> usize {
        self.id
    }

    fn backend_name(&self) -> &str {
        "libusb"
    }

    fn descriptor(&self) -> &DeviceDescriptor {
        &self.desc
    }

    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor] {
        &self.configs
    }

    fn ctrl_ep_ref(&self) -> &EndpointHandle {
        &self.ctrl_ep
    }

    fn ctrl_ep_mut(&mut self) -> &mut EndpointHandle {
        &mut self.ctrl_ep
    }

    fn claim_interface<'a>(
        &'a mut self,
        interface: u8,
        alternate: u8,
    ) -> futures::future::BoxFuture<'a, std::result::Result<BTreeMap<u8, EndpointHandle>, USBError>>
    {
        async move { self._claim_interface(interface, alternate).await }.boxed()
    }

    fn release_interface<'a>(
        &'a mut self,
        interface: u8,
    ) -> futures::future::BoxFuture<'a, std::result::Result<(), USBError>> {
        async move { self._release_interface(interface).await }.boxed()
    }

    fn set_configuration<'a>(
        &'a mut self,
        configuration_value: u8,
    ) -> futures::future::BoxFuture<'a, std::result::Result<(), USBError>> {
        async move {
            let endpoints = self.eps.values().cloned().collect::<Vec<_>>();
            for endpoint in &endpoints {
                endpoint.revoke();
            }
            if Self::quiesce_endpoints(endpoints.iter()).await.is_err() {
                return Err(USBError::InterfaceBroken);
            }
            usb!(libusb_set_configuration(
                self.handle.raw(),
                configuration_value as _
            ))?;
            self.eps.clear();
            self.ep_interfaces.clear();
            self.claimed_interfaces.clear();
            Ok(())
        }
        .boxed()
    }

    fn disconnect(&mut self) -> futures::future::BoxFuture<'_, std::result::Result<(), USBError>> {
        self._disconnect().boxed()
    }

    fn update_hub(
        &mut self,
        _params: crate::backend::ty::HubParams,
    ) -> futures::future::BoxFuture<'_, std::result::Result<(), USBError>> {
        // libusb 运行在用户空间，由内核 USB 驱动处理 Hub 参数
        async fn update_hub_inner() -> std::result::Result<(), USBError> {
            debug!("libusb backend: Hub parameters managed by kernel");
            Ok(())
        }
        update_hub_inner().boxed()
    }
}

fn libusb_device_desc_to_desc(
    desc: &libusb_device_descriptor,
) -> crate::err::Result<DeviceDescriptor> {
    Ok(DeviceDescriptor {
        class: desc.bDeviceClass,
        subclass: desc.bDeviceSubClass,
        protocol: desc.bDeviceProtocol,
        vendor_id: desc.idVendor,
        product_id: desc.idProduct,
        manufacturer_string_index: NonZero::new(desc.iManufacturer),
        product_string_index: NonZero::new(desc.iProduct),
        serial_number_string_index: NonZero::new(desc.iSerialNumber),
        num_configurations: desc.bNumConfigurations,
        usb_version: desc.bcdUSB,
        max_packet_size_0: desc.bMaxPacketSize0,
        device_version: desc.bcdDevice,
    })
}

pub struct DeviceHandle {
    raw: *mut libusb_device_handle,
    _ctx: Arc<Context>,
}
unsafe impl Send for DeviceHandle {}
unsafe impl Sync for DeviceHandle {}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        unsafe {
            libusb_close(self.raw);
        }
    }
}

impl DeviceHandle {
    pub fn raw(&self) -> *mut libusb_device_handle {
        self.raw
    }
}
