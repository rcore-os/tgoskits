use alloc::boxed::Box;
use core::any::Any;
use core::fmt::Debug;

use futures::future::BoxFuture;
use usb_if::descriptor::{ConfigurationDescriptor, DeviceDescriptor, EndpointDescriptor};

use crate::{backend::ty::ep::Endpoint, err::USBError};

pub mod ep;
pub mod transfer;

#[derive(Debug, Clone)]
pub enum Event {
    Nothing,
    PortChange { port: u8 },
    TransferActivity { count: usize },
    Stopped,
}

pub(crate) trait EventHandlerOp: Send + Any + Sync + 'static {
    fn handle_event(&self) -> Event;
}

#[allow(dead_code)]
pub(crate) trait DeviceInfoOp: Send + Sync + Any + Debug + 'static {
    fn id(&self) -> usize;
    fn backend_name(&self) -> &str;
    fn descriptor(&self) -> &DeviceDescriptor;
    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor];
}

pub(crate) enum ProbedDeviceInfoOp {
    Device(Box<dyn DeviceInfoOp>),
    Hub(Box<dyn DeviceInfoOp>),
}

/// USB 设备特征（高层抽象）
pub(crate) trait DeviceOp: Send + Any + 'static {
    fn id(&self) -> usize;
    fn backend_name(&self) -> &str;
    fn descriptor(&self) -> &DeviceDescriptor;
    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor];

    fn ctrl_ep_ref(&self) -> &Endpoint;

    fn ctrl_ep_mut(&mut self) -> &mut Endpoint;

    fn claim_interface<'a>(
        &'a mut self,
        interface: u8,
        alternate: u8,
    ) -> BoxFuture<'a, Result<(), USBError>>;

    fn set_configuration<'a>(
        &'a mut self,
        configuration_value: u8,
    ) -> BoxFuture<'a, Result<(), USBError>>;

    fn endpoint(&mut self, desc: &EndpointDescriptor) -> Result<ep::Endpoint, USBError>;

    fn update_hub(&mut self, params: HubParams) -> BoxFuture<'_, Result<(), USBError>>;
}

#[derive(Debug, Clone)]
pub struct HubParams {
    /// Hub 端口数量
    pub num_ports: u8,

    /// 是否为 Multi-TT Hub
    pub multi_tt: bool,

    /// TT 思考时间（单位：纳秒）
    /// 8 FS bit times = 666ns
    pub tt_think_time_ns: u16,

    /// 父 Hub Slot ID（0 表示 Root Hub）
    pub parent_hub_slot_id: u8,

    /// Root Hub 端口号
    pub root_hub_port_number: u8,
}
