mod channel;
mod dma;
mod endpoint;
mod event;
mod hub;
mod reg;
mod stats;

#[cfg(test)]
mod testutil;

use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
use core::{task::Poll, time::Duration};

use futures::{
    FutureExt,
    future::{BoxFuture, poll_fn},
};
use reg::*;
pub use stats::Dwc2TransferStats;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use usb_if::{
    descriptor::{
        ConfigurationDescriptor, DescriptorType, DeviceDescriptor, DeviceDescriptorBase,
        EndpointDescriptor, EndpointType,
    },
    endpoint::EndpointInfo,
    err::{TransferError, USBError},
    host::{ControlSetup, hub::Speed},
    transfer::{Direction, Recipient, Request, RequestType},
};

use super::{
    hub::HubOp,
    kcore::CoreOp,
    osal::{Kernel, KernelOp},
};
use crate::{
    Mmio,
    backend::{
        kmod::{
            DeviceAddressInfo,
            dwc2::{
                channel::{Dwc2ChannelCompletions, Dwc2PeriodicSchedule, HostChannelPool},
                endpoint::{Dwc2Endpoint, Dwc2EndpointParams},
                event::Dwc2EventHandler,
                hub::Dwc2RootHub,
                reg::Dwc2Registers,
                stats::Dwc2Stats,
            },
        },
        ty::{
            DeviceOp, EventHandlerOp, HubParams,
            ep::{EndpointHandle, EndpointOp},
        },
    },
    err::Result,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dwc2UtmiWidth {
    Eight,
    Sixteen,
    Auto,
}

/// Hardware signal that confirms a DWC2 core soft reset has completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dwc2SoftResetCompletion {
    StartBitCleared,
    DoneBitSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dwc2FifoSizes {
    pub rx_depth: u16,
    pub non_periodic_tx_depth: u16,
    pub periodic_tx_depth: u16,
}

impl Dwc2FifoSizes {
    pub const fn sg2002_default() -> Self {
        Self {
            rx_depth: 536,
            non_periodic_tx_depth: 32,
            periodic_tx_depth: 768,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dwc2Quirks {
    pub otg_host_session_override: bool,
    pub clear_utmi_override: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dwc2HostParams {
    pub dma_mask: u64,
    pub fifo: Dwc2FifoSizes,
    pub utmi: Dwc2UtmiWidth,
    pub soft_reset_completion: Dwc2SoftResetCompletion,
    pub quirks: Dwc2Quirks,
}

impl Dwc2HostParams {
    pub const fn sg2002() -> Self {
        Self {
            dma_mask: DWC2_DMA_MASK_32,
            fifo: Dwc2FifoSizes::sg2002_default(),
            utmi: Dwc2UtmiWidth::Auto,
            // SG2002 exposes the post-4.20a CSFTRST_DONE handshake even
            // though its GSNPSID revision field reports an older value.
            soft_reset_completion: Dwc2SoftResetCompletion::DoneBitSet,
            quirks: Dwc2Quirks {
                otg_host_session_override: true,
                clear_utmi_override: true,
            },
        }
    }
}

#[derive(Clone, Copy)]
pub struct Dwc2NewParams {
    pub mmio: Mmio,
    pub kernel: &'static dyn KernelOp,
    pub params: Dwc2HostParams,
}

pub struct Dwc2 {
    regs: Dwc2Registers,
    kernel: Kernel,
    params: Dwc2HostParams,
    root_hub: Option<Dwc2RootHub>,
    event_handler: Option<Dwc2EventHandler>,
    next_addr: u8,
    channel_pool: HostChannelPool,
    stats: Dwc2Stats,
}

unsafe impl Send for Dwc2 {}
unsafe impl Sync for Dwc2 {}

impl Dwc2 {
    pub fn new(params: Dwc2NewParams) -> Result<Self> {
        if params.params.dma_mask != DWC2_DMA_MASK_32 {
            return Err(USBError::NotSupported);
        }

        let regs = Dwc2Registers::new(params.mmio);
        let kernel = Kernel::new(
            dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Direct,
                dma_api::DmaCoherency::NonCoherent,
                dma_api::DmaConstraints::new(params.params.dma_mask),
            ),
            params.kernel,
        );
        let root_hub = Dwc2RootHub::new(regs, kernel.clone());
        let channel_completions = Dwc2ChannelCompletions::new();
        let stats = Dwc2Stats::new();
        let event_handler = Dwc2EventHandler::new(regs, channel_completions.clone(), stats.clone());
        let channel_count = regs.host_channel_count();
        let periodic = Dwc2PeriodicSchedule::new(&kernel)
            .map_err(|err| USBError::Other(anyhow!("DWC2 frame list allocation failed: {err}")))?;

        Ok(Self {
            regs,
            kernel,
            params: params.params,
            root_hub: Some(root_hub),
            event_handler: Some(event_handler),
            next_addr: 1,
            channel_pool: HostChannelPool::new(
                channel_count,
                channel_completions,
                Arc::new(periodic),
            ),
            stats,
        })
    }

    async fn init_controller(&mut self) -> Result<()> {
        self.disable_irq()?;
        self.regs.regs().gintsts.set(u32::MAX);
        self.core_soft_reset()?;
        log::debug!("dwc2: initial core reset complete");
        self.force_host_mode()?;
        log::debug!("dwc2: host mode active");
        self.core_soft_reset()?;
        log::debug!("dwc2: host-mode core reset complete");

        if self.params.quirks.otg_host_session_override {
            let gotgctl = self.regs.regs().gotgctl.get();
            self.regs.regs().gotgctl.set(
                gotgctl
                    | GOTGCTL_DBNCE_FLTR_BYPASS
                    | GOTGCTL_AVALOEN
                    | GOTGCTL_AVALOVAL
                    | GOTGCTL_VBVALOEN
                    | GOTGCTL_VBVALOVAL,
            );
            self.kernel.delay(Duration::from_micros(200));
        }

        self.init_gusbcfg();
        self.regs.regs().pcgctl.set(0);

        let arch = self.regs.regs().ghwcfg2.read(GHWCFG2::ARCHITECTURE);
        let gahbcfg = build_gahbcfg_internal_dma(arch)?;
        self.regs.regs().gahbcfg.set(gahbcfg);

        // 硬件不具备 DDMA 能力时直接拒绝，
        if !self.regs.is_support_ddma() {
            log::error!("dwc2: controller lacks descriptor DMA capability");
            return Err(USBError::NotSupported);
        }

        self.regs
            .regs()
            .hcfg
            .modify(HCFG::FSLSPCLKSEL::CLEAR + HCFG::DESCDMA::SET);
        log::debug!("dwc2: descriptor DMA enabled");

        let fifo = fifo_register_plan(self.params.fifo);
        self.regs.regs().grxfsiz.set(fifo.grxfsiz);
        self.regs.regs().gnptxfsiz.set(fifo.gnptxfsiz);
        self.regs.regs().hptxfsiz.set(fifo.hptxfsiz);
        self.flush_tx_fifo_all()?;
        log::debug!("dwc2: TX FIFOs flushed");
        self.flush_rx_fifo()?;
        log::debug!("dwc2: RX FIFO flushed");

        let channel_count = self.regs.host_channel_count();
        self.prepare_runtime_irqs(channel_count);
        log::debug!("dwc2: runtime IRQ state prepared");
        self.port_power_on();
        log::debug!("dwc2: root port powered");
        self.kernel.delay(Duration::from_millis(20));
        log::debug!("dwc2: controller initialization settled");
        Ok(())
    }

    fn prepare_runtime_irqs(&self, channel_count: u8) {
        // channel_count 已钳制到 2..=16，`1 << 16 − 1` 即为全 16 位 HAINTMSK。
        let channel_mask = (1u32 << channel_count) - 1;
        // The caller registers the controller IRQ before initialization, but
        // only `CoreOp::enable_irq` publishes runtime events. Clear stale
        // status while masked so port power-on cannot re-enter half-built HCD
        // state through PRTINT/HCHINT.
        self.regs.regs().gintmsk.set(0);
        self.regs.regs().gintsts.set(u32::MAX);
        self.regs.regs().haintmsk.set(channel_mask);
    }

    fn init_gusbcfg(&self) {
        let want_16bit = match self.params.utmi {
            Dwc2UtmiWidth::Eight => false,
            Dwc2UtmiWidth::Sixteen => true,
            Dwc2UtmiWidth::Auto => self.regs.regs().ghwcfg4.read(GHWCFG4::UTMI_PHY_DATA_WIDTH) == 1,
        };

        let mut value = self.regs.regs().gusbcfg.get();
        value &= !(GUSBCFG_TOUTCAL_MASK
            | GUSBCFG_PHYIF16
            | GUSBCFG_ULPI_UTMI_SEL
            | GUSBCFG_FORCEDEVMODE);
        value |= GUSBCFG_FORCEHOSTMODE | 0x7;
        if want_16bit {
            value |= GUSBCFG_PHYIF16;
        }
        self.regs.regs().gusbcfg.set(value);
    }

    fn wait_until(&self, stage: &'static str, ready: impl Fn() -> bool) -> Result<()> {
        for iter in 0..DWC2_WAIT_ITERS {
            if ready() {
                self.stats.record_init_wait_iters(iter + 1);
                return Ok(());
            }
            core::hint::spin_loop();
        }
        self.stats.record_init_wait_iters(DWC2_WAIT_ITERS);
        self.stats.record_timeout();
        log::warn!(
            "dwc2: {stage} timed out gsnpsid={:#010x} grstctl={:#010x} gusbcfg={:#010x} \
             gintsts={:#010x}",
            self.regs.regs().gsnpsid.get(),
            self.regs.regs().grstctl.get(),
            self.regs.regs().gusbcfg.get(),
            self.regs.regs().gintsts.get(),
        );
        Err(USBError::Timeout)
    }

    fn wait_ahb_idle(&self) -> Result<()> {
        self.wait_until("AHB idle", || {
            self.regs.regs().grstctl.get() & GRSTCTL_AHBIDLE != 0
        })
    }

    fn core_soft_reset(&self) -> Result<()> {
        let value = self.regs.regs().grstctl.get();
        self.regs.regs().grstctl.set(value | GRSTCTL_CSFTRST);
        match self.params.soft_reset_completion {
            Dwc2SoftResetCompletion::StartBitCleared => {
                self.wait_until("core soft reset clear", || {
                    self.regs.regs().grstctl.get() & GRSTCTL_CSFTRST == 0
                })?;
            }
            Dwc2SoftResetCompletion::DoneBitSet => {
                self.wait_until("core soft reset done", || {
                    self.regs.regs().grstctl.get() & GRSTCTL_CSFTRST_DONE != 0
                })?;
                let value = self.regs.regs().grstctl.get();
                self.regs
                    .regs()
                    .grstctl
                    .set((value & !GRSTCTL_CSFTRST) | GRSTCTL_CSFTRST_DONE);
            }
        }
        self.wait_ahb_idle()?;
        self.kernel.delay(Duration::from_millis(1));
        Ok(())
    }

    fn force_host_mode(&self) -> Result<()> {
        let value = self.regs.regs().gusbcfg.get();
        self.regs
            .regs()
            .gusbcfg
            .set((value | GUSBCFG_FORCEHOSTMODE) & !GUSBCFG_FORCEDEVMODE);
        self.kernel.delay(Duration::from_millis(25));
        self.wait_until("force host mode", || {
            self.regs.regs().gintsts.get() & GINTSTS_CURMODE_HOST != 0
        })
    }

    fn flush_tx_fifo_all(&self) -> Result<()> {
        self.regs
            .regs()
            .grstctl
            .set(GRSTCTL_TXFFLSH | GRSTCTL_TXFNUM_ALL);
        self.wait_until("flush all TX FIFOs", || {
            self.regs.regs().grstctl.get() & GRSTCTL_TXFFLSH == 0
        })?;
        self.kernel.delay(Duration::from_micros(1));
        Ok(())
    }

    fn flush_rx_fifo(&self) -> Result<()> {
        self.regs.regs().grstctl.set(GRSTCTL_RXFFLSH);
        self.wait_until("flush RX FIFO", || {
            self.regs.regs().grstctl.get() & GRSTCTL_RXFFLSH == 0
        })?;
        self.kernel.delay(Duration::from_micros(1));
        Ok(())
    }

    fn port_power_on(&self) {
        self.regs.hprt().update_safe(|value| value | HPRT_PWR);
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
        let channel_count = self.channel_pool.channel_count;
        let channel_mask = (1u32 << channel_count) - 1;
        self.channel_pool.completions.mark_connected(|| {
            self.regs.regs().haintmsk.set(channel_mask);
            let mask = self.regs.regs().gintmsk.get();
            self.regs.regs().gintmsk.set(mask | DWC2_RUNTIME_GINTMSK);
        });
        let addr = self.allocate_address()?;
        let mut device = Dwc2Device::new(Dwc2DeviceParams {
            address: addr,
            regs: self.regs,
            kernel: self.kernel.clone(),
            port_speed: info.port_speed,
            channel_pool: self.channel_pool.clone(),
            stats: self.stats.clone(),
        })?;
        device.init().await?;
        Ok(Box::new(device))
    }
}

impl CoreOp for Dwc2 {
    fn init<'a>(&'a mut self) -> BoxFuture<'a, Result<()>> {
        self.init_controller().boxed()
    }

    fn root_hub(&mut self) -> Box<dyn HubOp> {
        Box::new(
            self.root_hub
                .take()
                .expect("DWC2 root hub can only be taken once"),
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
                .expect("DWC2 event handler can only be created once"),
        )
    }

    fn enable_irq(&mut self) -> Result<()> {
        self.regs.regs().gintmsk.set(DWC2_RUNTIME_GINTMSK);
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<()> {
        self.regs.regs().gintmsk.set(0);
        Ok(())
    }

    fn dwc2_transfer_stats(&self) -> Option<Dwc2TransferStats> {
        Some(self.stats.snapshot())
    }

    fn reset_dwc2_transfer_stats(&self) {
        self.stats.reset();
    }

    fn kernel(&self) -> &Kernel {
        &self.kernel
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dwc2Pid {
    Data0,
    Data2,
    Data1,
    Setup,
    MData,
}

impl Dwc2Pid {
    const fn bits(self) -> u32 {
        match self {
            Self::Data0 => 0,
            Self::Data2 => 1,
            Self::Data1 => 2,
            Self::Setup => 3,
            Self::MData => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dwc2EpType {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

impl Dwc2EpType {
    const fn bits(self) -> u32 {
        match self {
            Self::Control => 0,
            Self::Isochronous => 1,
            Self::Bulk => 2,
            Self::Interrupt => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FifoRegisterPlan {
    pub(crate) grxfsiz: u32,
    pub(crate) gnptxfsiz: u32,
    pub(crate) hptxfsiz: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dwc2TransferFault {
    Nak,
    Stall,
    Ahb,
    Xact,
    Babble,
    FrameOverrun,
    DataToggle,
    HaltedWithoutComplete,
}

pub(crate) fn build_gahbcfg_internal_dma(arch: u32) -> core::result::Result<u32, USBError> {
    if arch != 2 {
        return Err(USBError::NotSupported);
    }
    Ok((1 << 0) | (7 << 1) | (1 << 5))
}

pub(crate) fn fifo_register_plan(fifo: Dwc2FifoSizes) -> FifoRegisterPlan {
    let rx = u32::from(fifo.rx_depth);
    let nptx = u32::from(fifo.non_periodic_tx_depth);
    let ptx = u32::from(fifo.periodic_tx_depth);
    FifoRegisterPlan {
        grxfsiz: rx,
        gnptxfsiz: (nptx << 16) | rx,
        hptxfsiz: (ptx << 16) | (rx + nptx),
    }
}

/// HCTSIZ 的 DDMA 编码：PID + NTD（描述符数 − 1）+ SCHINFO。
/// XFERSIZE/PKTCNT 在 Descriptor DMA 模式不使用（Linux 同）。
pub(crate) fn hctsiz_ddma(pid: Dwc2Pid, n_descs: u32, schinfo: u32) -> u32 {
    ((pid.bits() & 0b11) << 29) | ((n_descs.saturating_sub(1).min(0xff)) << 8) | (schinfo & 0xff)
}

pub(crate) fn hcchar(
    device: u8,
    endpoint: u8,
    direction: Direction,
    ep_type: Dwc2EpType,
    max_packet_size: u16,
    low_speed: bool,
    mult: u8,
) -> u32 {
    let mut value = u32::from(max_packet_size.max(1)) & 0x7ff;
    value |= (u32::from(endpoint) & 0x0f) << 11;
    value |= (direction as u32) << 15;
    if low_speed {
        value |= 1 << 17;
    }
    value |= ep_type.bits() << 18;
    // MULTICNT = mult − 1（ISO/INT 多事务计数）。
    value |= (u32::from(mult.saturating_sub(1)) & 0x3) << 20;
    value |= (u32::from(device) & 0x7f) << 22;
    value
}

pub(crate) fn hcint_fault(bits: u32) -> Option<Dwc2TransferFault> {
    if bits & HCINT_STALL != 0 {
        Some(Dwc2TransferFault::Stall)
    } else if bits & HCINT_NAK != 0 {
        Some(Dwc2TransferFault::Nak)
    } else if bits & HCINT_AHBERR != 0 {
        Some(Dwc2TransferFault::Ahb)
    } else if bits & HCINT_XACTERR != 0 {
        Some(Dwc2TransferFault::Xact)
    } else if bits & HCINT_BBLERR != 0 {
        Some(Dwc2TransferFault::Babble)
    } else if bits & HCINT_FRMOVRN != 0 {
        Some(Dwc2TransferFault::FrameOverrun)
    } else if bits & HCINT_DATATGLERR != 0 {
        Some(Dwc2TransferFault::DataToggle)
    } else if bits & (HCINT_CHHLTD | HCINT_XFERCOMPL) != 0 {
        None
    } else {
        Some(Dwc2TransferFault::HaltedWithoutComplete)
    }
}

pub(crate) fn fault_to_transfer_error(fault: Dwc2TransferFault, hcint: u32) -> TransferError {
    match fault {
        Dwc2TransferFault::Stall => TransferError::Stall,
        Dwc2TransferFault::Nak => TransferError::Other(anyhow!("DWC2 transfer NAK")),
        Dwc2TransferFault::Ahb => TransferError::Other(anyhow!("DWC2 AHB error hcint={hcint:#x}")),
        Dwc2TransferFault::Xact => {
            TransferError::Other(anyhow!("DWC2 transaction error hcint={hcint:#x}"))
        }
        Dwc2TransferFault::Babble => {
            TransferError::Other(anyhow!("DWC2 babble error hcint={hcint:#x}"))
        }
        Dwc2TransferFault::FrameOverrun => {
            TransferError::Other(anyhow!("DWC2 frame overrun hcint={hcint:#x}"))
        }
        Dwc2TransferFault::DataToggle => {
            TransferError::Other(anyhow!("DWC2 data toggle error hcint={hcint:#x}"))
        }
        Dwc2TransferFault::HaltedWithoutComplete => {
            TransferError::Other(anyhow!("DWC2 halted without completion hcint={hcint:#x}"))
        }
    }
}

pub(crate) fn endpoint_number(address: u8) -> u8 {
    address & 0x0f
}

pub(crate) fn endpoint_type_to_dwc2(ty: EndpointType) -> Result<Dwc2EpType> {
    match ty {
        EndpointType::Control => Ok(Dwc2EpType::Control),
        EndpointType::Isochronous => Ok(Dwc2EpType::Isochronous),
        EndpointType::Bulk => Ok(Dwc2EpType::Bulk),
        EndpointType::Interrupt => Ok(Dwc2EpType::Interrupt),
    }
}

pub(crate) fn dma_addr32(addr: u64) -> core::result::Result<u32, TransferError> {
    u32::try_from(addr)
        .map_err(|_| TransferError::Other(anyhow!("DWC2 DMA address above 32-bit mask: {addr:#x}")))
}

fn device_descriptor_base_from_bytes(data: [u8; 8]) -> DeviceDescriptorBase {
    DeviceDescriptorBase {
        length: data[0],
        descriptor_type: data[1],
        usb_version: u16::from_le_bytes([data[2], data[3]]),
        class: data[4],
        subclass: data[5],
        protocol: data[6],
        max_packet_size_0: data[7],
    }
}

struct Dwc2Device {
    address: u8,
    regs: Dwc2Registers,
    kernel: Kernel,
    port_speed: Speed,
    channel_pool: HostChannelPool,
    stats: Dwc2Stats,
    desc: Option<DeviceDescriptor>,
    ctrl_ep: EndpointHandle,
    config_desc: Vec<ConfigurationDescriptor>,
    current_config_value: Option<u8>,
    eps: BTreeMap<u8, EndpointHandle>,
    ep_interfaces: BTreeMap<u8, u8>,
}

struct Dwc2DeviceParams {
    address: u8,
    regs: Dwc2Registers,
    kernel: Kernel,
    port_speed: Speed,
    channel_pool: HostChannelPool,
    stats: Dwc2Stats,
}

#[derive(Clone, Copy)]
enum Dwc2QuiesceReason {
    Reconfigure,
    Disconnect,
}

unsafe impl Send for Dwc2Device {}

impl Dwc2Device {
    fn new(params: Dwc2DeviceParams) -> Result<Self> {
        let Dwc2DeviceParams {
            address,
            regs,
            kernel,
            port_speed,
            channel_pool,
            stats,
        } = params;
        let raw = Dwc2Endpoint::new(Dwc2EndpointParams {
            regs,
            kernel: kernel.clone(),
            device_address: 0,
            port_speed,
            info: EndpointInfo::control(),
            channel_pool: channel_pool.clone(),
            stats: stats.clone(),
        })?;
        Ok(Self {
            address,
            regs,
            kernel,
            port_speed,
            channel_pool,
            stats,
            desc: None,
            ctrl_ep: EndpointHandle::new(EndpointInfo::control(), raw),
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
            .with_raw_mut::<Dwc2Endpoint, _>(|ep| ep.set_device_address(self.address));
        self.ctrl_ep
            .with_raw_mut::<Dwc2Endpoint, _>(|ep| ep.set_max_packet_size(base.max_packet_size_0));
        self.kernel.delay(Duration::from_millis(10));

        let desc = self.ctrl_ep.get_device_descriptor().await?;
        self.current_config_value = Some(self.ctrl_ep.get_configuration().await?);
        for index in 0..desc.num_configurations {
            let config = self.ctrl_ep.get_configuration_descriptor(index).await?;
            self.config_desc.push(config);
        }
        self.desc = Some(desc);
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
        Ok(device_descriptor_base_from_bytes(data))
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
        let old_endpoints = self.eps.values().cloned().collect::<Vec<_>>();
        for endpoint in &old_endpoints {
            endpoint.revoke();
        }
        if Self::quiesce_endpoints(old_endpoints.iter(), Dwc2QuiesceReason::Reconfigure)
            .await
            .is_err()
        {
            return Err(USBError::InterfaceBroken);
        }
        if let Err(err) = self.ctrl_ep.set_configuration(configuration_value).await {
            for endpoint in &old_endpoints {
                endpoint.reactivate();
            }
            return Err(err.into());
        }
        self.current_config_value = Some(configuration_value);
        self.eps.clear();
        self.ep_interfaces.clear();
        Ok(())
    }

    async fn claim_interface_inner(
        &mut self,
        interface: u8,
        alternate: u8,
    ) -> Result<BTreeMap<u8, EndpointHandle>> {
        let pending_endpoints = self.prepare_interface_endpoints(interface, alternate)?;
        let stale_addresses = self
            .ep_interfaces
            .iter()
            .filter_map(|(address, owner)| (*owner == interface).then_some(*address))
            .collect::<Vec<_>>();
        let old_endpoints = stale_addresses
            .iter()
            .filter_map(|address| self.eps.get(address).cloned())
            .collect::<Vec<_>>();
        for endpoint in &old_endpoints {
            endpoint.revoke();
        }
        if Self::quiesce_endpoints(old_endpoints.iter(), Dwc2QuiesceReason::Reconfigure)
            .await
            .is_err()
        {
            return Err(USBError::InterfaceBroken);
        }

        if let Err(err) = self
            .ctrl_ep
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
            .await
        {
            for endpoint in &old_endpoints {
                endpoint.reactivate();
            }
            return Err(err.into());
        }
        for address in stale_addresses {
            self.eps.remove(&address);
            self.ep_interfaces.remove(&address);
        }
        for (address, endpoint) in &pending_endpoints {
            self.eps.insert(*address, endpoint.clone());
            self.ep_interfaces.insert(*address, interface);
        }
        Ok(pending_endpoints)
    }

    async fn release_interface_inner(&mut self, interface: u8) -> Result<()> {
        let stale_addresses = self
            .ep_interfaces
            .iter()
            .filter_map(|(address, owner)| (*owner == interface).then_some(*address))
            .collect::<Vec<_>>();
        let old_endpoints = stale_addresses
            .iter()
            .filter_map(|address| self.eps.get(address).cloned())
            .collect::<Vec<_>>();
        for endpoint in &old_endpoints {
            endpoint.revoke();
        }
        if Self::quiesce_endpoints(old_endpoints.iter(), Dwc2QuiesceReason::Reconfigure)
            .await
            .is_err()
        {
            return Err(USBError::InterfaceBroken);
        }
        for address in stale_addresses {
            self.eps.remove(&address);
            self.ep_interfaces.remove(&address);
        }
        Ok(())
    }

    async fn disconnect_inner(&mut self) -> Result<()> {
        let mut endpoints = self.eps.values().cloned().collect::<Vec<_>>();
        endpoints.push(self.ctrl_ep.clone());
        for endpoint in &endpoints {
            endpoint.revoke();
        }
        if Self::quiesce_endpoints(endpoints.iter(), Dwc2QuiesceReason::Disconnect)
            .await
            .is_err()
        {
            return Err(USBError::InterfaceBroken);
        }
        self.eps.clear();
        self.ep_interfaces.clear();
        Ok(())
    }

    async fn quiesce_endpoints<'a>(
        endpoints: impl Iterator<Item = &'a EndpointHandle>,
        reason: Dwc2QuiesceReason,
    ) -> Result<()> {
        for endpoint in endpoints {
            let request_id =
                endpoint.with_raw_mut::<Dwc2Endpoint, _>(|raw| raw.in_flight_request_id());
            let Some(request_id) = request_id else {
                continue;
            };
            endpoint
                .with_raw_mut::<Dwc2Endpoint, _>(|raw| raw.cancel_request(request_id))
                .map_err(USBError::from)?;
            poll_fn(|cx| {
                endpoint.with_raw_mut::<Dwc2Endpoint, _>(|raw| {
                    if let Some(result) = raw.reclaim_request(request_id) {
                        return Poll::Ready(match result {
                            Ok(_) | Err(TransferError::Cancelled) => Ok(()),
                            Err(TransferError::Disconnected)
                                if matches!(reason, Dwc2QuiesceReason::Disconnect) =>
                            {
                                Ok(())
                            }
                            Err(err) => Err(USBError::from(err)),
                        });
                    }
                    raw.register_waker(request_id, cx);
                    Poll::Pending
                })
            })
            .await?;
        }
        Ok(())
    }

    fn prepare_interface_endpoints(
        &self,
        interface: u8,
        alternate: u8,
    ) -> Result<BTreeMap<u8, EndpointHandle>> {
        let endpoints = self
            .find_interface_endpoints(interface, alternate)?
            .to_vec();
        let mut prepared = BTreeMap::new();
        for desc in endpoints {
            let info = EndpointInfo::from(&desc);
            let raw = Dwc2Endpoint::new(Dwc2EndpointParams {
                regs: self.regs,
                kernel: self.kernel.clone(),
                device_address: self.address,
                port_speed: self.port_speed,
                info,
                channel_pool: self.channel_pool.clone(),
                stats: self.stats.clone(),
            })?;
            prepared.insert(desc.address, EndpointHandle::new(info, raw));
        }
        Ok(prepared)
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

impl DeviceOp for Dwc2Device {
    fn id(&self) -> usize {
        self.address as usize
    }

    fn backend_name(&self) -> &str {
        "dwc2"
    }

    fn descriptor(&self) -> &DeviceDescriptor {
        self.desc
            .as_ref()
            .expect("DWC2 device descriptor must be initialized before device publication")
    }

    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor] {
        &self.config_desc
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
    ) -> BoxFuture<'a, Result<BTreeMap<u8, EndpointHandle>>> {
        self.claim_interface_inner(interface, alternate).boxed()
    }

    fn release_interface<'a>(&'a mut self, interface: u8) -> BoxFuture<'a, Result<()>> {
        self.release_interface_inner(interface).boxed()
    }

    fn set_configuration<'a>(&'a mut self, configuration_value: u8) -> BoxFuture<'a, Result<()>> {
        self.set_configuration_inner(configuration_value).boxed()
    }

    fn disconnect(&mut self) -> BoxFuture<'_, Result<()>> {
        self.disconnect_inner().boxed()
    }

    fn update_hub(&mut self, _params: HubParams) -> BoxFuture<'_, Result<()>> {
        async { Ok(()) }.boxed()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn hctsiz_ddma_encodes_pid_ntd_schinfo() {
        assert_eq!(hctsiz_ddma(Dwc2Pid::Data0, 1, 0), 0);
        assert_eq!(hctsiz_ddma(Dwc2Pid::Setup, 1, 0), 3 << 29);
        assert_eq!(
            hctsiz_ddma(Dwc2Pid::Data1, 2, 0x55),
            (2 << 29) | (1 << 8) | 0x55
        );
        // NTD 8 位截断。
        assert_eq!(hctsiz_ddma(Dwc2Pid::Data0, 256, 0), 255 << 8);
        // ISO MC 编码：DATA2=1、MDATA=3。
        assert_eq!(
            hctsiz_ddma(Dwc2Pid::Data2, 256, 0xff),
            (1 << 29) | (255 << 8) | 0xff
        );
        assert_eq!(hctsiz_ddma(Dwc2Pid::MData, 1, 0), 3 << 29);
    }

    #[test]
    fn hcint_fault_maps_nak_stall_xact_and_bus_errors() {
        assert_eq!(hcint_fault(HCINT_STALL), Some(Dwc2TransferFault::Stall));
        assert_eq!(hcint_fault(HCINT_NAK), Some(Dwc2TransferFault::Nak));
        assert_eq!(hcint_fault(HCINT_XACTERR), Some(Dwc2TransferFault::Xact));
        assert_eq!(hcint_fault(HCINT_AHBERR), Some(Dwc2TransferFault::Ahb));
        assert_eq!(hcint_fault(HCINT_BBLERR), Some(Dwc2TransferFault::Babble));
        assert_eq!(
            hcint_fault(HCINT_FRMOVRN),
            Some(Dwc2TransferFault::FrameOverrun)
        );
        assert_eq!(
            hcint_fault(HCINT_DATATGLERR),
            Some(Dwc2TransferFault::DataToggle)
        );
        // CHHLTD（± XFERCOMPL）是正常通道终止，不是故障。
        assert_eq!(hcint_fault(HCINT_CHHLTD), None);
        assert_eq!(hcint_fault(HCINT_CHHLTD | HCINT_XFERCOMPL), None);
        // 非完成位（如 BNA）没有 CHHLTD/XFERCOMPL → 无完成暂停。
        assert_eq!(
            hcint_fault(1 << 11),
            Some(Dwc2TransferFault::HaltedWithoutComplete)
        );
        assert_eq!(
            hcint_fault(0),
            Some(Dwc2TransferFault::HaltedWithoutComplete)
        );
    }
}
