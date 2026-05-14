use alloc::collections::BTreeMap;

use alloc::{sync::Arc, vec::Vec};

use futures::{FutureExt, future::BoxFuture};
use mbarrier::mb;
use spin::Mutex;
use usb_if::descriptor::DeviceDescriptorBase;
use usb_if::endpoint::EndpointInfo;
use usb_if::err::USBError;
use usb_if::{
    descriptor::{
        ConfigurationDescriptor, DescriptorType, DeviceDescriptor, EndpointDescriptor, EndpointType,
    },
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
use crate::DeviceAddressInfo;
use crate::backend::ty::HubParams;

use crate::osal::Kernel;
use crate::{
    backend::{
        Dci,
        ty::{DeviceOp, ep::Endpoint},
    },
    err::Result,
};

pub struct Device {
    id: SlotId,
    ctx: ContextData,
    desc: DeviceDescriptor,
    ctrl_ep: Option<Endpoint>,
    transfer_result_handler: TransferResultHandler,
    bell: Arc<Mutex<SlotBell>>,
    kernel: Kernel,
    current_config_value: Option<u8>,
    config_desc: Vec<ConfigurationDescriptor>,
    port_speed: Speed,
    eps: BTreeMap<u8, Endpoint>,
    ep_interfaces: BTreeMap<u8, u8>,
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
            cmd: host.cmd.clone(),
        })
    }

    fn new_ep(&mut self, dci: Dci) -> Result<XhciEndpoint> {
        let ep = XhciEndpoint::new(dci, &self.kernel, self.bell.clone())?;
        self.transfer_result_handler
            .register_queue(self.id.as_u8(), dci.as_u8(), ep.ring());

        Ok(ep)
    }

    fn control_endpoint(&self) -> &Endpoint {
        self.ctrl_ep.as_ref().unwrap()
    }

    fn control_endpoint_mut(&mut self) -> &mut Endpoint {
        self.ctrl_ep.as_mut().unwrap()
    }

    pub(crate) async fn init(&mut self, host: &mut Xhci, info: &DeviceAddressInfo) -> Result {
        // Keep the raw PORTSC.PortSpeed encoding for interval calculations
        self.port_speed = info.port_speed;
        // let speed = info.port_speed.to_xhci_portsc_value();

        let ep = self.new_ep(Dci::CTRL)?;
        self.ctrl_ep = Some(Endpoint::new(EndpointInfo::control(), ep));
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
            let _ = input.control_mut().add_context_flag(1); // Endpoint 0 Context

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
                    tt_port = parent_hub.port_id;
                    if parent_hub.hub_depth == -1 {
                        break;
                    }
                    if matches!(parent_hub.speed, Speed::High) {
                        hs_parent = Some(p);
                        break;
                    }
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
            // • Max Packet Size = The default maximum packet size for the Default Control Endpoint,
            //   as function of the PORTSC Port Speed field.
            endpoint_0.set_max_packet_size(max_packet_size);
            // • Max Burst Size = 0.
            endpoint_0.set_max_burst_size(0);
            // • TR Dequeue Pointer = Start address of first segment of the Default Control
            //   Endpoint Transfer Ring.
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
        self.control_endpoint_mut()
            .get_descriptor(DescriptorType::DEVICE, 0, 0, data.as_mut_slice())
            .await?;

        let desc = unsafe { *(data.as_mut_slice().as_ptr() as *const DeviceDescriptorBase) };

        Ok(desc)
    }

    async fn get_configuration(&mut self) -> Result<u8> {
        let val = self.control_endpoint_mut().get_configuration().await?;
        self.current_config_value = Some(val);
        Ok(val)
    }

    async fn _set_configuration(&mut self, configuration_value: u8) -> Result {
        self.ctx.perper_change();
        self.control_endpoint_mut()
            .set_configuration(configuration_value)
            .await?;

        self.current_config_value = Some(configuration_value);
        self.eps.clear();
        self.ep_interfaces.clear();

        self.ctx.with_input(|input| {
            let c = input.control_mut();
            c.set_configuration_value(configuration_value);
        });
        self.evaluate().await?;
        debug!("Device configuration set to {configuration_value}");
        Ok(())
    }

    async fn _claim_interface(&mut self, interface: u8, alternate: u8) -> Result {
        self.ctx.perper_change();
        self.ctx.with_input(|input| {
            let c = input.control_mut();
            c.set_interface_number(interface);
            c.set_alternate_setting(alternate);
        });

        self.control_endpoint_mut()
            .control_out(
                ControlSetup {
                    request_type: RequestType::Standard,
                    recipient: Recipient::Interface,
                    request: usb_if::transfer::Request::SetInterface,
                    value: alternate as _, // alternate setting goes in value
                    index: interface as _, // interface number goes in index
                },
                &[],
            )
            .await?;
        self.setup_interface_endpoints(interface, alternate).await?;
        debug!("Interface {interface} set successfully");
        Ok(())
    }

    async fn setup_interface_endpoints(&mut self, interface: u8, alternate: u8) -> Result {
        self.ctx.perper_change();
        let stale_endpoints = self
            .ep_interfaces
            .iter()
            .filter_map(|(address, ep_interface)| (*ep_interface == interface).then_some(*address))
            .collect::<Vec<_>>();
        let drop_dcis = stale_endpoints
            .iter()
            .filter_map(|address| {
                self.find_ep_desc_in_current_config(*address)
                    .ok()
                    .map(|desc| desc.dci())
            })
            .collect::<Vec<_>>();
        let mut old_endpoints = Vec::with_capacity(stale_endpoints.len());
        for address in &stale_endpoints {
            if let Some(endpoint) = self.eps.remove(address) {
                old_endpoints.push(endpoint);
            }
            self.ep_interfaces.remove(address);
        }
        self.ctx.with_input(|input| {
            let control_context = input.control_mut();
            for dci in drop_dcis {
                control_context.set_drop_context_flag(dci as _);
            }
        });

        for desc in self
            .find_interface_endpoints(interface, alternate)?
            .to_vec()
        {
            let dci = desc.dci();
            let mut ep_raw = self.new_ep(dci.into())?;
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
            let ring_addr = ep_raw.bus_addr();
            self.eps
                .insert(desc.address, Endpoint::new((&desc).into(), ep_raw));
            self.ep_interfaces.insert(desc.address, interface);

            let xhci_interval =
                self.calculate_xhci_interval(desc.interval, desc.transfer_type, desc.interval);

            self.ctx.with_input(|input| {
                let control_context = input.control_mut();

                control_context.set_add_context_flag(dci as _);
                control_context.clear_drop_context_flag(dci as _);

                debug!(
                    "init ep addr {:#x}  dci {dci} {:?}",
                    desc.address, desc.transfer_type
                );

                let ep_mut = input.device_mut().endpoint_mut(dci as _);

                debug!(
                    "Set XHCI interval: {} (original bInterval: {})",
                    xhci_interval, desc.interval
                );
                ep_mut.set_interval(xhci_interval);
                ep_mut.set_endpoint_type(desc.endpoint_type());
                ep_mut.set_tr_dequeue_pointer(ring_addr.raw());
                ep_mut.set_max_packet_size(desc.max_packet_size);
                ep_mut.set_error_count(3);
                ep_mut.set_dequeue_cycle_state();

                match desc.transfer_type {
                    EndpointType::Isochronous | EndpointType::Interrupt => {
                        //init for isoch/interrupt
                        ep_mut.set_max_packet_size(desc.max_packet_size);
                        ep_mut.set_max_burst_size(periodic_burst_size.try_into().unwrap());
                        ep_mut.set_mult(0); //always 0 for interrupt
                        let max_esit_payload =
                            desc.max_packet_size as usize * (periodic_burst_size + 1);
                        ep_mut
                            .set_average_trb_length(max_esit_payload.min(u16::MAX as usize) as u16);
                        ep_mut.set_max_endpoint_service_time_interval_payload_low(
                            max_esit_payload.min(u16::MAX as usize) as u16,
                        );
                    }
                    _ => {}
                }

                if let EndpointType::Isochronous = desc.transfer_type {
                    ep_mut.set_error_count(0);
                }
            });
        }

        let max_dci = self
            .eps
            .keys()
            .filter_map(|address| {
                self.find_ep_desc_in_current_config(*address)
                    .ok()
                    .map(|desc| desc.dci())
            })
            .max()
            .unwrap_or(1);
        self.ctx.with_input(|input| {
            input
                .device_mut()
                .slot_mut()
                .set_context_entries(max_dci + 1)
        });
        mb();

        let _result = self
            .cmd
            .cmd_request(command::Allowed::ConfigureEndpoint(
                *command::ConfigureEndpoint::default()
                    .set_slot_id(self.id.into())
                    .set_input_context_pointer(self.ctx.input_bus_addr()),
            ))
            .await?;
        // Keep old endpoint rings alive until hardware accepts the new input context.
        drop(old_endpoints);

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

    fn current_config_desc(&self) -> Result<&ConfigurationDescriptor> {
        let Some(current_config_value) = self.current_config_value else {
            return Err(USBError::ConfigurationNotSet);
        };
        self.config_desc
            .iter()
            .find(|config| config.configuration_value == current_config_value)
            .ok_or(USBError::NotFound)
    }

    fn find_ep_desc_in_current_config(&self, address: u8) -> Result<&EndpointDescriptor> {
        let config = self.current_config_desc()?;
        for iface in &config.interfaces {
            for alt in &iface.alt_settings {
                for desc in &alt.endpoints {
                    if desc.address == address {
                        return Ok(desc);
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
                        // HighSpeed, SuperSpeed, SuperSpeedPlus ISO 端点
                        // Interval = max(1, min(16, bInterval))
                        let interval = binterval.clamp(1, 16);
                        debug!(
                            "ISO endpoint HS/SS: bInterval={} -> XHCI interval={}",
                            binterval, interval
                        );
                        interval
                    }
                    _ => {
                        // FullSpeed/LowSpeed ISO 端点
                        // Interval = max(1, min(16, floor(log2(bInterval)) + 3))
                        if binterval == 0 {
                            1
                        } else {
                            // 计算 floor(log2(bInterval))
                            let log2_binterval = 31 - (binterval as u32).leading_zeros() as u8 - 1;
                            let interval = (log2_binterval + 3).clamp(1, 16);
                            debug!(
                                "ISO endpoint FS/LS: bInterval={} -> log2={} -> XHCI interval={}",
                                binterval, log2_binterval, interval
                            );
                            interval
                        }
                    }
                }
            }
            EndpointType::Interrupt => {
                match self.port_speed {
                    Speed::High | Speed::SuperSpeed | Speed::SuperSpeedPlus => {
                        // HighSpeed, SuperSpeed, SuperSpeedPlus 中断端点
                        // Interval = max(1, min(16, bInterval))
                        let interval = binterval.clamp(1, 16);
                        debug!(
                            "INT endpoint HS/SS: bInterval={} -> XHCI interval={}",
                            binterval, interval
                        );
                        interval
                    }
                    _ => {
                        // FullSpeed/LowSpeed 中断端点
                        // Interval = max(1, min(16, floor(log2(bInterval)) + 3))
                        if binterval == 0 {
                            1
                        } else {
                            // 计算 floor(log2(bInterval))
                            let log2_binterval = 31 - (binterval as u32).leading_zeros() as u8 - 1;
                            let interval = (log2_binterval + 3).clamp(1, 16);
                            debug!(
                                "INT endpoint FS/LS: bInterval={} -> log2={} -> XHCI interval={}",
                                binterval, log2_binterval, interval
                            );
                            interval
                        }
                    }
                }
            }
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

    fn ctrl_ep_ref(&self) -> &Endpoint {
        self.control_endpoint()
    }

    fn ctrl_ep_mut(&mut self) -> &mut Endpoint {
        self.control_endpoint_mut()
    }

    fn claim_interface<'a>(
        &'a mut self,
        interface: u8,
        alternate: u8,
    ) -> BoxFuture<'a, Result<()>> {
        self._claim_interface(interface, alternate).boxed()
    }

    fn set_configuration<'a>(&'a mut self, configuration_value: u8) -> BoxFuture<'a, Result<()>> {
        self._set_configuration(configuration_value).boxed()
    }

    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor] {
        &self.config_desc
    }

    fn endpoint(&mut self, desc: &usb_if::descriptor::EndpointDescriptor) -> Result<Endpoint> {
        let ep = self.eps.remove(&desc.address);
        ep.ok_or(USBError::NotFound)
    }

    fn update_hub(&mut self, params: HubParams) -> BoxFuture<'_, Result<()>> {
        self.update_hub_inner(params).boxed()
    }
}
