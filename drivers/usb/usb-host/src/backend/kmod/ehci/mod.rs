use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec, vec::Vec};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    task::Context,
    time::Duration,
};

use ax_kspin::SpinRaw as Mutex;
use dma_api::CoherentBox;
use futures::{FutureExt, future::BoxFuture, task::AtomicWaker};
use mbarrier::{mb, wmb};
use usb_if::{
    descriptor::{
        ConfigurationDescriptor, DescriptorType, DeviceDescriptor, DeviceDescriptorBase,
        EndpointDescriptor, EndpointType,
    },
    endpoint::{EndpointInfo, RequestId, TransferCompletion, TransferRequest, TransferStatus},
    err::{TransferError, USBError},
    host::{ControlSetup, hub::Speed},
    transfer::{BmRequestType, Direction, Recipient, Request, RequestType},
};

use super::{
    hub::{HubInfo, HubOp, PortChangeInfo, PortState},
    kcore::CoreOp,
    osal::{Kernel, KernelOp},
};
use crate::{
    DeviceAddressInfo, Mmio,
    backend::ty::{DeviceOp, Event, EventHandlerOp, HubParams, ep::Endpoint, transfer::Transfer},
    err::{HostError, Result},
};

const QTD_MAX_TOTAL_BYTES: usize = 20 * 1024;
const EHCI_DMA_MASK: u64 = u32::MAX as u64;
const EHCI_QH_LINK_TYPE_QH: u32 = 0b01 << 1;
const EHCI_LINK_TERMINATE: u32 = 1;

const USBCMD: usize = 0x00;
const USBSTS: usize = 0x04;
const USBINTR: usize = 0x08;
const CTRLDSSEGMENT: usize = 0x10;
const PERIODICLISTBASE: usize = 0x14;
const ASYNCLISTADDR: usize = 0x18;
const CONFIGFLAG: usize = 0x40;
const PORTSC_BASE: usize = 0x44;

const USBCMD_RUN_STOP: u32 = 1 << 0;
const USBCMD_HCRESET: u32 = 1 << 1;
const USBCMD_PERIODIC_ENABLE: u32 = 1 << 4;
const USBCMD_ASYNC_ENABLE: u32 = 1 << 5;
const USBCMD_INT_ASYNC_ADVANCE_DOORBELL: u32 = 1 << 6;

const USBSTS_USBINT: u32 = 1 << 0;
const USBSTS_USBERRINT: u32 = 1 << 1;
const USBSTS_PORT_CHANGE: u32 = 1 << 2;
const USBSTS_FRAME_LIST_ROLLOVER: u32 = 1 << 3;
const USBSTS_HOST_SYSTEM_ERROR: u32 = 1 << 4;
const USBSTS_INTERRUPT_ASYNC_ADVANCE: u32 = 1 << 5;
const USBSTS_HALTED: u32 = 1 << 12;

const USBINTR_USBINT: u32 = 1 << 0;
const USBINTR_USBERRINT: u32 = 1 << 1;
const USBINTR_PORT_CHANGE: u32 = 1 << 2;
const USBINTR_ASYNC_ADVANCE: u32 = 1 << 5;

const PORT_CONNECT_CHANGE: u32 = 1 << 1;
const PORT_ENABLE_CHANGE: u32 = 1 << 3;
const PORT_OVER_CURRENT_CHANGE: u32 = 1 << 5;
const PORT_RESET: u32 = 1 << 8;
const PORT_POWER: u32 = 1 << 12;
const PORT_OWNER: u32 = 1 << 13;

const EHCI_WAIT_ITERS: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QtdPid {
    Out,
    In,
    Setup,
}

impl QtdPid {
    const fn token_bits(self) -> u32 {
        match self {
            Self::Out => 0,
            Self::In => 1,
            Self::Setup => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QtdToken(u32);

impl QtdToken {
    #[cfg(test)]
    pub(crate) const PID_MASK: u32 = 0b11 << 8;
    #[cfg(test)]
    pub(crate) const PID_IN: u32 = 1 << 8;

    const ACTIVE: u32 = 1 << 7;
    const ERROR_COUNTER_SHIFT: u32 = 10;
    const ERROR_COUNTER_MASK: u32 = 0b11 << Self::ERROR_COUNTER_SHIFT;
    const INTERRUPT_ON_COMPLETE: u32 = 1 << 15;
    const TOTAL_BYTES_SHIFT: u32 = 16;
    const TOTAL_BYTES_MASK: u32 = 0x7fff << Self::TOTAL_BYTES_SHIFT;
    const DATA_TOGGLE: u32 = 1 << 31;
    const ERROR_MASK: u32 = (1 << 6) | (1 << 5) | (1 << 4) | (1 << 3);

    pub(crate) fn new(pid: QtdPid, total_bytes: usize) -> Self {
        let total_bytes = total_bytes.min(0x7fff) as u32;
        Self((pid.token_bits() << 8) | (total_bytes << Self::TOTAL_BYTES_SHIFT))
    }

    pub(crate) const fn raw(self) -> u32 {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn pid(self) -> QtdPid {
        match (self.0 & Self::PID_MASK) >> 8 {
            0 => QtdPid::Out,
            1 => QtdPid::In,
            2 => QtdPid::Setup,
            _ => QtdPid::Out,
        }
    }

    pub(crate) fn total_bytes(self) -> usize {
        ((self.0 & Self::TOTAL_BYTES_MASK) >> Self::TOTAL_BYTES_SHIFT) as usize
    }

    pub(crate) fn with_interrupt_on_complete(mut self, enabled: bool) -> Self {
        if enabled {
            self.0 |= Self::INTERRUPT_ON_COMPLETE;
        } else {
            self.0 &= !Self::INTERRUPT_ON_COMPLETE;
        }
        self
    }

    #[cfg(test)]
    pub(crate) const fn interrupt_on_complete(self) -> bool {
        (self.0 & Self::INTERRUPT_ON_COMPLETE) != 0
    }

    pub(crate) fn with_error_counter(mut self, count: u8) -> Self {
        self.0 &= !Self::ERROR_COUNTER_MASK;
        self.0 |= ((count.min(3) as u32) << Self::ERROR_COUNTER_SHIFT) & Self::ERROR_COUNTER_MASK;
        self
    }

    #[cfg(test)]
    pub(crate) fn error_counter(self) -> u8 {
        ((self.0 & Self::ERROR_COUNTER_MASK) >> Self::ERROR_COUNTER_SHIFT) as u8
    }

    pub(crate) fn with_active(mut self, active: bool) -> Self {
        if active {
            self.0 |= Self::ACTIVE;
        } else {
            self.0 &= !Self::ACTIVE;
        }
        self
    }

    pub(crate) const fn active(self) -> bool {
        (self.0 & Self::ACTIVE) != 0
    }

    fn with_data_toggle(mut self, toggle: bool) -> Self {
        if toggle {
            self.0 |= Self::DATA_TOGGLE;
        } else {
            self.0 &= !Self::DATA_TOGGLE;
        }
        self
    }

    fn has_error(self) -> bool {
        (self.0 & Self::ERROR_MASK) != 0
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EhciPortStatus(u32);

impl EhciPortStatus {
    pub(crate) const CURRENT_CONNECT: u32 = 1 << 0;
    pub(crate) const PORT_ENABLED: u32 = 1 << 2;
    pub(crate) const LINE_STATUS_K_STATE: u32 = 1 << 10;
    const LINE_STATUS_MASK: u32 = 0b11 << 10;

    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub(crate) const fn connected(self) -> bool {
        (self.0 & Self::CURRENT_CONNECT) != 0
    }

    pub(crate) const fn enabled(self) -> bool {
        (self.0 & Self::PORT_ENABLED) != 0
    }

    pub(crate) fn speed(self) -> Speed {
        if self.connected() && self.enabled() {
            return Speed::High;
        }

        match self.0 & Self::LINE_STATUS_MASK {
            Self::LINE_STATUS_K_STATE => Speed::Low,
            _ => Speed::Full,
        }
    }

    pub(crate) fn is_high_speed_device_ready(self) -> bool {
        self.connected() && self.enabled() && matches!(self.speed(), Speed::High)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ControlTdPlan {
    pub(crate) setup: QtdToken,
    pub(crate) data: Option<QtdToken>,
    pub(crate) status: QtdToken,
}

pub(crate) fn build_control_td_plan(
    request: &TransferRequest,
) -> core::result::Result<ControlTdPlan, TransferError> {
    let TransferRequest::Control {
        setup: _,
        direction,
        buffer,
    } = request
    else {
        return Err(TransferError::InvalidEndpoint);
    };

    let data_len = buffer.map(|buffer| buffer.len).unwrap_or(0);
    let setup = QtdToken::new(QtdPid::Setup, 8)
        .with_error_counter(3)
        .with_active(true)
        .with_data_toggle(false);
    let data = (data_len > 0).then(|| {
        let pid = match direction {
            Direction::In => QtdPid::In,
            Direction::Out => QtdPid::Out,
        };
        QtdToken::new(pid, data_len)
            .with_error_counter(3)
            .with_active(true)
            .with_data_toggle(true)
    });
    let status_pid = match (direction, data_len > 0) {
        (Direction::In, true) => QtdPid::Out,
        (Direction::Out, true) => QtdPid::In,
        (_, false) => QtdPid::In,
    };
    let status = QtdToken::new(status_pid, 0)
        .with_error_counter(3)
        .with_interrupt_on_complete(true)
        .with_active(true)
        .with_data_toggle(true);

    Ok(ControlTdPlan {
        setup,
        data,
        status,
    })
}

pub(crate) fn split_bulk_lengths(len: usize, _direction: Direction) -> Vec<usize> {
    let mut left = len;
    let mut out = Vec::new();

    while left > 0 {
        let chunk = left.min(QTD_MAX_TOTAL_BYTES);
        out.push(chunk);
        left -= chunk;
    }

    if out.is_empty() {
        out.push(0);
    }

    out
}

#[derive(Clone, Copy)]
struct EhciRegisters {
    op: NonNull<u8>,
    ports: u8,
}

unsafe impl Send for EhciRegisters {}
unsafe impl Sync for EhciRegisters {}

impl EhciRegisters {
    fn new(base: Mmio) -> Self {
        let cap_len = unsafe { base.as_ptr().read_volatile() as usize };
        let hcsparams = unsafe { (base.as_ptr().add(0x04) as *const u32).read_volatile() };
        let ports = (hcsparams & 0x0f).max(1) as u8;
        let op = unsafe { NonNull::new_unchecked(base.as_ptr().add(cap_len)) };

        Self { op, ports }
    }

    fn ports(self) -> u8 {
        self.ports
    }

    fn op_read32(self, offset: usize) -> u32 {
        unsafe { (self.op.as_ptr().add(offset) as *const u32).read_volatile() }
    }

    fn op_write32(self, offset: usize, value: u32) {
        unsafe { (self.op.as_ptr().add(offset) as *mut u32).write_volatile(value) }
    }

    fn op_update32(self, offset: usize, f: impl FnOnce(u32) -> u32) {
        let value = self.op_read32(offset);
        self.op_write32(offset, f(value));
    }

    fn port_offset(port_id: u8) -> usize {
        PORTSC_BASE + ((port_id as usize - 1) * 4)
    }

    fn port_read(self, port_id: u8) -> EhciPortStatus {
        EhciPortStatus::from_raw(self.op_read32(Self::port_offset(port_id)))
    }

    fn port_update(self, port_id: u8, f: impl FnOnce(u32) -> u32) {
        self.op_update32(Self::port_offset(port_id), f);
    }
}

#[derive(Clone, Copy)]
pub struct EhciNewParams {
    pub mmio: Mmio,
    pub kernel: &'static dyn KernelOp,
}

#[derive(Clone)]
struct AsyncSchedule(Arc<Mutex<AsyncScheduleInner>>);

struct AsyncScheduleInner {
    head: CoherentBox<QueueHead>,
    active_qh: Option<u32>,
}

impl AsyncSchedule {
    fn new(kernel: &Kernel) -> Result<Self> {
        let mut head = kernel
            .coherent_box_zero_with_align::<QueueHead>(32)
            .map_err(HostError::from)?;
        let addr = head.dma_addr().as_u64() as u32;
        head.write_cpu(QueueHead::async_head(addr));
        Ok(Self(Arc::new(Mutex::new(AsyncScheduleInner {
            head,
            active_qh: None,
        }))))
    }

    fn head_addr(&self) -> u32 {
        self.0.lock().head.dma_addr().as_u64() as u32
    }

    fn attach(&self, qh: &mut CoherentBox<QueueHead>) -> Result<()> {
        let mut inner = self.0.lock();
        let qh_addr = qh.dma_addr().as_u64() as u32;
        if inner.active_qh.is_some_and(|active| active != qh_addr) {
            return Err(USBError::TransferError(TransferError::QueueFull));
        }

        let head_addr = inner.head.dma_addr().as_u64() as u32;
        qh.modify_cpu(|qh| {
            qh.horizontal_link = ehci_qh_link(head_addr);
        });
        inner.head.modify_cpu(|head| {
            head.horizontal_link = ehci_qh_link(qh_addr);
        });
        inner.active_qh = Some(qh_addr);
        wmb();
        Ok(())
    }

    fn detach(&self, qh_addr: u32) {
        let mut inner = self.0.lock();
        if inner.active_qh != Some(qh_addr) {
            return;
        }
        let head_addr = inner.head.dma_addr().as_u64() as u32;
        inner.head.modify_cpu(|head| {
            head.horizontal_link = ehci_qh_link(head_addr);
        });
        inner.active_qh = None;
        wmb();
    }
}

fn ehci_qh_link(addr: u32) -> u32 {
    (addr & !0x1f) | EHCI_QH_LINK_TYPE_QH
}

#[derive(Clone, Copy)]
#[repr(C, align(32))]
struct QueueHead {
    horizontal_link: u32,
    endpoint_chars: u32,
    endpoint_caps: u32,
    current_qtd: u32,
    overlay: QueueTransferDescriptor,
}

impl QueueHead {
    fn async_head(addr: u32) -> Self {
        Self {
            horizontal_link: ehci_qh_link(addr),
            endpoint_chars: (1 << 15) | (64 << 16),
            endpoint_caps: 1 << 30,
            current_qtd: 0,
            overlay: QueueTransferDescriptor::terminated(),
        }
    }

    fn endpoint(addr: u8, ep_num: u8, ep_type: EndpointType, max_packet_size: u16) -> Self {
        let mut endpoint_chars = (addr as u32 & 0x7f)
            | ((ep_num as u32 & 0x0f) << 8)
            | (0b10 << 12)
            | ((max_packet_size as u32) << 16)
            | (0x0f << 28);

        if matches!(ep_type, EndpointType::Control) {
            endpoint_chars |= 1 << 14;
        }

        Self {
            horizontal_link: EHCI_LINK_TERMINATE,
            endpoint_chars,
            endpoint_caps: 1 << 30,
            current_qtd: 0,
            overlay: QueueTransferDescriptor::terminated(),
        }
    }

    fn set_next_qtd(&mut self, qtd_addr: u32) {
        self.overlay.next_qtd = qtd_addr & !0x1f;
        self.overlay.alt_next_qtd = EHCI_LINK_TERMINATE;
        self.overlay.token = 0;
    }
}

#[derive(Clone, Copy)]
#[repr(C, align(32))]
struct QueueTransferDescriptor {
    next_qtd: u32,
    alt_next_qtd: u32,
    token: u32,
    buffer: [u32; 5],
    ext_buffer: [u32; 5],
}

impl QueueTransferDescriptor {
    const fn terminated() -> Self {
        Self {
            next_qtd: EHCI_LINK_TERMINATE,
            alt_next_qtd: EHCI_LINK_TERMINATE,
            token: 0,
            buffer: [0; 5],
            ext_buffer: [0; 5],
        }
    }

    fn new(token: QtdToken, dma_addr: u64, len: usize) -> Self {
        let mut qtd = Self::terminated();
        qtd.token = token.raw();
        qtd.set_buffer(dma_addr, len);
        qtd
    }

    fn set_buffer(&mut self, dma_addr: u64, len: usize) {
        if len == 0 {
            return;
        }

        let first_page = dma_addr & !0xfff;
        for index in 0..5 {
            let page = first_page + (index as u64 * 4096);
            if page >= dma_addr + len as u64 {
                break;
            }
            self.buffer[index] = if index == 0 {
                dma_addr as u32
            } else {
                page as u32
            };
        }
    }

    fn token(&self) -> QtdToken {
        QtdToken(self.token)
    }

    fn set_next(&mut self, next: Option<u32>) {
        self.next_qtd = next.map(|addr| addr & !0x1f).unwrap_or(EHCI_LINK_TERMINATE);
    }
}

#[derive(Clone)]
struct TransferWakeups {
    count: Arc<AtomicUsize>,
}

impl TransferWakeups {
    fn new() -> Self {
        Self {
            count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn notify(&self) {
        self.count.fetch_add(1, Ordering::AcqRel);
    }

    fn take(&self) -> usize {
        self.count.swap(0, Ordering::AcqRel)
    }
}

pub struct Ehci {
    regs: EhciRegisters,
    kernel: Kernel,
    schedule: AsyncSchedule,
    root_hub: Option<EhciRootHub>,
    event_handler: Option<EhciEventHandler>,
    next_addr: u8,
}

unsafe impl Send for Ehci {}
unsafe impl Sync for Ehci {}

impl Ehci {
    pub fn new(params: EhciNewParams) -> Result<Self> {
        let regs = EhciRegisters::new(params.mmio);
        let kernel = Kernel::new(EHCI_DMA_MASK, params.kernel);
        let schedule = AsyncSchedule::new(&kernel)?;
        let wakeups = TransferWakeups::new();
        let root_hub = EhciRootHub::new(regs, kernel.clone());
        let event_handler = EhciEventHandler::new(regs, wakeups.clone());

        Ok(Self {
            regs,
            kernel,
            schedule,
            root_hub: Some(root_hub),
            event_handler: Some(event_handler),
            next_addr: 1,
        })
    }

    async fn init_controller(&mut self) -> Result<()> {
        self.disable_irq()?;
        self.regs.op_update32(USBCMD, |cmd| {
            cmd & !(USBCMD_RUN_STOP | USBCMD_ASYNC_ENABLE | USBCMD_PERIODIC_ENABLE)
        });
        self.wait_until(|| self.regs.op_read32(USBSTS) & USBSTS_HALTED != 0)?;

        self.regs.op_update32(USBCMD, |cmd| cmd | USBCMD_HCRESET);
        self.wait_until(|| self.regs.op_read32(USBCMD) & USBCMD_HCRESET == 0)?;

        self.regs.op_write32(CTRLDSSEGMENT, 0);
        self.regs.op_write32(PERIODICLISTBASE, 0);
        self.regs
            .op_write32(ASYNCLISTADDR, self.schedule.head_addr());
        self.regs.op_write32(CONFIGFLAG, 1);
        self.regs.op_write32(
            USBSTS,
            USBSTS_USBINT
                | USBSTS_USBERRINT
                | USBSTS_PORT_CHANGE
                | USBSTS_FRAME_LIST_ROLLOVER
                | USBSTS_HOST_SYSTEM_ERROR
                | USBSTS_INTERRUPT_ASYNC_ADVANCE,
        );

        self.regs.op_update32(USBCMD, |cmd| {
            (cmd | USBCMD_RUN_STOP | USBCMD_ASYNC_ENABLE) & !USBCMD_PERIODIC_ENABLE
        });
        self.wait_until(|| self.regs.op_read32(USBSTS) & USBSTS_HALTED == 0)?;
        self.kernel.delay(Duration::from_millis(100));
        Ok(())
    }

    fn wait_until(&self, ready: impl Fn() -> bool) -> Result<()> {
        for _ in 0..EHCI_WAIT_ITERS {
            if ready() {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(USBError::Timeout)
    }

    fn allocate_address(&mut self) -> Result<u8> {
        if self.next_addr >= 128 {
            return Err(USBError::SlotLimitReached);
        }
        let addr = self.next_addr;
        self.next_addr += 1;
        Ok(addr)
    }

    async fn new_device(&mut self, info: DeviceAddressInfo) -> Result<Box<dyn DeviceOp>> {
        if !matches!(info.port_speed, Speed::High) {
            return Err(USBError::NotSupported);
        }

        let addr = self.allocate_address()?;
        let mut device = EhciDevice::new(
            addr,
            self.regs,
            self.schedule.clone(),
            self.kernel.clone(),
            info.port_speed,
        )?;
        device.init().await?;
        Ok(Box::new(device))
    }
}

impl CoreOp for Ehci {
    fn init<'a>(&'a mut self) -> BoxFuture<'a, Result<()>> {
        self.init_controller().boxed()
    }

    fn root_hub(&mut self) -> Box<dyn HubOp> {
        Box::new(
            self.root_hub
                .take()
                .expect("EHCI root hub can only be taken once"),
        )
    }

    fn new_addressed_device<'a>(
        &'a mut self,
        addr: DeviceAddressInfo,
    ) -> BoxFuture<'a, Result<Box<dyn DeviceOp>>> {
        self.new_device(addr).boxed()
    }

    fn create_event_handler(&mut self) -> Box<dyn EventHandlerOp> {
        Box::new(
            self.event_handler
                .take()
                .expect("EHCI event handler can only be created once"),
        )
    }

    fn enable_irq(&mut self) -> Result<()> {
        self.regs.op_write32(
            USBINTR,
            USBINTR_USBINT | USBINTR_USBERRINT | USBINTR_PORT_CHANGE | USBINTR_ASYNC_ADVANCE,
        );
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<()> {
        self.regs.op_write32(USBINTR, 0);
        Ok(())
    }

    fn kernel(&self) -> &Kernel {
        &self.kernel
    }
}

struct EhciRootHub {
    regs: EhciRegisters,
    kernel: Kernel,
    ports: Vec<PortState>,
}

unsafe impl Send for EhciRootHub {}

impl EhciRootHub {
    fn new(regs: EhciRegisters, kernel: Kernel) -> Self {
        Self {
            regs,
            kernel,
            ports: vec![PortState::Uninit; regs.ports() as usize],
        }
    }

    async fn init_ports(&mut self, mut info: HubInfo) -> Result<HubInfo> {
        info.speed = Speed::High;
        for port_id in 1..=self.regs.ports() {
            self.regs.port_update(port_id, |value| {
                (value | PORT_POWER)
                    & !(PORT_CONNECT_CHANGE | PORT_ENABLE_CHANGE | PORT_OVER_CURRENT_CHANGE)
            });
        }
        self.kernel.delay(Duration::from_millis(20));
        Ok(info)
    }

    async fn changed_ports_inner(&mut self) -> Result<Vec<PortChangeInfo>> {
        let mut out = Vec::new();

        for port_id in 1..=self.regs.ports() {
            let idx = port_id as usize - 1;
            if matches!(self.ports[idx], PortState::Probed) {
                continue;
            }

            let status = self.regs.port_read(port_id);
            if !status.connected() {
                continue;
            }

            if matches!(self.ports[idx], PortState::Uninit) {
                self.reset_port(port_id);
                self.ports[idx] = PortState::Reseted;
            }

            let status = self.regs.port_read(port_id);
            if status.is_high_speed_device_ready() {
                self.ports[idx] = PortState::Probed;
                out.push(PortChangeInfo {
                    root_port_id: port_id,
                    port_id,
                    port_speed: Speed::High,
                    tt_port_on_hub: None,
                });
            } else if status.connected() && !status.enabled() {
                warn!(
                    "ehci: port {} has non-high-speed device ({:?}); handing to companion is not \
                     supported in v1",
                    port_id,
                    status.speed()
                );
                self.regs.port_update(port_id, |value| value | PORT_OWNER);
            }
        }

        Ok(out)
    }

    fn reset_port(&self, port_id: u8) {
        self.regs.port_update(port_id, |value| {
            (value | PORT_POWER | PORT_RESET)
                & !(PORT_CONNECT_CHANGE | PORT_ENABLE_CHANGE | PORT_OVER_CURRENT_CHANGE)
        });
        self.kernel.delay(Duration::from_millis(50));
        self.regs.port_update(port_id, |value| value & !PORT_RESET);
        self.kernel.delay(Duration::from_millis(20));
    }
}

impl HubOp for EhciRootHub {
    fn init<'a>(&'a mut self, info: HubInfo) -> BoxFuture<'a, Result<HubInfo>> {
        self.init_ports(info).boxed()
    }

    fn changed_ports<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<PortChangeInfo>>> {
        self.changed_ports_inner().boxed()
    }

    fn slot_id(&self) -> u8 {
        0
    }
}

struct EhciEventHandler {
    regs: EhciRegisters,
    wakeups: TransferWakeups,
}

unsafe impl Send for EhciEventHandler {}
unsafe impl Sync for EhciEventHandler {}

impl EhciEventHandler {
    fn new(regs: EhciRegisters, wakeups: TransferWakeups) -> Self {
        Self { regs, wakeups }
    }
}

impl EventHandlerOp for EhciEventHandler {
    fn handle_event(&self) -> Event {
        let sts = self.regs.op_read32(USBSTS);
        let pending = sts
            & (USBSTS_USBINT
                | USBSTS_USBERRINT
                | USBSTS_PORT_CHANGE
                | USBSTS_HOST_SYSTEM_ERROR
                | USBSTS_INTERRUPT_ASYNC_ADVANCE);
        if pending == 0 {
            return Event::Nothing;
        }

        self.regs.op_write32(USBSTS, pending);
        if pending & USBSTS_PORT_CHANGE != 0 {
            return Event::PortChange { port: 0 };
        }
        if pending & (USBSTS_USBINT | USBSTS_USBERRINT | USBSTS_INTERRUPT_ASYNC_ADVANCE) != 0 {
            self.wakeups.notify();
            return Event::TransferActivity {
                count: self.wakeups.take().max(1),
            };
        }
        Event::Stopped
    }
}

struct EhciDevice {
    address: u8,
    regs: EhciRegisters,
    schedule: AsyncSchedule,
    kernel: Kernel,
    _port_speed: Speed,
    desc: DeviceDescriptor,
    ctrl_ep: Endpoint,
    config_desc: Vec<ConfigurationDescriptor>,
    current_config_value: Option<u8>,
    eps: BTreeMap<u8, Endpoint>,
    ep_interfaces: BTreeMap<u8, u8>,
}

unsafe impl Send for EhciDevice {}

impl EhciDevice {
    fn new(
        address: u8,
        regs: EhciRegisters,
        schedule: AsyncSchedule,
        kernel: Kernel,
        port_speed: Speed,
    ) -> Result<Self> {
        let raw = EhciEndpoint::new(
            regs,
            schedule.clone(),
            kernel.clone(),
            0,
            EndpointInfo::control(),
        )?;
        Ok(Self {
            address,
            regs,
            schedule,
            kernel,
            _port_speed: port_speed,
            desc: unsafe { core::mem::zeroed() },
            ctrl_ep: Endpoint::new(EndpointInfo::control(), raw),
            config_desc: Vec::new(),
            current_config_value: None,
            eps: BTreeMap::new(),
            ep_interfaces: BTreeMap::new(),
        })
    }

    async fn init(&mut self) -> Result<()> {
        let base = self.get_device_descriptor_base().await?;
        self.set_address().await?;
        self.ctrl_ep
            .with_raw_mut::<EhciEndpoint, _>(|ep| ep.set_device_address(self.address));
        self.ctrl_ep
            .with_raw_mut::<EhciEndpoint, _>(|ep| ep.set_max_packet_size(base.max_packet_size_0));
        self.kernel.delay(Duration::from_millis(10));

        self.desc = self.ctrl_ep.get_device_descriptor().await?;
        self.current_config_value = Some(self.ctrl_ep.get_configuration().await?);
        for index in 0..self.desc.num_configurations {
            let config = self.ctrl_ep.get_configuration_descriptor(index).await?;
            self.config_desc.push(config);
        }
        if let Some(config) = self.config_desc.first() {
            self.set_configuration_inner(config.configuration_value)
                .await?;
        }
        Ok(())
    }

    async fn get_device_descriptor_base(&mut self) -> Result<DeviceDescriptorBase> {
        let mut data = [0u8; 8];
        self.ctrl_ep
            .get_descriptor(DescriptorType::DEVICE, 0, 0, &mut data)
            .await?;
        Ok(unsafe { (data.as_ptr() as *const DeviceDescriptorBase).read_unaligned() })
    }

    async fn set_address(&mut self) -> Result<()> {
        self.ctrl_ep
            .control_out(
                ControlSetup {
                    request_type: RequestType::Standard,
                    recipient: Recipient::Device,
                    request: Request::SetAddress,
                    value: self.address as u16,
                    index: 0,
                },
                &[],
            )
            .await?;
        Ok(())
    }

    async fn set_configuration_inner(&mut self, configuration_value: u8) -> Result<()> {
        self.ctrl_ep.set_configuration(configuration_value).await?;
        self.current_config_value = Some(configuration_value);
        self.eps.clear();
        self.ep_interfaces.clear();
        Ok(())
    }

    async fn claim_interface_inner(&mut self, interface: u8, alternate: u8) -> Result<()> {
        self.ctrl_ep
            .control_out(
                ControlSetup {
                    request_type: RequestType::Standard,
                    recipient: Recipient::Interface,
                    request: Request::SetInterface,
                    value: alternate as u16,
                    index: interface as u16,
                },
                &[],
            )
            .await?;
        self.setup_interface_endpoints(interface, alternate)?;
        Ok(())
    }

    fn setup_interface_endpoints(&mut self, interface: u8, alternate: u8) -> Result<()> {
        let endpoints = self
            .find_interface_endpoints(interface, alternate)?
            .to_vec();
        for desc in endpoints {
            if matches!(desc.transfer_type, EndpointType::Isochronous) {
                warn!(
                    "ehci: isochronous endpoint {:#x} is not supported in v1",
                    desc.address
                );
                continue;
            }
            let info = EndpointInfo::from(&desc);
            let raw = EhciEndpoint::new(
                self.regs,
                self.schedule.clone(),
                self.kernel.clone(),
                self.address,
                info,
            )?;
            self.eps.insert(desc.address, Endpoint::new(info, raw));
            self.ep_interfaces.insert(desc.address, interface);
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
                if iface.interface_number != interface {
                    continue;
                }
                for alt in &iface.alt_settings {
                    if alt.alternate_setting == alternate {
                        return Ok(&alt.endpoints);
                    }
                }
            }
        }
        Err(USBError::NotFound)
    }
}

impl DeviceOp for EhciDevice {
    fn id(&self) -> usize {
        self.address as usize
    }

    fn backend_name(&self) -> &str {
        "ehci"
    }

    fn descriptor(&self) -> &DeviceDescriptor {
        &self.desc
    }

    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor] {
        &self.config_desc
    }

    fn ctrl_ep_ref(&self) -> &Endpoint {
        &self.ctrl_ep
    }

    fn ctrl_ep_mut(&mut self) -> &mut Endpoint {
        &mut self.ctrl_ep
    }

    fn claim_interface<'a>(
        &'a mut self,
        interface: u8,
        alternate: u8,
    ) -> BoxFuture<'a, Result<()>> {
        self.claim_interface_inner(interface, alternate).boxed()
    }

    fn set_configuration<'a>(&'a mut self, configuration_value: u8) -> BoxFuture<'a, Result<()>> {
        self.set_configuration_inner(configuration_value).boxed()
    }

    fn endpoint(&mut self, desc: &EndpointDescriptor) -> Result<Endpoint> {
        self.eps.remove(&desc.address).ok_or(USBError::NotFound)
    }

    fn update_hub(&mut self, _params: HubParams) -> BoxFuture<'_, Result<()>> {
        async { Ok(()) }.boxed()
    }
}

struct EhciEndpoint {
    regs: EhciRegisters,
    schedule: AsyncSchedule,
    kernel: Kernel,
    qh: CoherentBox<QueueHead>,
    next_request_id: u64,
    inflight: Option<SubmittedTransfer>,
    waker: AtomicWaker,
}

unsafe impl Send for EhciEndpoint {}

impl EhciEndpoint {
    fn new(
        regs: EhciRegisters,
        schedule: AsyncSchedule,
        kernel: Kernel,
        device_address: u8,
        info: EndpointInfo,
    ) -> Result<Self> {
        let mut qh = kernel
            .coherent_box_zero_with_align::<QueueHead>(32)
            .map_err(HostError::from)?;
        qh.write_cpu(QueueHead::endpoint(
            device_address,
            endpoint_number(info.address.raw()),
            info.transfer_type,
            info.max_packet_size.max(8),
        ));

        Ok(Self {
            regs,
            schedule,
            kernel,
            qh,
            next_request_id: 1,
            inflight: None,
            waker: AtomicWaker::new(),
        })
    }

    fn set_device_address(&mut self, address: u8) {
        self.qh.modify_cpu(|qh| {
            qh.endpoint_chars = (qh.endpoint_chars & !0x7f) | (address as u32 & 0x7f);
        });
    }

    fn set_max_packet_size(&mut self, max_packet_size: u8) {
        let max_packet_size = max_packet_size.max(8) as u32;
        self.qh.modify_cpu(|qh| {
            qh.endpoint_chars = (qh.endpoint_chars & !(0x7ff << 16)) | (max_packet_size << 16);
        });
    }

    fn allocate_request_id(&mut self) -> RequestId {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        RequestId::new(id)
    }

    fn build_qtds(
        &self,
        transfer: &Transfer,
        request: &TransferRequest,
    ) -> Result<SubmittedTransfer> {
        let mut qtds = Vec::new();
        let mut chunk_lengths = Vec::new();
        let mut setup_packet = None;

        match request {
            TransferRequest::Control {
                setup, direction, ..
            } => {
                let setup_bytes = setup_packet_bytes(setup, *direction, transfer.buffer_len());
                let mut setup_dma = self
                    .kernel
                    .coherent_box_zero_with_align::<[u8; 8]>(8)
                    .map_err(HostError::from)?;
                setup_dma.write_cpu(setup_bytes);
                let plan = build_control_td_plan(request)?;
                qtds.push(new_qtd(
                    &self.kernel,
                    plan.setup,
                    setup_dma.dma_addr().as_u64(),
                    8,
                )?);
                chunk_lengths.push(0);
                if let Some(data_token) = plan.data {
                    qtds.push(new_qtd(
                        &self.kernel,
                        data_token,
                        transfer.dma_addr(),
                        transfer.buffer_len(),
                    )?);
                    chunk_lengths.push(transfer.buffer_len());
                }
                qtds.push(new_qtd(&self.kernel, plan.status, 0, 0)?);
                chunk_lengths.push(0);
                setup_packet = Some(setup_dma);
            }
            TransferRequest::Bulk { direction, .. }
            | TransferRequest::Interrupt { direction, .. } => {
                let chunks = split_bulk_lengths(transfer.buffer_len(), *direction);
                let mut offset = 0u64;
                for (idx, len) in chunks.iter().copied().enumerate() {
                    let pid = match direction {
                        Direction::In => QtdPid::In,
                        Direction::Out => QtdPid::Out,
                    };
                    let token = QtdToken::new(pid, len)
                        .with_error_counter(3)
                        .with_interrupt_on_complete(idx + 1 == chunks.len())
                        .with_active(true);
                    qtds.push(new_qtd(
                        &self.kernel,
                        token,
                        transfer.dma_addr() + offset,
                        len,
                    )?);
                    chunk_lengths.push(len);
                    offset += len as u64;
                }
            }
            TransferRequest::Isochronous { .. } => return Err(USBError::NotSupported),
        }

        link_qtds(&mut qtds);

        Ok(SubmittedTransfer {
            request_id: RequestId::new(0),
            transfer: None,
            qtds,
            chunk_lengths,
            _setup_packet: setup_packet,
            cancelled: false,
        })
    }

    fn start_transfer(&mut self, submitted: &SubmittedTransfer) -> Result<()> {
        let first_qtd = submitted
            .qtds
            .first()
            .ok_or(USBError::InvalidParameter)?
            .dma_addr()
            .as_u64() as u32;
        self.qh.modify_cpu(|qh| qh.set_next_qtd(first_qtd));
        self.schedule.attach(&mut self.qh)?;
        mb();
        self.regs
            .op_update32(USBCMD, |cmd| cmd | USBCMD_ASYNC_ENABLE | USBCMD_RUN_STOP);
        Ok(())
    }

    fn finish_if_ready(
        &mut self,
        id: RequestId,
    ) -> Option<core::result::Result<TransferCompletion, TransferError>> {
        let submitted = self.inflight.as_mut()?;
        if submitted.request_id != id {
            return Some(Err(TransferError::InvalidEndpoint));
        }

        let mut actual_length = 0usize;
        let mut all_done = true;
        let mut error = None;
        for (qtd, requested) in submitted
            .qtds
            .iter()
            .zip(submitted.chunk_lengths.iter().copied())
        {
            let token = qtd.read_cpu().token();
            if token.active() {
                all_done = false;
                break;
            }
            if token.has_error() {
                error = Some(TransferError::Other(anyhow!(
                    "EHCI qTD failed token={:#x}",
                    token.raw()
                )));
                break;
            }
            actual_length += requested.saturating_sub(token.total_bytes());
        }

        if !all_done {
            return None;
        }

        let mut submitted = self.inflight.take()?;
        let qh_addr = self.qh.dma_addr().as_u64() as u32;
        self.schedule.detach(qh_addr);
        self.regs
            .op_update32(USBCMD, |cmd| cmd | USBCMD_INT_ASYNC_ADVANCE_DOORBELL);

        if submitted.cancelled {
            return Some(Err(TransferError::Cancelled));
        }
        if let Some(err) = error {
            return Some(Err(err));
        }

        let Some(transfer) = submitted.transfer.take() else {
            return Some(Err(TransferError::Other(anyhow!(
                "EHCI transfer missing state"
            ))));
        };
        if actual_length > 0 && matches!(transfer.direction, Direction::In) {
            transfer.complete_for_cpu_all();
        }

        Some(Ok(TransferCompletion {
            request_id: id,
            status: TransferStatus::Completed,
            actual_length,
            iso_packets: Vec::new(),
        }))
    }
}

impl crate::backend::ty::ep::EndpointOp for EhciEndpoint {
    fn submit_request(
        &mut self,
        request: TransferRequest,
    ) -> core::result::Result<RequestId, TransferError> {
        if self.inflight.is_some() {
            return Err(TransferError::QueueFull);
        }
        if matches!(request, TransferRequest::Isochronous { .. }) {
            return Err(TransferError::NotSupported);
        }

        let transfer = Transfer::from_request(&self.kernel, request.clone())?;
        if transfer.buffer_len() > 0 && matches!(transfer.direction, Direction::Out) {
            transfer.prepare_for_device_all();
        }
        let mut submitted = self
            .build_qtds(&transfer, &request)
            .map_err(usb_to_transfer_error)?;
        let id = self.allocate_request_id();
        submitted.request_id = id;
        submitted.transfer = Some(transfer);
        self.start_transfer(&submitted)
            .map_err(usb_to_transfer_error)?;
        self.inflight = Some(submitted);
        Ok(id)
    }

    fn reclaim_request(
        &mut self,
        id: RequestId,
    ) -> Option<core::result::Result<TransferCompletion, TransferError>> {
        self.finish_if_ready(id)
    }

    fn register_waker(&self, _id: RequestId, cx: &mut Context<'_>) {
        self.waker.register(cx.waker());
        cx.waker().wake_by_ref();
    }

    fn cancel_request(&mut self, id: RequestId) -> core::result::Result<(), TransferError> {
        let submitted = self
            .inflight
            .as_mut()
            .ok_or(TransferError::InvalidEndpoint)?;
        if submitted.request_id != id {
            return Err(TransferError::InvalidEndpoint);
        }
        submitted.cancelled = true;
        Ok(())
    }
}

struct SubmittedTransfer {
    request_id: RequestId,
    transfer: Option<Transfer>,
    qtds: Vec<CoherentBox<QueueTransferDescriptor>>,
    chunk_lengths: Vec<usize>,
    _setup_packet: Option<CoherentBox<[u8; 8]>>,
    cancelled: bool,
}

fn new_qtd(
    kernel: &Kernel,
    token: QtdToken,
    dma_addr: u64,
    len: usize,
) -> Result<CoherentBox<QueueTransferDescriptor>> {
    let mut qtd = kernel
        .coherent_box_zero_with_align::<QueueTransferDescriptor>(32)
        .map_err(HostError::from)?;
    qtd.write_cpu(QueueTransferDescriptor::new(token, dma_addr, len));
    Ok(qtd)
}

fn link_qtds(qtds: &mut [CoherentBox<QueueTransferDescriptor>]) {
    let mut next_addrs = Vec::with_capacity(qtds.len());
    for qtd in qtds.iter() {
        next_addrs.push(qtd.dma_addr().as_u64() as u32);
    }
    for (idx, qtd) in qtds.iter_mut().enumerate() {
        let next = next_addrs.get(idx + 1).copied();
        qtd.modify_cpu(|qtd| qtd.set_next(next));
    }
}

fn endpoint_number(address: u8) -> u8 {
    address & 0x0f
}

fn setup_packet_bytes(setup: &ControlSetup, direction: Direction, len: usize) -> [u8; 8] {
    let request_type = BmRequestType::new(direction, setup.request_type, setup.recipient);
    let value = setup.value.to_le_bytes();
    let index = setup.index.to_le_bytes();
    let len = (len as u16).to_le_bytes();
    [
        request_type.into(),
        setup.request.into(),
        value[0],
        value[1],
        index[0],
        index[1],
        len[0],
        len[1],
    ]
}

fn usb_to_transfer_error(err: USBError) -> TransferError {
    match err {
        USBError::TransferError(err) => err,
        USBError::NoMemory => TransferError::Other(anyhow!("EHCI DMA allocation failed")),
        USBError::Timeout => TransferError::Timeout,
        USBError::NotSupported => TransferError::NotSupported,
        USBError::NotFound | USBError::InvalidParameter => TransferError::InvalidEndpoint,
        err => TransferError::Other(anyhow!("EHCI transfer setup failed: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use usb_if::{
        endpoint::TransferRequest,
        host::{ControlSetup, hub::Speed},
        transfer::{Direction, Recipient, Request, RequestType},
    };

    use super::{
        ControlTdPlan, EhciPortStatus, QtdPid, QtdToken, build_control_td_plan, split_bulk_lengths,
    };

    #[test]
    fn port_status_reports_high_speed_only_when_enabled_and_line_status_is_k_state() {
        let status = EhciPortStatus::from_raw(
            EhciPortStatus::CURRENT_CONNECT
                | EhciPortStatus::PORT_ENABLED
                | EhciPortStatus::LINE_STATUS_K_STATE,
        );

        assert_eq!(status.speed(), Speed::High);
        assert!(status.is_high_speed_device_ready());
    }

    #[test]
    fn port_status_rejects_low_or_full_speed_for_ehci_v1() {
        let low_speed = EhciPortStatus::from_raw(
            EhciPortStatus::CURRENT_CONNECT | EhciPortStatus::LINE_STATUS_K_STATE,
        );
        let full_speed = EhciPortStatus::from_raw(EhciPortStatus::CURRENT_CONNECT);

        assert_eq!(low_speed.speed(), Speed::Low);
        assert_eq!(full_speed.speed(), Speed::Full);
        assert!(!low_speed.is_high_speed_device_ready());
        assert!(!full_speed.is_high_speed_device_ready());
    }

    #[test]
    fn qtd_token_encodes_pid_length_ioc_active_and_error_counter() {
        let token = QtdToken::new(QtdPid::In, 512)
            .with_interrupt_on_complete(true)
            .with_error_counter(3)
            .with_active(true);

        assert_eq!(token.raw() & QtdToken::PID_MASK, QtdToken::PID_IN);
        assert_eq!(token.total_bytes(), 512);
        assert!(token.interrupt_on_complete());
        assert_eq!(token.error_counter(), 3);
        assert!(token.active());
    }

    #[test]
    fn control_td_plan_uses_setup_data_and_opposite_direction_status_stage() {
        let mut data = [0u8; 18];
        let request = TransferRequest::control_in(
            ControlSetup {
                request_type: RequestType::Standard,
                recipient: Recipient::Device,
                request: Request::GetDescriptor,
                value: 0x0100,
                index: 0,
            },
            &mut data,
        );

        let ControlTdPlan {
            setup,
            data,
            status,
        } = build_control_td_plan(&request).expect("control IN request should be supported");

        assert_eq!(setup.pid(), QtdPid::Setup);
        assert_eq!(setup.total_bytes(), 8);
        assert_eq!(data.expect("data stage").pid(), QtdPid::In);
        assert_eq!(status.pid(), QtdPid::Out);
        assert!(status.interrupt_on_complete());
    }

    #[test]
    fn control_td_plan_has_no_data_stage_for_zero_length_control_out() {
        let request = TransferRequest::control_out(
            ControlSetup {
                request_type: RequestType::Standard,
                recipient: Recipient::Device,
                request: Request::SetAddress,
                value: 7,
                index: 0,
            },
            &[],
        );

        let plan = build_control_td_plan(&request).expect("zero-length control OUT is supported");

        assert_eq!(plan.setup.pid(), QtdPid::Setup);
        assert!(plan.data.is_none());
        assert_eq!(plan.status.pid(), QtdPid::In);
    }

    #[test]
    fn bulk_transfers_are_split_to_qtd_max_total_bytes() {
        let lengths = split_bulk_lengths(48 * 1024, Direction::Out);

        assert_eq!(lengths, [20 * 1024, 20 * 1024, 8 * 1024]);
    }
}
