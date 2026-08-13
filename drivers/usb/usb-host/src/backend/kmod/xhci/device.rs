use alloc::{collections::BTreeMap, sync::Arc, vec, vec::Vec};

use ax_sync::SpinLock as Mutex;
use futures::{FutureExt, future::BoxFuture};
use mbarrier::mb;
use usb_if::{
    descriptor::{
        ConfigurationDescriptor, DescriptorType, DeviceDescriptor, DeviceDescriptorBase,
        EndpointDescriptor, EndpointType,
    },
    endpoint::EndpointInfo,
    err::USBError,
    host::{ControlSetup, hub::Speed},
    transfer::{Recipient, RequestType},
};
use xhci::ring::trb::command;

use super::{
    SlotId, Xhci,
    cmd::CommandRing,
    context::ContextData,
    endpoint::{Endpoint as XhciEndpoint, EndpointDescriptorExt},
    parse_default_max_packet_size_from_port_speed,
    reg::SlotBell,
    transfer::TransferResultHandler,
};
use crate::{
    DeviceAddressInfo,
    backend::{
        Dci,
        ty::{DeviceOp, HubParams, ep::EndpointHandle},
    },
    err::Result,
    osal::Kernel,
};

fn endpoint_address_dci(address: u8) -> u8 {
    let endpoint_number = address & 0x0f;
    endpoint_number * 2 + u8::from(address & 0x80 != 0)
}

pub struct Device {
    id: SlotId,
    ctx: ContextData,
    desc: DeviceDescriptor,
    ctrl_ep: Option<EndpointHandle>,
    transfer_result_handler: TransferResultHandler,
    bell: Arc<Mutex<SlotBell>>,
    kernel: Kernel,
    current_config_value: Option<u8>,
    config_desc: Vec<ConfigurationDescriptor>,
    port_speed: Speed,
    eps: BTreeMap<u8, EndpointHandle>,
    ep_interfaces: BTreeMap<u8, u8>,
    interface_alternates: BTreeMap<u8, u8>,
    quarantined_eps: Vec<EndpointHandle>,
    cmd: CommandRing,
}

impl Device {
    pub(crate) async fn new(host: &mut Xhci) -> Result<Self> {
        let slot_id = host.device_slot_assignment().await?;
        debug!("Slot {slot_id} assigned");
        let is_64 = host.is_64bit_ctx();
        debug!(
            "Creating new context for slot {slot_id}, {}",
            if is_64 { "64-bit" } else { "32-bit" }
        );
        let dma = host.kernel.clone();
        let ctx = host.dev_mut()?.new_ctx(slot_id, is_64, &dma)?;
        let bell = host.new_slot_bell(slot_id);
        let bell = Arc::new(Mutex::new(bell));
        // let port_speed = host.port_speed(port);
        let desc = unsafe { core::mem::zeroed() };

        Ok(Self {
            id: slot_id,
            ctx,
            bell,
            ctrl_ep: None,
            desc,
            kernel: dma,
            transfer_result_handler: host.transfer_result_handler.clone(),
            current_config_value: None,
            config_desc: vec![],
            port_speed: Speed::Full,
            eps: BTreeMap::new(),
            ep_interfaces: BTreeMap::new(),
            interface_alternates: BTreeMap::new(),
            quarantined_eps: Vec::new(),
            cmd: host.cmd.clone(),
        })
    }

    fn create_ep(&self, dci: Dci) -> Result<XhciEndpoint> {
        XhciEndpoint::new(
            self.id,
            dci,
            &self.kernel,
            self.bell.clone(),
            self.cmd.clone(),
        )
    }

    fn create_registered_ep(&self, dci: Dci) -> Result<XhciEndpoint> {
        let ep = self.create_ep(dci)?;
        self.transfer_result_handler
            .register_queue(self.id.as_u8(), dci.as_u8(), ep.ring())?;
        Ok(ep)
    }

    fn control_endpoint(&self) -> &EndpointHandle {
        self.ctrl_ep.as_ref().unwrap()
    }

    fn control_endpoint_mut(&mut self) -> &mut EndpointHandle {
        self.ctrl_ep.as_mut().unwrap()
    }

    pub(crate) async fn init(&mut self, host: &mut Xhci, info: &DeviceAddressInfo) -> Result {
        // Keep the raw PORTSC.PortSpeed encoding for interval calculations
        self.port_speed = info.port_speed;
        // let speed = info.port_speed.to_xhci_portsc_value();

        let ep = self.create_registered_ep(Dci::CTRL)?;
        self.ctrl_ep = Some(EndpointHandle::new(EndpointInfo::control(), ep));
        self.address(host, info).await?;
        // self.dump_device_out();
        let base = self.get_device_descriptor_base().await?;
        debug!("Device Descriptor Base: {:#x?}", base);

        self.setup_max_packet(base).await?;

        // 读取当前配置（应该返回 0，表示未配置）
        let current_config = self.get_configuration().await?;
        debug!("Current configuration value: {}", current_config);

        self.read_descriptor().await?;

        // 读取所有配置描述符
        for i in 0..self.desc.num_configurations {
            let config_desc = self
                .control_endpoint_mut()
                .get_configuration_descriptor(i)
                .await?;
            self.config_desc.push(config_desc);
        }

        // 设置配置为第一个配置（大多数设备只有一个配置）
        // 参考 USB 2.0 规范第 9.1.1 节和 u-boot 的 usb_set_configure_device
        if !self.config_desc.is_empty() {
            let config_value = self.config_desc[0].configuration_value;
            debug!("Setting device configuration to {}", config_value);
            self._set_configuration(config_value).await?;
        }

        debug!("device descriptor ok");
        Ok(())
    }

    async fn evaluate(&mut self) -> Result {
        mb();
        debug!("Evaluating context for slot {}", self.id.as_u8());
        let _result = self
            .cmd
            .cmd_request(command::Allowed::EvaluateContext(
                *command::EvaluateContext::default()
                    .set_slot_id(self.id.into())
                    .set_input_context_pointer(self.ctx.input_bus_addr()),
            ))
            .await?;
        debug!("Evaluate context ok");
        Ok(())
    }

    async fn setup_max_packet(&mut self, desc: DeviceDescriptorBase) -> Result {
        self.ctx.perper_change();
        // USB 设备描述符的 bMaxPacketSize0 字段（偏移 7）
        // 对于控制端点，这是直接的字节数值，不需要解码
        let packet_size = if desc.max_packet_size_0 == 0 {
            8u8
        } else {
            desc.max_packet_size_0
        } as u16;

        let dci = Dci::CTRL;
        self.ctx.with_input(|input| {
            input.control_mut().set_add_context_flag(1); // Endpoint 0 Context

            let endpoint = input.device_mut().endpoint_mut(dci.as_usize());
            endpoint.set_max_packet_size(packet_size);
        });

        self.evaluate().await?;

        Ok(())
    }

    async fn address(&mut self, host: &mut Xhci, info: &DeviceAddressInfo) -> Result {
        // 直接使用 DeviceSpeed 枚举计算默认 max packet size
        let max_packet_size = parse_default_max_packet_size_from_port_speed(info.port_speed);

        // Route String 由拓扑决定（root hub 端口不计入）
        let mut route_string = 0u32;
        let mut parent_id = info.parent_hub;
        let mut port_id = info.port_id;

        while let Some(pid) = parent_id {
            let parent_hub = info.infos.get(&pid).unwrap();
            if parent_hub.hub_depth == -1 {
                break;
            }
            if port_id > 15 {
                port_id = 15;
            }
            route_string |= (port_id as u32) << (parent_hub.hub_depth * 4);
            port_id = parent_hub.port_id;
            parent_id = parent_hub.parent;
        }

        let ctrl_ring_addr = self
            .control_endpoint_mut()
            .with_raw_mut::<XhciEndpoint, _>(|ep| ep.bus_addr());
        // ctrl dci
        let dci = Dci::CTRL;
        // 1. Allocate an Input Context data structure (6.2.5) and initialize all fields to
        // ‘0’.
        self.ctx.with_empty_input(|input| {
            let control_context = input.control_mut();
            // Initialize the Input Control Context (6.2.5.1) of the Input Context by
            // setting the A0 and A1 flags to ‘1’. These flags indicate that the Slot
            // Context and the Endpoint 0 Context of the Input Context are affected by
            // the command.
            control_context.set_add_context_flag(0);
            control_context.set_add_context_flag(1);
            for i in 2..32 {
                control_context.clear_drop_context_flag(i);
            }

            // Initialize the Input Slot Context data structure (6.2.2).
            // • Root Hub Port Number = Topology defined.
            // • Route String = Topology defined. Refer to section 8.9 in the USB3 spec. Note
            // that the Route String does not include the Root Hub Port Number.
            // • Context Entries = 1.
            let slot_context = input.device_mut().slot_mut();
            slot_context.clear_multi_tt();
            slot_context.clear_hub();
            slot_context.set_route_string(route_string);
            slot_context.set_context_entries(1);
            slot_context.set_max_exit_latency(0);
            slot_context.set_root_hub_port_number(info.root_port_id);
            slot_context.set_number_of_ports(0);
            slot_context.set_parent_hub_slot_id(0);

            // TT info is only valid for LS/FS devices behind a HS hub.
            if matches!(info.port_speed, Speed::Low | Speed::Full) {
                let mut parent_id = info.parent_hub;
                let mut tt_port = info.port_id;
                let mut hs_parent = None;

                while let Some(p) = parent_id {
                    let parent_hub = info.infos.get(&p).unwrap();
                    if parent_hub.hub_depth == -1 {
                        break;
                    }
                    if matches!(parent_hub.speed, Speed::High) {
                        hs_parent = Some(p);
                        break;
                    }
                    tt_port = parent_hub.port_id;
                    parent_id = parent_hub.parent;
                }

                if let Some(hs_id) = hs_parent {
                    let parent = info.infos.get(&hs_id).unwrap();
                    let slot_id = parent.slot_id;
                    if parent.tt.multi {
                        slot_context.set_multi_tt();
                    }

                    slot_context.set_parent_hub_slot_id(slot_id);
                    slot_context.set_parent_port_number(tt_port);
                    debug!(
                        "Setting parent_port_number (TT): {}, parent_hub_slot_id: {}",
                        tt_port, slot_id
                    );
                }
            }

            slot_context.set_tt_think_time(0);
            slot_context.set_interrupter_target(0);
            // 转换为 xHCI Slot Context 速度值
            slot_context.set_speed(info.port_speed.to_xhci_slot_value());

            // Initialize the Input default control Endpoint 0 Context (6.2.3).
            let endpoint_0 = input.device_mut().endpoint_mut(dci.as_usize());
            // • EP Type = Control.
            endpoint_0.set_endpoint_type(xhci::context::EndpointType::Control);
            // • Max Packet Size = The default maximum packet size for the Default Control EndpointHandle,
            //   as function of the PORTSC Port Speed field.
            endpoint_0.set_max_packet_size(max_packet_size);
            // • Max Burst Size = 0.
            endpoint_0.set_max_burst_size(0);
            // • TR Dequeue Pointer = Start address of first segment of the Default Control
            //   EndpointHandle Transfer Ring.
            endpoint_0.set_tr_dequeue_pointer(ctrl_ring_addr.raw());
            // • Dequeue Cycle State (DCS) = 1. Reflects Cycle bit state for valid TRBs written
            //   by software.
            // if ring_cycle_bit {
            endpoint_0.set_dequeue_cycle_state();
            // } else {
            //     endpoint_0.clear_dequeue_cycle_state();
            // }
            // • Interval = 0.
            endpoint_0.set_interval(0);
            // • Max Primary Streams (MaxPStreams) = 0.
            endpoint_0.set_max_primary_streams(0);
            // • Mult = 0.
            endpoint_0.set_mult(0);
            // • Error Count (CErr) = 3.
            endpoint_0.set_error_count(3);
            // • Average TRB Length = 8 (xHCI spec 6.2.3).
            endpoint_0.set_average_trb_length(8);
        });

        debug!(
            r#"Address device {:?}
    root port: {}
    route string: {:#x}
    ctrl ring: {:x?}
    port speed: {:?}
    max packet size: {}"#,
            self.id,
            info.root_port_id,
            route_string,
            ctrl_ring_addr,
            info.port_speed,
            max_packet_size
        );

        mb();

        let input_bus_addr = self.ctx.input_bus_addr();
        trace!("Input context bus address: {input_bus_addr:#x?}");
        let result = host
            .cmd_request(command::Allowed::AddressDevice(
                *command::AddressDevice::new()
                    .set_slot_id(self.id.into())
                    .set_input_context_pointer(input_bus_addr),
            ))
            .await?;

        debug!("Address slot ok {result:x?}");

        Ok(())
    }

    async fn read_descriptor(&mut self) -> Result<()> {
        self.desc = self.control_endpoint_mut().get_device_descriptor().await?;
        Ok(())
    }
    async fn get_device_descriptor_base(&mut self) -> Result<DeviceDescriptorBase> {
        let mut data = vec![0u8; 8];

        // DMA 传输
        let actual = self
            .control_endpoint_mut()
            .get_descriptor(DescriptorType::DEVICE, 0, 0, data.as_mut_slice())
            .await?;
        if actual != data.len() {
            return Err(anyhow!(
                "short device descriptor header: expected {} bytes, got {actual}",
                data.len()
            )
            .into());
        }

        let desc = unsafe { (data.as_ptr() as *const DeviceDescriptorBase).read_unaligned() };

        Ok(desc)
    }

    async fn get_configuration(&mut self) -> Result<u8> {
        let val = self.control_endpoint_mut().get_configuration().await?;
        self.current_config_value = Some(val);
        Ok(val)
    }

    async fn _set_configuration(&mut self, configuration_value: u8) -> Result {
        let old_endpoints = self.eps.clone();
        let old_descriptors = self
            .interface_alternates
            .iter()
            .map(|(interface, alternate)| {
                self.find_interface_endpoints(*interface, *alternate)
                    .map(<[_]>::to_vec)
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        for endpoint in old_endpoints.values() {
            endpoint.revoke();
        }
        if self.stop_endpoints(old_endpoints.values()).await.is_err() {
            return Err(USBError::InterfaceBroken);
        }

        self.prepare_configure_context(0, 0, &old_descriptors, &[], &BTreeMap::new());
        if self.configure_endpoint().await.is_err() {
            if self
                .resume_stopped_endpoints(old_endpoints.values())
                .await
                .is_err()
            {
                return Err(USBError::InterfaceBroken);
            }
            return Err(USBError::Other(anyhow!(
                "xHCI failed to disable endpoints before SET_CONFIGURATION"
            )));
        }

        if let Err(err) = self
            .control_endpoint_mut()
            .set_configuration(configuration_value)
            .await
        {
            self.prepare_configure_context(0, 0, &[], &old_descriptors, &old_endpoints);
            if self.configure_endpoint().await.is_err()
                || self
                    .resume_stopped_endpoints(old_endpoints.values())
                    .await
                    .is_err()
            {
                return Err(USBError::InterfaceBroken);
            }
            return Err(err.into());
        }

        self.publish_endpoint_routes(&old_endpoints, &BTreeMap::new())?;
        self.eps.clear();
        self.ep_interfaces.clear();
        self.interface_alternates.clear();

        self.ctx.perper_change();
        self.ctx.with_input(|input| {
            let c = input.control_mut();
            c.set_configuration_value(configuration_value);
        });
        if self.evaluate().await.is_err() {
            return Err(USBError::InterfaceBroken);
        }
        self.current_config_value = Some(configuration_value);
        debug!("Device configuration set to {configuration_value}");
        Ok(())
    }

    async fn _claim_interface(
        &mut self,
        interface: u8,
        alternate: u8,
    ) -> Result<BTreeMap<u8, EndpointHandle>> {
        let new_descriptors = self
            .find_interface_endpoints(interface, alternate)?
            .to_vec();
        self.validate_endpoint_addresses(interface, &new_descriptors)?;
        let pending_endpoints = self.prepare_endpoints(&new_descriptors)?;
        let old_alternate = self.interface_alternates.get(&interface).copied();
        let old_descriptors = old_alternate
            .map(|old| {
                self.find_interface_endpoints(interface, old)
                    .map(<[_]>::to_vec)
            })
            .transpose()?
            .unwrap_or_default();
        let stale_endpoints = self
            .ep_interfaces
            .iter()
            .filter_map(|(address, ep_interface)| (*ep_interface == interface).then_some(*address))
            .collect::<Vec<_>>();
        let mut old_endpoints = BTreeMap::new();
        for address in &stale_endpoints {
            if let Some(endpoint) = self.eps.get(address).cloned() {
                endpoint.revoke();
                old_endpoints.insert(*address, endpoint);
            }
        }

        if let Err(err) = self.stop_endpoints(old_endpoints.values()).await {
            for endpoint in old_endpoints.values() {
                endpoint.reactivate();
            }
            return Err(err);
        }

        self.prepare_configure_context(
            interface,
            alternate,
            &old_descriptors,
            &new_descriptors,
            &pending_endpoints,
        );

        if let Err(err) = self.configure_endpoint().await {
            self.resume_stopped_endpoints(old_endpoints.values())
                .await?;
            return Err(err);
        }

        let set_interface_result = self
            .control_endpoint_mut()
            .control_out(
                ControlSetup {
                    request_type: RequestType::Standard,
                    recipient: Recipient::Interface,
                    request: usb_if::transfer::Request::SetInterface,
                    value: alternate.into(),
                    index: interface.into(),
                },
                &[],
            )
            .await;
        if let Err(err) = set_interface_result {
            let rollback = self
                .rollback_interface_configuration(
                    interface,
                    old_alternate,
                    &new_descriptors,
                    &old_descriptors,
                    &old_endpoints,
                )
                .await;
            if let Err(rollback_err) = rollback {
                for endpoint in pending_endpoints.values() {
                    endpoint.revoke();
                    self.quarantined_eps.push(endpoint.clone());
                }
                warn!(
                    "SET_INTERFACE {interface}:{alternate} failed ({err}); xHCI rollback failed: \
                     {rollback_err}"
                );
                return Err(USBError::InterfaceBroken);
            }
            return Err(err.into());
        }

        if self
            .publish_endpoint_routes(&old_endpoints, &pending_endpoints)
            .is_err()
        {
            for endpoint in pending_endpoints.values() {
                endpoint.revoke();
                self.quarantined_eps.push(endpoint.clone());
            }
            return Err(USBError::InterfaceBroken);
        }
        for address in stale_endpoints {
            self.eps.remove(&address);
            self.ep_interfaces.remove(&address);
        }
        for (address, endpoint) in &pending_endpoints {
            self.eps.insert(*address, endpoint.clone());
            self.ep_interfaces.insert(*address, interface);
        }
        self.interface_alternates.insert(interface, alternate);
        debug!("Interface {interface} alternate {alternate} committed");
        Ok(pending_endpoints)
    }

    async fn _release_interface(&mut self, interface: u8) -> Result {
        let stale_addresses = self
            .ep_interfaces
            .iter()
            .filter_map(|(address, owner)| (*owner == interface).then_some(*address))
            .collect::<Vec<_>>();
        let old_endpoints = stale_addresses
            .iter()
            .filter_map(|address| self.eps.get(address).cloned().map(|ep| (*address, ep)))
            .collect::<BTreeMap<_, _>>();
        for endpoint in old_endpoints.values() {
            endpoint.revoke();
        }
        if let Err(err) = self.stop_endpoints(old_endpoints.values()).await {
            for endpoint in old_endpoints.values() {
                endpoint.reactivate();
            }
            return Err(err);
        }

        let old_alternate = self
            .interface_alternates
            .get(&interface)
            .copied()
            .unwrap_or(0);
        let old_descriptors = self
            .find_interface_endpoints(interface, old_alternate)?
            .to_vec();
        self.prepare_configure_context(
            interface,
            old_alternate,
            &old_descriptors,
            &[],
            &BTreeMap::new(),
        );
        if self.configure_endpoint().await.is_err() {
            if self
                .resume_stopped_endpoints(old_endpoints.values())
                .await
                .is_err()
            {
                return Err(USBError::InterfaceBroken);
            }
            return Err(USBError::Other(anyhow!(
                "xHCI failed to disable interface {interface}"
            )));
        }
        self.publish_endpoint_routes(&old_endpoints, &BTreeMap::new())?;
        for address in stale_addresses {
            self.eps.remove(&address);
            self.ep_interfaces.remove(&address);
        }
        self.interface_alternates.remove(&interface);
        Ok(())
    }

    async fn _disconnect(&mut self) -> Result {
        let mut old_endpoints = self.eps.clone();
        if let Some(control) = &self.ctrl_ep {
            old_endpoints.insert(0, control.clone());
        }
        for endpoint in old_endpoints.values() {
            endpoint.revoke();
        }

        // Flush every endpoint first, matching usb_hcd_flush_endpoint(). A
        // physical disconnect does not prevent the controller from executing
        // Stop Endpoint for the slot; if a stop command fails, Disable Slot is
        // still the final hardware-ownership boundary for all remaining rings.
        for endpoint in old_endpoints.values() {
            let future = endpoint.with_raw_mut::<XhciEndpoint, _>(|raw| raw.stop_future());
            if future.await.is_ok() {
                endpoint.with_raw_mut::<XhciEndpoint, _>(XhciEndpoint::retire_all_after_stop);
            }
        }

        let disable = self
            .cmd
            .cmd_request(command::Allowed::DisableSlot(
                *command::DisableSlot::default().set_slot_id(self.id.into()),
            ))
            .await;
        if let Err(err) = disable {
            for endpoint in old_endpoints.values() {
                self.quarantined_eps.push(endpoint.clone());
            }
            warn!("xHCI Disable Slot failed during disconnect: {err}");
            return Err(USBError::InterfaceBroken);
        }

        for endpoint in old_endpoints.values() {
            endpoint.with_raw_mut::<XhciEndpoint, _>(XhciEndpoint::retire_all_after_stop);
        }
        if self
            .publish_endpoint_routes(&old_endpoints, &BTreeMap::new())
            .is_err()
        {
            // Disable Slot has stopped hardware ownership, but a mismatched
            // software route means the endpoint registry is no longer a
            // trustworthy release boundary. Keep every ring quarantined and
            // fail closed instead of dropping DMA-backed queue ownership.
            self.quarantined_eps.extend(old_endpoints.values().cloned());
            return Err(USBError::InterfaceBroken);
        }
        self.eps.clear();
        self.ep_interfaces.clear();
        self.interface_alternates.clear();
        Ok(())
    }

    fn prepare_endpoints(
        &self,
        descriptors: &[EndpointDescriptor],
    ) -> Result<BTreeMap<u8, EndpointHandle>> {
        let mut endpoints = BTreeMap::new();
        for desc in descriptors {
            let dci = desc.dci();
            let mut ep_raw = self.create_ep(dci.into())?;
            let periodic_burst_size = match self.port_speed {
                Speed::High
                    if matches!(
                        desc.transfer_type,
                        EndpointType::Isochronous | EndpointType::Interrupt
                    ) =>
                {
                    desc.packets_per_microframe.saturating_sub(1)
                }
                _ => 0,
            };
            ep_raw.configure_periodic(
                desc.max_packet_size as usize,
                periodic_burst_size,
                desc.interval,
            );
            endpoints.insert(desc.address, EndpointHandle::new(desc.into(), ep_raw));
        }
        Ok(endpoints)
    }

    fn prepare_configure_context(
        &mut self,
        interface: u8,
        alternate: u8,
        drop_descriptors: &[EndpointDescriptor],
        add_descriptors: &[EndpointDescriptor],
        endpoints: &BTreeMap<u8, EndpointHandle>,
    ) {
        self.ctx.perper_change();
        let drop_dcis = drop_descriptors
            .iter()
            .map(EndpointDescriptor::dci)
            .collect::<Vec<_>>();
        self.ctx.with_input(|input| {
            let control = input.control_mut();
            control.set_interface_number(interface);
            control.set_alternate_setting(alternate);
            for dci in &drop_dcis {
                control.set_drop_context_flag((*dci).into());
            }
        });

        for descriptor in add_descriptors {
            let endpoint = endpoints
                .get(&descriptor.address)
                .expect("prepared endpoint must match its descriptor");
            let ring_addr = endpoint.with_raw_mut::<XhciEndpoint, _>(|raw| raw.bus_addr());
            let dci = descriptor.dci();
            let xhci_interval = self.calculate_xhci_interval(
                descriptor.interval,
                descriptor.transfer_type,
                descriptor.interval,
            );
            let periodic_burst_size = match self.port_speed {
                Speed::High
                    if matches!(
                        descriptor.transfer_type,
                        EndpointType::Isochronous | EndpointType::Interrupt
                    ) =>
                {
                    descriptor.packets_per_microframe.saturating_sub(1)
                }
                _ => 0,
            };
            self.ctx.with_input(|input| {
                input.control_mut().set_add_context_flag(dci.into());
                let endpoint_context = input.device_mut().endpoint_mut(dci.into());
                endpoint_context.set_interval(xhci_interval);
                endpoint_context.set_endpoint_type(descriptor.endpoint_type());
                endpoint_context.set_tr_dequeue_pointer(ring_addr.raw());
                endpoint_context.set_max_packet_size(descriptor.max_packet_size);
                endpoint_context.set_error_count(3);
                endpoint_context.set_dequeue_cycle_state();
                if matches!(
                    descriptor.transfer_type,
                    EndpointType::Isochronous | EndpointType::Interrupt
                ) {
                    endpoint_context
                        .set_max_burst_size(periodic_burst_size.min(u8::MAX as usize) as u8);
                    endpoint_context.set_mult(0);
                    let max_esit_payload =
                        descriptor.max_packet_size as usize * (periodic_burst_size + 1);
                    endpoint_context
                        .set_average_trb_length(max_esit_payload.min(u16::MAX as usize) as u16);
                    endpoint_context.set_max_endpoint_service_time_interval_payload_low(
                        max_esit_payload.min(u16::MAX as usize) as u16,
                    );
                }
                if matches!(descriptor.transfer_type, EndpointType::Isochronous) {
                    endpoint_context.set_error_count(0);
                }
            });
        }

        let max_dci = self
            .eps
            .keys()
            .map(|address| endpoint_address_dci(*address))
            .chain(add_descriptors.iter().map(EndpointDescriptor::dci))
            .max()
            .unwrap_or(1);
        self.ctx.with_input(|input| {
            input
                .device_mut()
                .slot_mut()
                .set_context_entries(max_dci + 1)
        });
        mb();
    }

    async fn configure_endpoint(&mut self) -> Result {
        self.cmd
            .cmd_request(command::Allowed::ConfigureEndpoint(
                *command::ConfigureEndpoint::default()
                    .set_slot_id(self.id.into())
                    .set_input_context_pointer(self.ctx.input_bus_addr()),
            ))
            .await?;
        Ok(())
    }

    async fn stop_endpoints<'a>(
        &self,
        endpoints: impl Iterator<Item = &'a EndpointHandle>,
    ) -> Result {
        let endpoints = endpoints.cloned().collect::<Vec<_>>();
        let mut stopped = Vec::new();
        for endpoint in endpoints {
            let future = endpoint.with_raw_mut::<XhciEndpoint, _>(|raw| raw.stop_future());
            if let Err(err) = future.await {
                self.resume_stopped_endpoints(stopped.iter()).await?;
                return Err(err.into());
            }
            endpoint.with_raw_mut::<XhciEndpoint, _>(XhciEndpoint::retire_all_after_stop);
            stopped.push(endpoint);
        }
        Ok(())
    }

    async fn resume_stopped_endpoints<'a>(
        &self,
        endpoints: impl Iterator<Item = &'a EndpointHandle>,
    ) -> Result {
        for endpoint in endpoints {
            let future = endpoint.with_raw_mut::<XhciEndpoint, _>(|raw| raw.resume_future());
            future.await?;
            endpoint.reactivate();
        }
        Ok(())
    }

    async fn rollback_interface_configuration(
        &mut self,
        interface: u8,
        old_alternate: Option<u8>,
        new_descriptors: &[EndpointDescriptor],
        old_descriptors: &[EndpointDescriptor],
        old_endpoints: &BTreeMap<u8, EndpointHandle>,
    ) -> Result {
        let rollback_alternate = old_alternate.unwrap_or(0);
        self.prepare_configure_context(
            interface,
            rollback_alternate,
            new_descriptors,
            old_descriptors,
            old_endpoints,
        );
        self.configure_endpoint().await?;
        if let Some(old_alternate) = old_alternate {
            self.control_endpoint_mut()
                .control_out(
                    ControlSetup {
                        request_type: RequestType::Standard,
                        recipient: Recipient::Interface,
                        request: usb_if::transfer::Request::SetInterface,
                        value: old_alternate.into(),
                        index: interface.into(),
                    },
                    &[],
                )
                .await?;
        }
        for endpoint in old_endpoints.values() {
            endpoint.reactivate();
        }
        Ok(())
    }

    fn publish_endpoint_routes(
        &self,
        old_endpoints: &BTreeMap<u8, EndpointHandle>,
        new_endpoints: &BTreeMap<u8, EndpointHandle>,
    ) -> Result {
        let old_routes = old_endpoints.values().map(|endpoint| {
            let dci = endpoint_address_dci(endpoint.info().address.raw());
            let queue =
                endpoint.with_raw_mut::<XhciEndpoint, _>(|raw| raw.ring().finished_handle());
            (dci, queue)
        });
        let new_routes = new_endpoints.values().map(|endpoint| {
            let dci = endpoint_address_dci(endpoint.info().address.raw());
            let queue =
                endpoint.with_raw_mut::<XhciEndpoint, _>(|raw| raw.ring().finished_handle());
            (dci, queue)
        });
        self.transfer_result_handler
            .replace_queues(self.id.as_u8(), old_routes, new_routes)?;
        Ok(())
    }

    fn validate_endpoint_addresses(
        &self,
        interface: u8,
        descriptors: &[EndpointDescriptor],
    ) -> Result {
        for descriptor in descriptors {
            if self
                .ep_interfaces
                .get(&descriptor.address)
                .is_some_and(|owner| *owner != interface)
            {
                return Err(USBError::InvalidParameter);
            }
        }

        Ok(())
    }

    fn find_interface_endpoints(
        &self,
        interface: u8,
        alternate: u8,
    ) -> Result<&[EndpointDescriptor]> {
        for config in &self.config_desc {
            for iface in &config.interfaces {
                if iface.interface_number == interface {
                    for alt in &iface.alt_settings {
                        if alt.alternate_setting == alternate {
                            return Ok(&alt.endpoints);
                        }
                    }
                }
            }
        }
        Err(USBError::NotFound)
    }

    /// 根据 XHCI 规范计算端点的 interval 值
    /// 参考 xHCI 规范第 6.2.3.6 节
    fn calculate_xhci_interval(
        &self,
        binterval: u8,
        transfer_type: EndpointType,
        default: u8,
    ) -> u8 {
        match transfer_type {
            EndpointType::Isochronous => {
                match self.port_speed {
                    Speed::High | Speed::SuperSpeed | Speed::SuperSpeedPlus => {
                        // HS/SS bInterval is one-based; xHCI Interval is zero-based.
                        let interval = binterval.clamp(1, 16) - 1;
                        debug!(
                            "ISO endpoint HS/SS: bInterval={} -> XHCI interval={}",
                            binterval, interval
                        );
                        interval
                    }
                    _ => {
                        let interval = binterval.max(1).ilog2() as u8 + 3;
                        debug!(
                            "ISO endpoint FS/LS: bInterval={} -> XHCI interval={}",
                            binterval, interval
                        );
                        interval
                    }
                }
            }
            EndpointType::Interrupt => match self.port_speed {
                Speed::High | Speed::SuperSpeed | Speed::SuperSpeedPlus => {
                    let interval = binterval.clamp(1, 16) - 1;
                    debug!(
                        "INT endpoint HS/SS: bInterval={} -> XHCI interval={}",
                        binterval, interval
                    );
                    interval
                }
                _ => {
                    let interval = binterval.max(1).ilog2() as u8 + 3;
                    debug!(
                        "INT endpoint FS/LS: bInterval={} -> XHCI interval={}",
                        binterval, interval
                    );
                    interval
                }
            },
            _ => {
                // 控制和批量端点不使用 interval
                default
            }
        }
    }

    async fn update_hub_inner(&mut self, params: HubParams) -> Result<()> {
        debug!(
            "Updating hub context for slot {}: ports={}, multi_tt={}, tt_time={}ns",
            self.id.as_u8(),
            params.num_ports,
            params.multi_tt,
            params.tt_think_time_ns,
        );

        self.ctx.perper_change();
        // 2. 设置 Slot Context Hub 参数
        self.ctx.with_input(|input| {
            let slot_ctx = input.device_mut().slot_mut();

            // 设置 Hub 标志
            slot_ctx.set_hub();

            // 设置 Multi-TT 标志（参考 U-Boot）
            // 如果 hub->tt.multi 为真，设置 MTT
            // 对于 Full Speed Hub，必须清除 MTT（xHCI 规范 6.2.2）
            if params.multi_tt {
                slot_ctx.set_multi_tt();
            } else if matches!(self.port_speed, Speed::Full) {
                slot_ctx.clear_multi_tt();
            }

            // 设置端口数量
            slot_ctx.set_number_of_ports(params.num_ports);

            // 设置 TT 思考时间（参考 U-Boot xhci_update_hub_device）
            // xHCI spec: TT_THINK_TIME (Bits[16:17] of DWORD 2)
            // 0 = 8 FS bit times, 1 = 16 FS bit times, 2 = 24 FS bit times, 3 = 32 FS bit times
            // 只对 High Speed Hub 设置 TT 思考时间
            if matches!(self.port_speed, Speed::High) {
                // params.tt_think_time_ns 已经是转换后的值 (0, 666, 1333, 1999)
                // 需要转换为 xHCI 寄存器值
                let think_time = if params.tt_think_time_ns > 0 {
                    ((params.tt_think_time_ns / 666) - 1) as u8
                } else {
                    0
                };
                slot_ctx.set_tt_think_time(think_time);
                debug!(
                    "Set TT think time: {} (tt_think_time_ns={}ns)",
                    think_time, params.tt_think_time_ns
                );
            }
        });

        self.evaluate().await?;
        Ok(())
    }
}

impl DeviceOp for Device {
    fn id(&self) -> usize {
        self.id.as_usize()
    }

    fn backend_name(&self) -> &str {
        "xhci"
    }

    fn descriptor(&self) -> &DeviceDescriptor {
        &self.desc
    }

    fn ctrl_ep_ref(&self) -> &EndpointHandle {
        self.control_endpoint()
    }

    fn ctrl_ep_mut(&mut self) -> &mut EndpointHandle {
        self.control_endpoint_mut()
    }

    fn claim_interface<'a>(
        &'a mut self,
        interface: u8,
        alternate: u8,
    ) -> BoxFuture<'a, Result<BTreeMap<u8, EndpointHandle>>> {
        self._claim_interface(interface, alternate).boxed()
    }

    fn release_interface<'a>(&'a mut self, interface: u8) -> BoxFuture<'a, Result<()>> {
        self._release_interface(interface).boxed()
    }

    fn set_configuration<'a>(&'a mut self, configuration_value: u8) -> BoxFuture<'a, Result<()>> {
        self._set_configuration(configuration_value).boxed()
    }

    fn disconnect(&mut self) -> BoxFuture<'_, Result<()>> {
        self._disconnect().boxed()
    }

    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor] {
        &self.config_desc
    }

    fn update_hub(&mut self, params: HubParams) -> BoxFuture<'_, Result<()>> {
        self.update_hub_inner(params).boxed()
    }
}
