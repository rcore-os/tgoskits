#[cfg(any(kmod, umod))]
use alloc::boxed::Box;
#[cfg(kmod)]
use alloc::sync::Arc;
#[cfg(kmod)]
use core::sync::atomic::{AtomicBool, Ordering};
use core::{any::Any, fmt::Debug};

#[cfg(kmod)]
use ax_kspin::SpinRaw;
use futures::future::BoxFuture;
use usb_if::descriptor::{ConfigurationDescriptor, DeviceDescriptor, EndpointDescriptor};

use crate::{backend::ty::ep::Endpoint, err::USBError};

pub mod ep;
#[cfg(any(kmod, umod))]
pub mod transfer;

#[derive(Debug, Clone)]
pub enum Event {
    Nothing,
    PortChange { port: u8 },
    TransferActivity { count: usize },
    Stopped,
}

/// Serializes task-context controller IRQ control with deferred rearming.
///
/// Hard-IRQ acknowledgement deliberately does not acquire this gate. It may
/// mask and publish a pending source concurrently, while controller lifecycle
/// changes and the task-context rearm remain ordered through this state.
#[cfg(kmod)]
#[derive(Clone)]
pub(crate) struct ControllerIrqState {
    inner: Arc<ControllerIrqStateInner>,
}

#[cfg(kmod)]
struct ControllerIrqStateInner {
    enabled: AtomicBool,
    control: SpinRaw<()>,
}

#[cfg(kmod)]
impl ControllerIrqState {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            inner: Arc::new(ControllerIrqStateInner {
                enabled: AtomicBool::new(enabled),
                control: SpinRaw::new(()),
            }),
        }
    }

    pub(crate) fn set_enabled(&self, enabled: bool, apply: impl FnOnce()) {
        let _guard = self.inner.control.lock();
        self.inner.enabled.store(enabled, Ordering::Release);
        apply();
    }

    pub(crate) fn apply_enabled(&self, apply: impl FnOnce(bool)) {
        let _guard = self.inner.control.lock();
        apply(self.is_enabled());
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::Acquire)
    }
}

pub(crate) trait EventHandlerOp: Send + Any + Sync + 'static {
    /// Acknowledges and, when required, masks one device IRQ.
    ///
    /// This method is the hard-IRQ capability boundary. Implementations must
    /// perform bounded register work only and must not wake task-owned
    /// completions or drain an unbounded hardware queue.
    fn acknowledge_irq(&self) -> bool;

    /// Drains one task-context batch after an IRQ acknowledgement.
    fn drain_event(&self) -> Event;

    /// Rearms device interrupts after task-context draining completes.
    fn rearm_irq(&self);

    fn handle_event(&self) -> Event {
        self.acknowledge_irq();
        let event = self.drain_event();
        self.rearm_irq();
        event
    }
}

#[allow(dead_code)]
pub(crate) trait DeviceInfoOp: Send + Sync + Any + Debug + 'static {
    fn id(&self) -> usize;
    fn backend_name(&self) -> &str;
    fn descriptor(&self) -> &DeviceDescriptor;
    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor];
}

#[cfg(any(kmod, umod))]
pub enum ProbedDeviceInfoOp {
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
