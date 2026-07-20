use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec, vec::Vec};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    task::Context,
    time::Duration,
};

use ax_kspin::SpinRaw as Mutex;
use dma_api::CoherentArray;
use futures::{FutureExt, future::BoxFuture, task::AtomicWaker};
use mbarrier::mb;
use rdif_irq::{ContainmentCause, FaultContainment, IrqCapture, MaskedSource};
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
    backend::ty::{
        DeviceOp, Event, EventHandlerOp, HubParams, IrqEpoch, UsbIrqEvent, UsbIrqFault,
        ep::Endpoint,
    },
    err::Result,
};

const DWC2_DMA_MASK_32: u64 = u32::MAX as u64;
const DWC2_WAIT_ITERS: usize = 3_000_000;
const DWC2_MAX_CHANNELS: u8 = 16;
const DWC2_DMA_ALIGN: usize = 64;
const DWC2_NAK_RETRIES: u32 = 64;
const DWC2_XACT_RETRIES: u32 = 8;

const GOTGCTL: usize = 0x000;
const GAHBCFG: usize = 0x008;
const GUSBCFG: usize = 0x00c;
const GRSTCTL: usize = 0x010;
const GINTSTS: usize = 0x014;
const GINTMSK: usize = 0x018;
const GRXFSIZ: usize = 0x024;
const GNPTXFSIZ: usize = 0x028;
const GHWCFG2: usize = 0x048;
const GHWCFG4: usize = 0x050;
const HPTXFSIZ: usize = 0x100;
const HCFG: usize = 0x400;
const HFNUM: usize = 0x408;
const HAINT: usize = 0x414;
const HAINTMSK: usize = 0x418;
const HPRT0: usize = 0x440;
const HC_BASE: usize = 0x500;
const HC_STRIDE: usize = 0x20;
const PCGCTL: usize = 0xe00;

const HCCHAR: usize = 0x00;
const HCSPLT: usize = 0x04;
const HCINT: usize = 0x08;
const HCINTMSK: usize = 0x0c;
const HCTSIZ: usize = 0x10;
const HCDMA: usize = 0x14;

const GUSBCFG_TOUTCAL_MASK: u32 = 0x7;
const GUSBCFG_PHYIF16: u32 = 1 << 3;
const GUSBCFG_ULPI_UTMI_SEL: u32 = 1 << 4;
const GUSBCFG_FORCEHOSTMODE: u32 = 1 << 29;
const GUSBCFG_FORCEDEVMODE: u32 = 1 << 30;

const GRSTCTL_CSFTRST: u32 = 1 << 0;
const GRSTCTL_RXFFLSH: u32 = 1 << 4;
const GRSTCTL_TXFFLSH: u32 = 1 << 5;
const GRSTCTL_TXFNUM_ALL: u32 = 0x10 << 6;
const GRSTCTL_CSFTRST_DONE: u32 = 1 << 29;
const GRSTCTL_AHBIDLE: u32 = 1 << 31;

const GINTSTS_CURMODE_HOST: u32 = 1 << 0;
const GINTSTS_PRTINT: u32 = 1 << 24;
const GINTSTS_HCHINT: u32 = 1 << 25;
const GINTSTS_DISCONNINT: u32 = 1 << 29;
const DWC2_RUNTIME_GINTMSK: u32 = GINTSTS_PRTINT | GINTSTS_HCHINT | GINTSTS_DISCONNINT;

const GOTGCTL_VBVALOEN: u32 = 1 << 2;
const GOTGCTL_VBVALOVAL: u32 = 1 << 3;
const GOTGCTL_AVALOEN: u32 = 1 << 4;
const GOTGCTL_AVALOVAL: u32 = 1 << 5;
const GOTGCTL_DBNCE_FLTR_BYPASS: u32 = 1 << 15;

const HPRT_CONN_STS: u32 = 1 << 0;
const HPRT_CONN_DET: u32 = 1 << 1;
const HPRT_ENA: u32 = 1 << 2;
const HPRT_ENA_CHG: u32 = 1 << 3;
const HPRT_OVRCUR_CHG: u32 = 1 << 5;
const HPRT_RST: u32 = 1 << 8;
const HPRT_PWR: u32 = 1 << 12;
const HPRT_SPD_SHIFT: u32 = 17;
const HPRT_SPD_MASK: u32 = 0b11 << HPRT_SPD_SHIFT;
const HPRT_W1C_MASK: u32 = HPRT_CONN_DET | HPRT_ENA | HPRT_ENA_CHG | HPRT_OVRCUR_CHG;

const HCCHAR_CHENA: u32 = 1 << 31;
const HCCHAR_CHDIS: u32 = 1 << 30;
const HCCHAR_ODDFRM: u32 = 1 << 29;
const HCCHAR_EPDIR: u32 = 1 << 15;
const HCINT_XFERCOMPL: u32 = 1 << 0;
const HCINT_CHHLTD: u32 = 1 << 1;
const HCINT_AHBERR: u32 = 1 << 2;
const HCINT_STALL: u32 = 1 << 3;
const HCINT_NAK: u32 = 1 << 4;
const HCINT_XACTERR: u32 = 1 << 7;
const HCINT_BBLERR: u32 = 1 << 8;
const HCINT_FRMOVRN: u32 = 1 << 9;
const HCINT_DATATGLERR: u32 = 1 << 10;
const HCINT_ALL_W1C: u32 = 0x7ff;
const HCINT_TRANSFER_MASK: u32 = HCINT_XFERCOMPL
    | HCINT_CHHLTD
    | HCINT_AHBERR
    | HCINT_STALL
    | HCINT_NAK
    | HCINT_XACTERR
    | HCINT_BBLERR
    | HCINT_FRMOVRN
    | HCINT_DATATGLERR;
const HCINT_DMA_IRQ_MASK: u32 = HCINT_CHHLTD
    | HCINT_AHBERR
    | HCINT_STALL
    | HCINT_XACTERR
    | HCINT_BBLERR
    | HCINT_FRMOVRN
    | HCINT_DATATGLERR;

const HCTSIZ_XFERSIZE_MASK: u32 = 0x7ffff;
const HCTSIZ_MAX_PACKETS: u32 = 1023;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dwc2UtmiWidth {
    Eight,
    Sixteen,
    Auto,
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
    pub quirks: Dwc2Quirks,
}

impl Dwc2HostParams {
    pub const fn sg2002() -> Self {
        Self {
            dma_mask: DWC2_DMA_MASK_32,
            fifo: Dwc2FifoSizes::sg2002_default(),
            utmi: Dwc2UtmiWidth::Auto,
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

#[derive(Clone, Copy)]
struct Dwc2Registers {
    base: NonNull<u8>,
}

// SAFETY: `Dwc2Registers` is a copyable handle to a DWC2 MMIO mapping that is
// owned by the platform USB host for the whole backend lifetime. The type does
// not provide references into the mapping; all accesses are volatile 32-bit
// loads/stores through private methods, and higher-level code serializes
// channel programming with per-channel gates when concurrent task/IRQ paths can
// touch the same channel registers.
unsafe impl Send for Dwc2Registers {}
// SAFETY: Shared references to this handle cannot create Rust aliasing to the
// MMIO region because MMIO is only accessed by volatile operations. Register
// ordering and channel-level races are handled by the DWC2 protocol code and
// the per-channel gates/completion atomics rather than by Rust references.
unsafe impl Sync for Dwc2Registers {}

impl Dwc2Registers {
    fn new(base: Mmio) -> Self {
        Self { base }
    }

    fn read32(self, offset: usize) -> u32 {
        // SAFETY: `base` points at the live DWC2 register window supplied by the
        // board glue. `Dwc2Registers` is private, and every caller passes either
        // a constant register offset from this file or a channel offset computed
        // from a bounded host-channel index. Volatile access is required for
        // MMIO and does not create Rust references into the device memory.
        unsafe { (self.base.as_ptr().add(offset) as *const u32).read_volatile() }
    }

    fn write32(self, offset: usize, value: u32) {
        // SAFETY: Same mapping and offset invariants as `read32`. The write is
        // volatile because it programs hardware state; callers preserve W1C and
        // read-modify-write semantics at the register-specific helper layer.
        unsafe { (self.base.as_ptr().add(offset) as *mut u32).write_volatile(value) }
    }

    fn update32(self, offset: usize, f: impl FnOnce(u32) -> u32) {
        let value = self.read32(offset);
        self.write32(offset, f(value));
    }

    fn channel_offset(channel: u8, reg: usize) -> usize {
        HC_BASE + usize::from(channel) * HC_STRIDE + reg
    }

    fn channel_read32(self, channel: u8, reg: usize) -> u32 {
        self.read32(Self::channel_offset(channel, reg))
    }

    fn channel_write32(self, channel: u8, reg: usize, value: u32) {
        self.write32(Self::channel_offset(channel, reg), value);
    }

    fn host_channel_count(self) -> u8 {
        ((((self.read32(GHWCFG2) >> 14) & 0x0f) + 1) as u8).clamp(2, DWC2_MAX_CHANNELS)
    }

    fn hprt_status(self) -> Dwc2PortStatus {
        Dwc2PortStatus::from_raw(self.read32(HPRT0))
    }

    fn hprt_write_safe(self, value: u32) {
        self.write32(HPRT0, Dwc2PortStatus::from_raw(value).rmw_preserving_w1c());
    }

    fn hprt_update_safe(self, f: impl FnOnce(u32) -> u32) {
        let value = self.read32(HPRT0) & !HPRT_W1C_MASK;
        self.write32(HPRT0, f(value) & !HPRT_W1C_MASK);
    }

    fn clear_hprt_connect_detect(self) {
        let current = self.read32(HPRT0);
        if current & HPRT_CONN_DET != 0 {
            self.write32(HPRT0, (current & !HPRT_W1C_MASK) | HPRT_CONN_DET);
        }
    }

    fn periodic_odd_frame_bit(self) -> u32 {
        if self.read32(HFNUM) & 1 == 0 {
            HCCHAR_ODDFRM
        } else {
            0
        }
    }
}

struct Dwc2ChannelCompletionSlot {
    hcint: AtomicU32,
    deferred_hcint: AtomicU32,
    busy: AtomicBool,
    waker: AtomicWaker,
}

impl Dwc2ChannelCompletionSlot {
    fn new() -> Self {
        Self {
            hcint: AtomicU32::new(0),
            deferred_hcint: AtomicU32::new(0),
            busy: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        }
    }

    fn try_begin_request(&self) -> bool {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn end_request(&self) {
        self.busy.store(false, Ordering::Release);
        self.clear();
    }

    fn clear(&self) {
        self.hcint.store(0, Ordering::Release);
        self.deferred_hcint.store(0, Ordering::Release);
    }

    fn publish(&self, hcint: u32) {
        let deferred = self.deferred_hcint.swap(0, Ordering::AcqRel);
        self.hcint.fetch_or(hcint | deferred, Ordering::AcqRel);
        self.waker.wake();
    }

    fn defer(&self, hcint: u32) {
        self.deferred_hcint.fetch_or(hcint, Ordering::AcqRel);
    }

    fn take(&self) -> Option<u32> {
        let hcint = self.hcint.swap(0, Ordering::AcqRel);
        (hcint != 0).then_some(hcint)
    }

    fn register_waker(&self, cx: &mut Context<'_>) {
        self.waker.register(cx.waker());
    }
}

#[derive(Clone)]
struct Dwc2ChannelCompletions {
    slots: Arc<Vec<Dwc2ChannelCompletionSlot>>,
}

impl Dwc2ChannelCompletions {
    fn new() -> Self {
        Self {
            slots: Arc::new(
                (0..usize::from(DWC2_MAX_CHANNELS))
                    .map(|_| Dwc2ChannelCompletionSlot::new())
                    .collect(),
            ),
        }
    }

    fn slot(&self, channel: u8) -> &Dwc2ChannelCompletionSlot {
        self.slots
            .get(usize::from(channel))
            .unwrap_or_else(|| &self.slots[0])
    }

    fn try_begin_request(&self, channel: u8) -> bool {
        self.slot(channel).try_begin_request()
    }

    fn end_request(&self, channel: u8) {
        self.slot(channel).end_request();
    }

    fn clear(&self, channel: u8) {
        self.slot(channel).clear();
    }

    fn publish(&self, channel: u8, hcint: u32) {
        self.slot(channel).publish(hcint);
    }

    fn defer(&self, channel: u8, hcint: u32) {
        self.slot(channel).defer(hcint);
    }

    fn take(&self, channel: u8) -> Option<u32> {
        self.slot(channel).take()
    }

    fn register_waker(&self, channel: u8, cx: &mut Context<'_>) {
        self.slot(channel).register_waker(cx);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Dwc2TransferStats {
    pub transfers: usize,
    pub stages: usize,
    pub dma_allocs: usize,
    pub bounce_to_device_bytes: usize,
    pub bounce_from_device_bytes: usize,
    pub naks: usize,
    pub xact_errors: usize,
    pub timeouts: usize,
    pub wait_iters: usize,
    pub init_wait_iters: usize,
    pub transfer_busy_wait_iters: usize,
    pub irq_events: usize,
    pub channel_completions: usize,
}

#[derive(Default)]
struct Dwc2StatsInner {
    transfers: AtomicUsize,
    stages: AtomicUsize,
    dma_allocs: AtomicUsize,
    bounce_to_device_bytes: AtomicUsize,
    bounce_from_device_bytes: AtomicUsize,
    naks: AtomicUsize,
    xact_errors: AtomicUsize,
    timeouts: AtomicUsize,
    init_wait_iters: AtomicUsize,
    transfer_busy_wait_iters: AtomicUsize,
    irq_events: AtomicUsize,
    channel_completions: AtomicUsize,
}

#[derive(Clone, Default)]
struct Dwc2Stats {
    inner: Arc<Dwc2StatsInner>,
}

impl Dwc2Stats {
    fn new() -> Self {
        Self::default()
    }

    fn reset(&self) {
        self.inner.transfers.store(0, Ordering::Relaxed);
        self.inner.stages.store(0, Ordering::Relaxed);
        self.inner.dma_allocs.store(0, Ordering::Relaxed);
        self.inner
            .bounce_to_device_bytes
            .store(0, Ordering::Relaxed);
        self.inner
            .bounce_from_device_bytes
            .store(0, Ordering::Relaxed);
        self.inner.naks.store(0, Ordering::Relaxed);
        self.inner.xact_errors.store(0, Ordering::Relaxed);
        self.inner.timeouts.store(0, Ordering::Relaxed);
        self.inner.init_wait_iters.store(0, Ordering::Relaxed);
        self.inner
            .transfer_busy_wait_iters
            .store(0, Ordering::Relaxed);
        self.inner.irq_events.store(0, Ordering::Relaxed);
        self.inner.channel_completions.store(0, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Dwc2TransferStats {
        let init_wait_iters = self.inner.init_wait_iters.load(Ordering::Relaxed);
        let transfer_busy_wait_iters = self.inner.transfer_busy_wait_iters.load(Ordering::Relaxed);
        Dwc2TransferStats {
            transfers: self.inner.transfers.load(Ordering::Relaxed),
            stages: self.inner.stages.load(Ordering::Relaxed),
            dma_allocs: self.inner.dma_allocs.load(Ordering::Relaxed),
            bounce_to_device_bytes: self.inner.bounce_to_device_bytes.load(Ordering::Relaxed),
            bounce_from_device_bytes: self.inner.bounce_from_device_bytes.load(Ordering::Relaxed),
            naks: self.inner.naks.load(Ordering::Relaxed),
            xact_errors: self.inner.xact_errors.load(Ordering::Relaxed),
            timeouts: self.inner.timeouts.load(Ordering::Relaxed),
            wait_iters: init_wait_iters + transfer_busy_wait_iters,
            init_wait_iters,
            transfer_busy_wait_iters,
            irq_events: self.inner.irq_events.load(Ordering::Relaxed),
            channel_completions: self.inner.channel_completions.load(Ordering::Relaxed),
        }
    }

    fn record_transfer(&self) {
        self.inner.transfers.fetch_add(1, Ordering::Relaxed);
    }

    fn record_stage(&self) {
        self.inner.stages.fetch_add(1, Ordering::Relaxed);
    }

    fn record_dma_alloc(&self) {
        self.inner.dma_allocs.fetch_add(1, Ordering::Relaxed);
    }

    fn record_bounce_to_device(&self, bytes: usize) {
        self.inner
            .bounce_to_device_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }

    fn record_bounce_from_device(&self, bytes: usize) {
        self.inner
            .bounce_from_device_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }

    fn record_fault(&self, fault: Dwc2TransferFault) {
        match fault {
            Dwc2TransferFault::Nak => {
                self.inner.naks.fetch_add(1, Ordering::Relaxed);
            }
            Dwc2TransferFault::Xact => {
                self.inner.xact_errors.fetch_add(1, Ordering::Relaxed);
            }
            Dwc2TransferFault::Stall
            | Dwc2TransferFault::Ahb
            | Dwc2TransferFault::Babble
            | Dwc2TransferFault::FrameOverrun
            | Dwc2TransferFault::DataToggle
            | Dwc2TransferFault::HaltedWithoutComplete => {}
        }
    }

    fn record_timeout(&self) {
        self.inner.timeouts.fetch_add(1, Ordering::Relaxed);
    }

    fn record_init_wait_iters(&self, iters: usize) {
        self.inner
            .init_wait_iters
            .fetch_add(iters, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn record_transfer_busy_wait_iters(&self, iters: usize) {
        self.inner
            .transfer_busy_wait_iters
            .fetch_add(iters, Ordering::Relaxed);
    }

    fn record_irq_event(&self) {
        self.inner.irq_events.fetch_add(1, Ordering::Relaxed);
    }

    fn record_channel_completion(&self) {
        self.inner
            .channel_completions
            .fetch_add(1, Ordering::Relaxed);
    }
}

fn build_channel_gates(channel_count: u8) -> Vec<Arc<Mutex<()>>> {
    (0..usize::from(channel_count.max(1)))
        .map(|_| Arc::new(Mutex::new(())))
        .collect()
}

fn channel_gate(gates: &[Arc<Mutex<()>>], channel: u8) -> Arc<Mutex<()>> {
    gates
        .get(usize::from(channel))
        .or_else(|| gates.first())
        .expect("DWC2 channel gates must not be empty")
        .clone()
}

pub struct Dwc2 {
    regs: Dwc2Registers,
    kernel: Kernel,
    params: Dwc2HostParams,
    root_hub: Option<Dwc2RootHub>,
    event_handler: Option<Dwc2EventHandler>,
    next_addr: u8,
    channel_count: u8,
    channel_gates: Vec<Arc<Mutex<()>>>,
    channel_completions: Dwc2ChannelCompletions,
    stats: Dwc2Stats,
}

// SAFETY: `Dwc2` is moved into the kmod core as the unique owner of host state.
// Public core operations require `&mut self`; the separately exposed IRQ handler
// owns only cloned register/completion/stat handles. Shared state between the
// task path and IRQ path is restricted to atomics, wakers, and per-channel gates.
unsafe impl Send for Dwc2 {}
// SAFETY: The only `&self` operations exposed by `Dwc2` read/reset atomic
// statistics or return immutable kernel references. MMIO mutation remains behind
// `&mut self` core methods or the IRQ handler, whose synchronization invariants
// are documented on `Dwc2Registers` and `Dwc2EventHandler`.
unsafe impl Sync for Dwc2 {}

impl Dwc2 {
    pub fn new(params: Dwc2NewParams) -> Result<Self> {
        if params.params.dma_mask != DWC2_DMA_MASK_32 {
            return Err(USBError::NotSupported);
        }

        let regs = Dwc2Registers::new(params.mmio);
        let kernel = Kernel::new(params.params.dma_mask, params.kernel);
        let root_hub = Dwc2RootHub::new(regs, kernel.clone());
        let channel_completions = Dwc2ChannelCompletions::new();
        let stats = Dwc2Stats::new();
        let event_handler = Dwc2EventHandler::new(regs, channel_completions.clone(), stats.clone());
        let channel_count = regs.host_channel_count();

        Ok(Self {
            regs,
            kernel,
            params: params.params,
            root_hub: Some(root_hub),
            event_handler: Some(event_handler),
            next_addr: 1,
            channel_count,
            channel_gates: build_channel_gates(channel_count),
            channel_completions,
            stats,
        })
    }

    async fn init_controller(&mut self) -> Result<()> {
        self.disable_irq()?;
        self.regs.write32(GINTSTS, u32::MAX);
        self.core_soft_reset()?;
        self.force_host_mode()?;
        self.core_soft_reset()?;

        if self.params.quirks.otg_host_session_override {
            self.regs.update32(GOTGCTL, |value| {
                value
                    | GOTGCTL_DBNCE_FLTR_BYPASS
                    | GOTGCTL_AVALOEN
                    | GOTGCTL_AVALOVAL
                    | GOTGCTL_VBVALOEN
                    | GOTGCTL_VBVALOVAL
            });
            self.kernel.delay(Duration::from_micros(200));
        }

        self.init_gusbcfg();
        self.regs.write32(PCGCTL, 0);

        let arch = (self.regs.read32(GHWCFG2) >> 3) & 0b11;
        let gahbcfg = build_gahbcfg_internal_dma(arch)?;
        self.regs.write32(GAHBCFG, gahbcfg);

        self.regs.update32(HCFG, |value| value & !0x7);

        let fifo = fifo_register_plan(self.params.fifo);
        self.regs.write32(GRXFSIZ, fifo.grxfsiz);
        self.regs.write32(GNPTXFSIZ, fifo.gnptxfsiz);
        self.regs.write32(HPTXFSIZ, fifo.hptxfsiz);
        self.flush_tx_fifo_all()?;
        self.flush_rx_fifo()?;

        self.channel_count = self.regs.host_channel_count();
        if self.channel_gates.len() != usize::from(self.channel_count) {
            self.channel_gates = build_channel_gates(self.channel_count);
        }
        let channel_mask = if self.channel_count >= 16 {
            u16::MAX as u32
        } else {
            (1u32 << self.channel_count) - 1
        };
        self.regs.write32(HAINTMSK, channel_mask);
        self.regs.write32(GINTMSK, DWC2_RUNTIME_GINTMSK);
        self.regs.write32(GINTSTS, u32::MAX);
        self.port_power_on();
        self.kernel.delay(Duration::from_millis(20));
        Ok(())
    }

    fn init_gusbcfg(&self) {
        let want_16bit = match self.params.utmi {
            Dwc2UtmiWidth::Eight => false,
            Dwc2UtmiWidth::Sixteen => true,
            Dwc2UtmiWidth::Auto => ((self.regs.read32(GHWCFG4) >> 14) & 0b11) == 1,
        };

        self.regs.update32(GUSBCFG, |mut value| {
            value &= !(GUSBCFG_TOUTCAL_MASK
                | GUSBCFG_PHYIF16
                | GUSBCFG_ULPI_UTMI_SEL
                | GUSBCFG_FORCEDEVMODE);
            value |= GUSBCFG_FORCEHOSTMODE | 0x7;
            if want_16bit {
                value |= GUSBCFG_PHYIF16;
            }
            value
        });
    }

    fn wait_until(&self, ready: impl Fn() -> bool) -> Result<()> {
        for iter in 0..DWC2_WAIT_ITERS {
            if ready() {
                self.stats.record_init_wait_iters(iter + 1);
                return Ok(());
            }
            core::hint::spin_loop();
        }
        self.stats.record_init_wait_iters(DWC2_WAIT_ITERS);
        self.stats.record_timeout();
        Err(USBError::Timeout)
    }

    fn wait_ahb_idle(&self) -> Result<()> {
        self.wait_until(|| self.regs.read32(GRSTCTL) & GRSTCTL_AHBIDLE != 0)
    }

    fn core_soft_reset(&self) -> Result<()> {
        self.wait_ahb_idle()?;
        self.regs.update32(GRSTCTL, |value| value | GRSTCTL_CSFTRST);
        self.wait_until(|| {
            let value = self.regs.read32(GRSTCTL);
            value & GRSTCTL_CSFTRST == 0 || value & GRSTCTL_CSFTRST_DONE != 0
        })?;
        if self.regs.read32(GRSTCTL) & GRSTCTL_CSFTRST_DONE != 0 {
            self.regs.update32(GRSTCTL, |value| {
                (value & !GRSTCTL_CSFTRST) | GRSTCTL_CSFTRST_DONE
            });
        }
        self.kernel.delay(Duration::from_millis(1));
        Ok(())
    }

    fn force_host_mode(&self) -> Result<()> {
        self.regs.update32(GUSBCFG, |value| {
            (value | GUSBCFG_FORCEHOSTMODE) & !GUSBCFG_FORCEDEVMODE
        });
        self.kernel.delay(Duration::from_millis(25));
        self.wait_until(|| self.regs.read32(GINTSTS) & GINTSTS_CURMODE_HOST != 0)
    }

    fn flush_tx_fifo_all(&self) -> Result<()> {
        self.regs
            .write32(GRSTCTL, GRSTCTL_TXFFLSH | GRSTCTL_TXFNUM_ALL);
        self.wait_until(|| self.regs.read32(GRSTCTL) & GRSTCTL_TXFFLSH == 0)
    }

    fn flush_rx_fifo(&self) -> Result<()> {
        self.regs.write32(GRSTCTL, GRSTCTL_RXFFLSH);
        self.wait_until(|| self.regs.read32(GRSTCTL) & GRSTCTL_RXFFLSH == 0)
    }

    fn port_power_on(&self) {
        self.regs.hprt_update_safe(|value| value | HPRT_PWR);
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
        let addr = self.allocate_address()?;
        let mut device = Dwc2Device::new(Dwc2DeviceParams {
            address: addr,
            regs: self.regs,
            kernel: self.kernel.clone(),
            port_speed: info.port_speed,
            channel_count: self.channel_count,
            channel_gates: self.channel_gates.clone(),
            channel_completions: self.channel_completions.clone(),
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
        self.regs.write32(GINTMSK, DWC2_RUNTIME_GINTMSK);
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<()> {
        self.regs.write32(GINTMSK, 0);
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

struct Dwc2RootHub {
    regs: Dwc2Registers,
    kernel: Kernel,
    port: PortState,
    last_logged_hprt: Option<u32>,
}

// SAFETY: The root hub is driven by `HubOp` methods taking `&mut self`, so its
// port state and cached log value are not concurrently mutated. MMIO access uses
// the `Dwc2Registers` volatile-access invariants.
unsafe impl Send for Dwc2RootHub {}

impl Dwc2RootHub {
    fn new(regs: Dwc2Registers, kernel: Kernel) -> Self {
        Self {
            regs,
            kernel,
            port: PortState::Uninit,
            last_logged_hprt: None,
        }
    }

    async fn init_port(&mut self, mut info: HubInfo) -> Result<HubInfo> {
        info.speed = Speed::High;
        self.regs.hprt_update_safe(|value| value | HPRT_PWR);
        self.kernel.delay(Duration::from_millis(20));
        Ok(info)
    }

    async fn changed_ports_inner(&mut self) -> Result<Vec<PortChangeInfo>> {
        if matches!(self.port, PortState::Probed) {
            return Ok(Vec::new());
        }

        let mut status = self.regs.hprt_status();
        self.log_port_status("scan", status);
        if !status.connected() {
            return Ok(Vec::new());
        }

        if matches!(self.port, PortState::Uninit) {
            self.reset_port();
            self.port = PortState::Reseted;
            status = self.regs.hprt_status();
            self.log_port_status("reset", status);
        }

        if status.connected() && status.enabled() {
            self.port = PortState::Probed;
            Ok(vec![PortChangeInfo {
                root_port_id: 1,
                port_id: 1,
                port_speed: status.speed(),
                tt_port_on_hub: None,
            }])
        } else {
            Ok(Vec::new())
        }
    }

    fn reset_port(&self) {
        self.regs.clear_hprt_connect_detect();
        self.regs
            .hprt_write_safe((self.regs.read32(HPRT0) & !HPRT_W1C_MASK) | HPRT_PWR | HPRT_RST);
        self.kernel.delay(Duration::from_millis(60));
        self.regs
            .hprt_write_safe(((self.regs.read32(HPRT0) & !HPRT_W1C_MASK) | HPRT_PWR) & !HPRT_RST);
        self.kernel.delay(Duration::from_millis(80));
    }

    fn log_port_status(&mut self, phase: &str, status: Dwc2PortStatus) {
        if self.last_logged_hprt == Some(status.raw()) {
            return;
        }
        self.last_logged_hprt = Some(status.raw());
        log::info!(
            "dwc2: root port {phase} hprt0={:#010x} connected={} enabled={} speed={:?}",
            status.raw(),
            status.connected(),
            status.enabled(),
            status.speed()
        );
    }
}

impl HubOp for Dwc2RootHub {
    fn init<'a>(&'a mut self, info: HubInfo) -> BoxFuture<'a, Result<HubInfo>> {
        self.init_port(info).boxed()
    }

    fn changed_ports<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<PortChangeInfo>>> {
        self.changed_ports_inner().boxed()
    }

    fn slot_id(&self) -> u8 {
        0
    }
}

struct Dwc2EventHandler {
    regs: Dwc2Registers,
    channel_completions: Dwc2ChannelCompletions,
    stats: Dwc2Stats,
    irq_epoch: IrqEpoch,
}

// SAFETY: The event handler may be registered on an IRQ path and moved between
// runtimes. It contains no unsynchronized mutable Rust state: completions and
// statistics are atomic, and register access is volatile through
// `Dwc2Registers`.
unsafe impl Send for Dwc2EventHandler {}
// SAFETY: IRQ capture and owner-side event service use `&self`, acknowledge
// hardware bits, and publish completions through atomics/wakers. Channel
// programming races are bounded by per-channel gates and channel-halt ordering.
unsafe impl Sync for Dwc2EventHandler {}

impl Dwc2EventHandler {
    fn new(
        regs: Dwc2Registers,
        channel_completions: Dwc2ChannelCompletions,
        stats: Dwc2Stats,
    ) -> Self {
        Self {
            regs,
            channel_completions,
            stats,
            irq_epoch: IrqEpoch::new(),
        }
    }
}

impl EventHandlerOp for Dwc2EventHandler {
    fn capture_irq(&self) -> IrqCapture<UsbIrqEvent, UsbIrqFault> {
        let pending = self.regs.read32(GINTSTS)
            & self.regs.read32(GINTMSK)
            & (GINTSTS_PRTINT | GINTSTS_HCHINT | GINTSTS_DISCONNINT);
        if pending == 0 {
            return IrqCapture::Unhandled;
        }

        // Mask the controller source before returning to the generic IRQ flow.
        // Destructive channel-status reads are deferred to the fixed owner.
        self.regs.write32(GINTMSK, 0);
        match self.irq_epoch.capture(u64::from(pending)) {
            Ok((event, masked)) => IrqCapture::Captured {
                event,
                masked: Some(masked),
            },
            Err(reason) => match self.irq_epoch.contained_or_capture(u64::from(pending)) {
                Ok(masked) => IrqCapture::Fault {
                    reason,
                    containment: FaultContainment::DeviceSourceMasked(masked),
                },
                Err(_) => IrqCapture::Fault {
                    reason,
                    containment: FaultContainment::Uncontained,
                },
            },
        }
    }

    fn service_host_events(&self, event: UsbIrqEvent) -> core::result::Result<Event, UsbIrqFault> {
        self.irq_epoch.validate_event(event)?;
        let pending = event.sources() as u32;
        let mut topology_changed = false;
        let mut transfer_count = 0usize;
        if pending & GINTSTS_PRTINT != 0 {
            self.regs.write32(GINTSTS, GINTSTS_PRTINT);
            topology_changed = true;
        }
        if pending & GINTSTS_HCHINT != 0 {
            self.stats.record_irq_event();
            transfer_count = self.handle_channel_interrupts().max(1);
            self.regs.write32(GINTSTS, GINTSTS_HCHINT);
        }
        if pending & GINTSTS_DISCONNINT != 0 {
            self.regs.write32(GINTSTS, GINTSTS_DISCONNINT);
            topology_changed = true;
        }
        if topology_changed {
            Ok(Event::PortChange { port: 1 })
        } else if transfer_count != 0 {
            Ok(Event::TransferActivity {
                count: transfer_count,
            })
        } else {
            Ok(Event::Stopped)
        }
    }

    fn contain(&self, _cause: ContainmentCause) -> core::result::Result<MaskedSource, UsbIrqFault> {
        self.regs.write32(GINTMSK, 0);
        self.irq_epoch
            .contained_or_capture(u64::from(DWC2_RUNTIME_GINTMSK))
    }

    fn rearm_sources(&self, source: MaskedSource) -> core::result::Result<(), UsbIrqFault> {
        self.irq_epoch.finish_rearm(source)?;
        self.regs.write32(GINTMSK, DWC2_RUNTIME_GINTMSK);
        Ok(())
    }
}

impl Dwc2EventHandler {
    fn handle_channel_interrupts(&self) -> usize {
        let pending = self.regs.read32(HAINT) & self.regs.read32(HAINTMSK);
        let mut count = 0usize;
        for channel in 0..DWC2_MAX_CHANNELS {
            if pending & (1u32 << channel) == 0 {
                continue;
            }
            let hcint_raw = self.regs.channel_read32(channel, HCINT) & HCINT_TRANSFER_MASK;
            let hcint_masked = hcint_raw & self.regs.channel_read32(channel, HCINTMSK);
            if hcint_masked == 0 {
                continue;
            }
            self.regs.channel_write32(channel, HCINT, hcint_raw);
            let hcint = if hcint_masked & HCINT_CHHLTD != 0 {
                hcint_raw
            } else {
                hcint_masked
            };
            if hcint & HCINT_CHHLTD == 0 {
                let hcchar = self.regs.channel_read32(channel, HCCHAR);
                if hcchar & HCCHAR_CHENA != 0 {
                    self.channel_completions.defer(channel, hcint);
                    self.regs.channel_write32(
                        channel,
                        HCCHAR,
                        hcchar | HCCHAR_CHENA | HCCHAR_CHDIS,
                    );
                    continue;
                }
            }
            self.channel_completions.publish(channel, hcint);
            self.stats.record_channel_completion();
            count += 1;
        }
        count
    }
}

#[derive(Debug, Clone, Copy)]
struct Dwc2PortStatus(u32);

impl Dwc2PortStatus {
    const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    const fn connected(self) -> bool {
        self.0 & HPRT_CONN_STS != 0
    }

    const fn raw(self) -> u32 {
        self.0
    }

    const fn enabled(self) -> bool {
        self.0 & HPRT_ENA != 0
    }

    fn speed(self) -> Speed {
        match (self.0 & HPRT_SPD_MASK) >> HPRT_SPD_SHIFT {
            0 => Speed::High,
            2 => Speed::Low,
            _ => Speed::Full,
        }
    }

    const fn rmw_preserving_w1c(self) -> u32 {
        self.0 & !HPRT_W1C_MASK
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dwc2Pid {
    Data0,
    Data1,
    Setup,
}

impl Dwc2Pid {
    const fn bits(self) -> u32 {
        match self {
            Self::Data0 => 0,
            Self::Data1 => 2,
            Self::Setup => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dwc2EpType {
    Control,
    Bulk,
    Interrupt,
}

impl Dwc2EpType {
    const fn bits(self) -> u32 {
        match self {
            Self::Control => 0,
            Self::Bulk => 2,
            Self::Interrupt => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DataStageRetryPolicy {
    nak_retries: u32,
}

fn data_stage_retry_policy(ep_type: Dwc2EpType) -> DataStageRetryPolicy {
    match ep_type {
        Dwc2EpType::Control | Dwc2EpType::Bulk | Dwc2EpType::Interrupt => DataStageRetryPolicy {
            nak_retries: DWC2_NAK_RETRIES,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Dwc2TransferStage {
    pub(crate) hcchar: u32,
    pub(crate) hctsiz: u32,
    pub(crate) dma_addr: u32,
    pub(crate) len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dwc2ControlPlan {
    pub(crate) setup: Dwc2TransferStage,
    pub(crate) data: Vec<Dwc2TransferStage>,
    pub(crate) status: Dwc2TransferStage,
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

fn hctsiz(pid: Dwc2Pid, packet_count: u32, len: u32) -> u32 {
    ((pid.bits() & 0b11) << 29)
        | ((packet_count.min(HCTSIZ_MAX_PACKETS) & 0x03ff) << 19)
        | (len & HCTSIZ_XFERSIZE_MASK)
}

fn hcchar(
    device: u8,
    endpoint: u8,
    direction: Direction,
    ep_type: Dwc2EpType,
    max_packet_size: u16,
    low_speed: bool,
) -> u32 {
    let mut value = u32::from(max_packet_size.max(1)) & 0x7ff;
    value |= (u32::from(endpoint) & 0x0f) << 11;
    value |= (direction as u32) << 15;
    if low_speed {
        value |= 1 << 17;
    }
    value |= ep_type.bits() << 18;
    value |= (u32::from(device) & 0x7f) << 22;
    value
}

fn stage_actual_length(stage: Dwc2TransferStage, hctsiz_after: u32) -> usize {
    if stage.len == 0 {
        return 0;
    }
    if stage.hcchar & HCCHAR_EPDIR == 0 {
        return stage.len;
    }

    let remaining = (hctsiz_after & HCTSIZ_XFERSIZE_MASK) as usize;
    stage.len.saturating_sub(remaining)
}

pub(crate) fn build_control_plan(
    request: &TransferRequest,
    device: u8,
    max_packet_size: u16,
    setup_dma: u32,
    data_dma: u32,
) -> core::result::Result<Dwc2ControlPlan, TransferError> {
    let TransferRequest::Control {
        direction, buffer, ..
    } = request
    else {
        return Err(TransferError::InvalidEndpoint);
    };

    let data_len = buffer.map(|buffer| buffer.len).unwrap_or(0);
    let setup = Dwc2TransferStage {
        hcchar: hcchar(
            device,
            0,
            Direction::Out,
            Dwc2EpType::Control,
            max_packet_size,
            false,
        ),
        hctsiz: hctsiz(Dwc2Pid::Setup, 1, 8),
        dma_addr: setup_dma,
        len: 8,
    };

    let mut data = Vec::new();
    if data_len > 0 {
        let mut offset = 0usize;
        let mut toggle = DataToggle::data1();
        for len in split_dma_lengths(data_len, max_packet_size) {
            let packets = packet_count(len, max_packet_size);
            data.push(Dwc2TransferStage {
                hcchar: hcchar(
                    device,
                    0,
                    *direction,
                    Dwc2EpType::Control,
                    max_packet_size,
                    false,
                ),
                hctsiz: hctsiz(toggle.pid(), packets, len as u32),
                dma_addr: data_dma.wrapping_add(offset as u32),
                len,
            });
            toggle.advance(packets);
            offset += len;
        }
    }

    let status_direction = if data_len > 0 {
        match direction {
            Direction::In => Direction::Out,
            Direction::Out => Direction::In,
        }
    } else {
        Direction::In
    };
    let status = Dwc2TransferStage {
        hcchar: hcchar(
            device,
            0,
            status_direction,
            Dwc2EpType::Control,
            max_packet_size,
            false,
        ),
        hctsiz: hctsiz(Dwc2Pid::Data1, 1, 0),
        dma_addr: setup_dma,
        len: 0,
    };
    Ok(Dwc2ControlPlan {
        setup,
        data,
        status,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DataToggle(bool);

impl DataToggle {
    pub(crate) const fn data0() -> Self {
        Self(false)
    }

    pub(crate) const fn data1() -> Self {
        Self(true)
    }

    pub(crate) fn pid(self) -> Dwc2Pid {
        if self.0 {
            Dwc2Pid::Data1
        } else {
            Dwc2Pid::Data0
        }
    }

    pub(crate) fn advance(&mut self, packet_count: u32) {
        if packet_count % 2 == 1 {
            self.0 = !self.0;
        }
    }
}

fn packet_count(len: usize, max_packet_size: u16) -> u32 {
    if len == 0 {
        return 1;
    }
    let max_packet_size = u32::from(max_packet_size.max(1));
    (len as u32).div_ceil(max_packet_size)
}

fn split_dma_lengths(len: usize, max_packet_size: u16) -> Vec<usize> {
    if len == 0 {
        return vec![0];
    }

    let max_packet_size = usize::from(max_packet_size.max(1));
    let max_by_packets = max_packet_size * HCTSIZ_MAX_PACKETS as usize;
    let max_len = max_by_packets.min(HCTSIZ_XFERSIZE_MASK as usize).max(1);
    let mut left = len;
    let mut out = Vec::new();
    while left > 0 {
        let chunk = left.min(max_len);
        out.push(chunk);
        left -= chunk;
    }
    out
}

fn hcint_fault(bits: u32) -> Option<Dwc2TransferFault> {
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
    } else if bits & HCINT_XFERCOMPL == 0 {
        Some(Dwc2TransferFault::HaltedWithoutComplete)
    } else {
        None
    }
}

fn fault_to_transfer_error(fault: Dwc2TransferFault, hcint: u32) -> TransferError {
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

fn endpoint_number(address: u8) -> u8 {
    address & 0x0f
}

fn endpoint_type_to_dwc2(ty: EndpointType) -> Result<Dwc2EpType> {
    match ty {
        EndpointType::Control => Ok(Dwc2EpType::Control),
        EndpointType::Bulk => Ok(Dwc2EpType::Bulk),
        EndpointType::Interrupt => Ok(Dwc2EpType::Interrupt),
        EndpointType::Isochronous => Err(USBError::NotSupported),
    }
}

fn dma_addr32(addr: u64) -> core::result::Result<u32, TransferError> {
    u32::try_from(addr)
        .map_err(|_| TransferError::Other(anyhow!("DWC2 DMA address above 32-bit mask: {addr:#x}")))
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

#[derive(Default)]
struct Dwc2DmaBufferPool {
    cached: Option<CoherentArray<u8>>,
}

impl Dwc2DmaBufferPool {
    fn take(
        &mut self,
        kernel: &Kernel,
        len: usize,
        stats: &Dwc2Stats,
    ) -> core::result::Result<Option<CoherentArray<u8>>, TransferError> {
        if len == 0 {
            return Ok(None);
        }
        if self
            .cached
            .as_ref()
            .is_some_and(|buffer| buffer.len() >= len)
        {
            return Ok(self.cached.take());
        }

        let buffer = kernel
            .coherent_array_zero_with_align::<u8>(len, DWC2_DMA_ALIGN)
            .map_err(|err| {
                TransferError::Other(anyhow!("DWC2 coherent DMA allocation failed: {err}"))
            })?;
        stats.record_dma_alloc();
        Ok(Some(buffer))
    }

    fn reclaim(&mut self, buffer: Dwc2DmaBuffer) {
        if let Some(coherent) = buffer.coherent {
            self.cached = Some(coherent);
        }
    }
}

struct Dwc2DmaBuffer {
    direction: Direction,
    request_buffer: Option<(NonNull<u8>, usize)>,
    coherent: Option<CoherentArray<u8>>,
    len: usize,
}

impl Dwc2DmaBuffer {
    fn new(
        kernel: &Kernel,
        pool: &mut Dwc2DmaBufferPool,
        stats: &Dwc2Stats,
        request: &TransferRequest,
    ) -> core::result::Result<Self, TransferError> {
        let direction = request.direction();
        let request_buffer = request
            .buffer()
            .filter(|buffer| buffer.len > 0)
            .map(|buffer| (buffer.ptr, buffer.len));
        let len = request_buffer.map_or(0, |(_, len)| len);
        let coherent = if let Some((ptr, len)) = request_buffer {
            let mut coherent = pool.take(kernel, len, stats)?.ok_or_else(|| {
                TransferError::Other(anyhow!("DWC2 DMA buffer pool returned no buffer"))
            })?;
            if matches!(direction, Direction::Out) {
                // SAFETY: `TransferRequest::buffer` is the usb-host transfer
                // contract for a live CPU buffer of `len` bytes until the
                // request completes. We only create a temporary shared slice to
                // copy OUT payload bytes into the owned coherent bounce buffer.
                let data = unsafe { core::slice::from_raw_parts(ptr.as_ptr().cast_const(), len) };
                coherent.write_with_cpu(len, |dst| dst.copy_from_slice(data));
                stats.record_bounce_to_device(len);
            } else {
                coherent.write_with_cpu(len, |dst| dst.fill(0));
                stats.record_bounce_from_device(len);
            }
            Some(coherent)
        } else {
            None
        };

        Ok(Self {
            direction,
            request_buffer,
            coherent,
            len,
        })
    }

    fn buffer_len(&self) -> usize {
        self.len
    }

    fn dma_addr(&self) -> u64 {
        self.coherent
            .as_ref()
            .map_or(0, |buffer| buffer.dma_addr().as_u64())
    }

    fn copy_in_to_request(&self, actual: usize) -> core::result::Result<(), TransferError> {
        if !matches!(self.direction, Direction::In) || actual == 0 {
            return Ok(());
        }
        let Some((ptr, len)) = self.request_buffer else {
            return Err(TransferError::Other(anyhow!(
                "DWC2 IN transfer completed without a request buffer"
            )));
        };
        let Some(coherent) = self.coherent.as_ref() else {
            return Err(TransferError::Other(anyhow!(
                "DWC2 IN transfer completed without a DMA buffer"
            )));
        };
        if actual > len || actual > coherent.len() {
            return Err(TransferError::Other(anyhow!(
                "DWC2 IN transfer actual length {actual} exceeds buffer len {len}"
            )));
        }

        // SAFETY: The request buffer pointer comes from `TransferRequest` and
        // remains owned by that request until this completion path finishes.
        // `actual` has been checked against both the request length and the
        // coherent bounce buffer length, so the mutable slice is in-bounds and
        // used only for the final IN payload copy.
        let dst = unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), actual) };
        coherent.read_with_cpu(actual, |src| dst.copy_from_slice(src));
        Ok(())
    }
}

struct Dwc2Device {
    address: u8,
    regs: Dwc2Registers,
    kernel: Kernel,
    port_speed: Speed,
    channel_count: u8,
    channel_gates: Vec<Arc<Mutex<()>>>,
    channel_completions: Dwc2ChannelCompletions,
    stats: Dwc2Stats,
    desc: Option<DeviceDescriptor>,
    ctrl_ep: Endpoint,
    config_desc: Vec<ConfigurationDescriptor>,
    current_config_value: Option<u8>,
    eps: BTreeMap<u8, Endpoint>,
    ep_interfaces: BTreeMap<u8, u8>,
}

struct Dwc2DeviceParams {
    address: u8,
    regs: Dwc2Registers,
    kernel: Kernel,
    port_speed: Speed,
    channel_count: u8,
    channel_gates: Vec<Arc<Mutex<()>>>,
    channel_completions: Dwc2ChannelCompletions,
    stats: Dwc2Stats,
}

// SAFETY: `Dwc2Device` is owned as a boxed `DeviceOp` and every mutating device
// operation requires `&mut self`. Endpoint objects created from it get their own
// owned DMA pools and share only atomic completions, per-channel gates, and the
// volatile register handle.
unsafe impl Send for Dwc2Device {}

impl Dwc2Device {
    fn new(params: Dwc2DeviceParams) -> Result<Self> {
        let Dwc2DeviceParams {
            address,
            regs,
            kernel,
            port_speed,
            channel_count,
            channel_gates,
            channel_completions,
            stats,
        } = params;
        let raw = Dwc2Endpoint::new(Dwc2EndpointParams {
            regs,
            kernel: kernel.clone(),
            device_address: 0,
            port_speed,
            channel: 0,
            info: EndpointInfo::control(),
            channel_gate: channel_gate(&channel_gates, 0),
            channel_completions: channel_completions.clone(),
            stats: stats.clone(),
        })?;
        Ok(Self {
            address,
            regs,
            kernel,
            port_speed,
            channel_count,
            channel_gates,
            channel_completions,
            stats,
            desc: None,
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
                    "dwc2: isochronous endpoint {:#x} is not supported in v1",
                    desc.address
                );
                continue;
            }
            let info = EndpointInfo::from(&desc);
            let channel = self.channel_for_endpoint(info);
            let raw = Dwc2Endpoint::new(Dwc2EndpointParams {
                regs: self.regs,
                kernel: self.kernel.clone(),
                device_address: self.address,
                port_speed: self.port_speed,
                channel,
                info,
                channel_gate: channel_gate(&self.channel_gates, channel),
                channel_completions: self.channel_completions.clone(),
                stats: self.stats.clone(),
            })?;
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

    fn channel_for_endpoint(&self, info: EndpointInfo) -> u8 {
        if matches!(info.transfer_type, EndpointType::Control) {
            return 0;
        }
        let available = self.channel_count.saturating_sub(1).max(1);
        1 + ((endpoint_number(info.address.raw()).max(1) - 1) % available)
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

#[derive(Clone, Copy, Debug)]
enum Dwc2StageRole {
    ControlSetup,
    ControlData,
    ControlStatus,
    Data {
        direction: Direction,
        max_packet_size: u16,
    },
}

#[derive(Clone, Copy, Debug)]
struct Dwc2QueuedStage {
    stage: Dwc2TransferStage,
    role: Dwc2StageRole,
    nak_retries: u32,
}

#[derive(Clone, Copy)]
struct Dwc2InFlightStage {
    queued: Dwc2QueuedStage,
    nak_left: u32,
    xact_left: u32,
}

struct Dwc2ActiveRequest {
    id: RequestId,
    transfer: Dwc2DmaBuffer,
    _setup_dma: Option<CoherentArray<u8>>,
    stages: Vec<Dwc2QueuedStage>,
    next_stage: usize,
    in_flight: Option<Dwc2InFlightStage>,
    actual_length: usize,
}

struct Dwc2PreparedStages {
    stages: Vec<Dwc2QueuedStage>,
    setup_dma: Option<CoherentArray<u8>>,
}

struct Dwc2Endpoint {
    regs: Dwc2Registers,
    kernel: Kernel,
    device_address: u8,
    port_speed: Speed,
    channel: u8,
    info: EndpointInfo,
    channel_gate: Arc<Mutex<()>>,
    channel_completions: Dwc2ChannelCompletions,
    stats: Dwc2Stats,
    dma_pool: Dwc2DmaBufferPool,
    data_toggle: DataToggle,
    next_request_id: u64,
    active: Option<Dwc2ActiveRequest>,
    completed: Option<(
        RequestId,
        core::result::Result<TransferCompletion, TransferError>,
    )>,
}

// SAFETY: `EndpointOp` methods that mutate transfer state require `&mut self`.
// The endpoint owns its active DMA buffer and reusable bounce buffer pool; the
// IRQ path never accesses those buffers directly and only publishes HCINT bits
// through atomic completion slots. Register programming for the endpoint channel
// is serialized by `channel_gate`.
unsafe impl Send for Dwc2Endpoint {}

struct Dwc2EndpointParams {
    regs: Dwc2Registers,
    kernel: Kernel,
    device_address: u8,
    port_speed: Speed,
    channel: u8,
    info: EndpointInfo,
    channel_gate: Arc<Mutex<()>>,
    channel_completions: Dwc2ChannelCompletions,
    stats: Dwc2Stats,
}

impl Dwc2Endpoint {
    fn new(params: Dwc2EndpointParams) -> Result<Self> {
        let Dwc2EndpointParams {
            regs,
            kernel,
            device_address,
            port_speed,
            channel,
            info,
            channel_gate,
            channel_completions,
            stats,
        } = params;
        endpoint_type_to_dwc2(info.transfer_type)?;
        Ok(Self {
            regs,
            kernel,
            device_address,
            port_speed,
            channel,
            info,
            channel_gate,
            channel_completions,
            stats,
            dma_pool: Dwc2DmaBufferPool::default(),
            data_toggle: DataToggle::data0(),
            next_request_id: 1,
            active: None,
            completed: None,
        })
    }

    fn set_device_address(&mut self, address: u8) {
        self.device_address = address;
    }

    fn set_max_packet_size(&mut self, max_packet_size: u8) {
        self.info.max_packet_size = u16::from(max_packet_size).max(8);
    }

    fn allocate_request_id(&mut self) -> RequestId {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        RequestId::new(id)
    }

    fn prepare_request(
        &mut self,
        id: RequestId,
        request: TransferRequest,
    ) -> core::result::Result<Dwc2ActiveRequest, TransferError> {
        if matches!(request, TransferRequest::Isochronous { .. }) {
            return Err(TransferError::NotSupported);
        }

        self.stats.record_transfer();
        let transfer = Dwc2DmaBuffer::new(&self.kernel, &mut self.dma_pool, &self.stats, &request)?;
        let prepared = match &request {
            TransferRequest::Control { .. } => self.control_stages(&request, &transfer)?,
            TransferRequest::Bulk { .. } | TransferRequest::Interrupt { .. } => {
                Dwc2PreparedStages {
                    stages: self.data_stages(&request, &transfer)?,
                    setup_dma: None,
                }
            }
            TransferRequest::Isochronous { .. } => return Err(TransferError::NotSupported),
        };

        Ok(Dwc2ActiveRequest {
            id,
            transfer,
            _setup_dma: prepared.setup_dma,
            stages: prepared.stages,
            next_stage: 0,
            in_flight: None,
            actual_length: 0,
        })
    }

    fn control_stages(
        &self,
        request: &TransferRequest,
        transfer: &Dwc2DmaBuffer,
    ) -> core::result::Result<Dwc2PreparedStages, TransferError> {
        let TransferRequest::Control {
            setup, direction, ..
        } = request
        else {
            return Err(TransferError::InvalidEndpoint);
        };

        let mut setup_dma = self
            .kernel
            .coherent_array_zero_with_align::<u8>(8, DWC2_DMA_ALIGN)
            .map_err(|err| {
                TransferError::Other(anyhow!("DWC2 setup DMA allocation failed: {err}"))
            })?;
        self.stats.record_dma_alloc();
        let setup_bytes = setup_packet_bytes(setup, *direction, transfer.buffer_len());
        setup_dma.write_with_cpu(8, |dst| dst.copy_from_slice(&setup_bytes));
        let setup_addr = dma_addr32(setup_dma.dma_addr().as_u64())?;
        let data_addr = dma_addr32(transfer.dma_addr())?;
        let plan = build_control_plan(
            request,
            self.device_address,
            self.info.max_packet_size.max(8),
            setup_addr,
            data_addr,
        )?;

        let mut stages = Vec::with_capacity(plan.data.len() + 2);
        stages.push(Dwc2QueuedStage {
            stage: plan.setup,
            role: Dwc2StageRole::ControlSetup,
            nak_retries: DWC2_NAK_RETRIES,
        });
        for stage in plan.data {
            stages.push(Dwc2QueuedStage {
                stage,
                role: Dwc2StageRole::ControlData,
                nak_retries: DWC2_NAK_RETRIES,
            });
        }
        stages.push(Dwc2QueuedStage {
            stage: plan.status,
            role: Dwc2StageRole::ControlStatus,
            nak_retries: DWC2_NAK_RETRIES,
        });
        Ok(Dwc2PreparedStages {
            stages,
            setup_dma: Some(setup_dma),
        })
    }

    fn data_stages(
        &self,
        request: &TransferRequest,
        transfer: &Dwc2DmaBuffer,
    ) -> core::result::Result<Vec<Dwc2QueuedStage>, TransferError> {
        let (direction, ep_type) = match request {
            TransferRequest::Bulk { direction, .. } => (*direction, Dwc2EpType::Bulk),
            TransferRequest::Interrupt { direction, .. } => (*direction, Dwc2EpType::Interrupt),
            _ => return Err(TransferError::InvalidEndpoint),
        };

        let mps = self.info.max_packet_size.max(1);
        let endpoint = endpoint_number(self.info.address.raw());
        let retry_policy = data_stage_retry_policy(ep_type);
        let mut stages = Vec::new();
        let mut toggle = self.data_toggle;
        let mut offset = 0u64;
        for len in split_dma_lengths(transfer.buffer_len(), mps) {
            let packets = packet_count(len, mps);
            let mut stage = Dwc2TransferStage {
                hcchar: hcchar(
                    self.device_address,
                    endpoint,
                    direction,
                    ep_type,
                    mps,
                    matches!(self.port_speed, Speed::Low),
                ),
                hctsiz: hctsiz(toggle.pid(), packets, len as u32),
                dma_addr: dma_addr32(transfer.dma_addr() + offset)?,
                len,
            };
            if matches!(ep_type, Dwc2EpType::Interrupt) {
                stage.hcchar |= self.regs.periodic_odd_frame_bit();
            }
            stages.push(Dwc2QueuedStage {
                stage,
                role: Dwc2StageRole::Data {
                    direction,
                    max_packet_size: mps,
                },
                nak_retries: retry_policy.nak_retries,
            });
            toggle.advance(packets);
            offset += len as u64;
        }
        Ok(stages)
    }

    fn start_active_request(
        &mut self,
        mut active: Dwc2ActiveRequest,
    ) -> core::result::Result<Dwc2ActiveRequest, TransferError> {
        self.start_next_stage(&mut active)?;
        Ok(active)
    }

    fn start_next_stage(
        &self,
        active: &mut Dwc2ActiveRequest,
    ) -> core::result::Result<bool, TransferError> {
        let Some(queued) = active.stages.get(active.next_stage).copied() else {
            return Ok(false);
        };
        active.next_stage += 1;
        self.start_stage(queued.stage)?;
        active.in_flight = Some(Dwc2InFlightStage {
            queued,
            nak_left: queued.nak_retries,
            xact_left: DWC2_XACT_RETRIES,
        });
        Ok(true)
    }

    fn restart_stage(
        &self,
        active: &mut Dwc2ActiveRequest,
        in_flight: Dwc2InFlightStage,
    ) -> core::result::Result<(), TransferError> {
        self.start_stage(in_flight.queued.stage)?;
        active.in_flight = Some(in_flight);
        Ok(())
    }

    fn start_stage(&self, stage: Dwc2TransferStage) -> core::result::Result<(), TransferError> {
        if self.regs.channel_read32(self.channel, HCCHAR) & HCCHAR_CHENA != 0 {
            return Err(TransferError::QueueFull);
        }

        self.stats.record_stage();
        self.channel_completions.clear(self.channel);
        let _guard = self.channel_gate.lock();
        self.regs.channel_write32(self.channel, HCSPLT, 0);
        self.regs
            .channel_write32(self.channel, HCINT, HCINT_ALL_W1C);
        self.regs
            .channel_write32(self.channel, HCINTMSK, HCINT_DMA_IRQ_MASK);
        self.regs
            .channel_write32(self.channel, HCTSIZ, stage.hctsiz);
        mb();
        self.regs
            .channel_write32(self.channel, HCDMA, stage.dma_addr);
        mb();
        self.regs
            .channel_write32(self.channel, HCCHAR, stage.hcchar | HCCHAR_CHENA);
        Ok(())
    }

    fn poll_active_request(
        &mut self,
        mut active: Dwc2ActiveRequest,
    ) -> Option<core::result::Result<TransferCompletion, TransferError>> {
        let Some(hcint) = self.channel_completions.take(self.channel) else {
            self.active = Some(active);
            return None;
        };
        let Some(mut in_flight) = active.in_flight.take() else {
            return Some(self.complete_active_request(
                active,
                Err(TransferError::Other(anyhow!(
                    "DWC2 completion arrived without an in-flight stage"
                ))),
            ));
        };

        if let Some(fault) = hcint_fault(hcint) {
            self.stats.record_fault(fault);
            match fault {
                Dwc2TransferFault::Nak if in_flight.nak_left > 0 => {
                    in_flight.nak_left -= 1;
                    if let Err(err) = self.restart_stage(&mut active, in_flight) {
                        return Some(self.complete_active_request(active, Err(err)));
                    }
                    self.active = Some(active);
                    return None;
                }
                Dwc2TransferFault::Xact if in_flight.xact_left > 0 => {
                    in_flight.xact_left -= 1;
                    if let Err(err) = self.restart_stage(&mut active, in_flight) {
                        return Some(self.complete_active_request(active, Err(err)));
                    }
                    self.active = Some(active);
                    return None;
                }
                _ => {
                    warn!(
                        "dwc2: transfer fault channel={} role={:?} hcint={:#x} nak_left={} \
                         xact_left={}",
                        self.channel,
                        in_flight.queued.role,
                        hcint,
                        in_flight.nak_left,
                        in_flight.xact_left
                    );
                    return Some(self.complete_active_request(
                        active,
                        Err(fault_to_transfer_error(fault, hcint)),
                    ));
                }
            }
        }

        let hctsiz_after = self.regs.channel_read32(self.channel, HCTSIZ);
        let actual = stage_actual_length(in_flight.queued.stage, hctsiz_after);
        match in_flight.queued.role {
            Dwc2StageRole::ControlSetup | Dwc2StageRole::ControlStatus => {}
            Dwc2StageRole::ControlData => {
                active.actual_length = active.actual_length.saturating_add(actual);
            }
            Dwc2StageRole::Data {
                direction,
                max_packet_size,
            } => {
                active.actual_length = active.actual_length.saturating_add(actual);
                self.data_toggle.advance(successful_packet_count(
                    actual,
                    in_flight.queued.stage.len,
                    max_packet_size,
                ));
                if matches!(direction, Direction::In) && actual < in_flight.queued.stage.len {
                    return Some(self.complete_active_request(active, Ok(())));
                }
            }
        }

        match self.start_next_stage(&mut active) {
            Ok(true) => {
                self.active = Some(active);
                None
            }
            Ok(false) => Some(self.complete_active_request(active, Ok(()))),
            Err(err) => Some(self.complete_active_request(active, Err(err))),
        }
    }

    fn complete_active_request(
        &mut self,
        active: Dwc2ActiveRequest,
        result: core::result::Result<(), TransferError>,
    ) -> core::result::Result<TransferCompletion, TransferError> {
        let id = active.id;
        let completion = result.and_then(|()| {
            active.transfer.copy_in_to_request(active.actual_length)?;
            Ok(TransferCompletion {
                request_id: id,
                status: TransferStatus::Completed,
                actual_length: active.actual_length,
                iso_packets: Vec::new(),
            })
        });
        self.dma_pool.reclaim(active.transfer);
        self.channel_completions.end_request(self.channel);
        completion
    }
}

impl crate::backend::ty::ep::EndpointOp for Dwc2Endpoint {
    fn submit_request(
        &mut self,
        request: TransferRequest,
    ) -> core::result::Result<RequestId, TransferError> {
        if self.active.is_some() || self.completed.is_some() {
            return Err(TransferError::QueueFull);
        }
        let id = self.allocate_request_id();
        if !self.channel_completions.try_begin_request(self.channel) {
            return Err(TransferError::QueueFull);
        }
        self.channel_completions.clear(self.channel);
        let active = match self.prepare_request(id, request) {
            Ok(active) => active,
            Err(err) => {
                self.channel_completions.end_request(self.channel);
                return Err(err);
            }
        };
        let active = match self.start_active_request(active) {
            Ok(active) => active,
            Err(err) => {
                self.channel_completions.end_request(self.channel);
                return Err(err);
            }
        };
        self.active = Some(active);
        Ok(id)
    }

    fn reclaim_request(
        &mut self,
        id: RequestId,
    ) -> Option<core::result::Result<TransferCompletion, TransferError>> {
        if let Some((completed_id, _)) = self.completed.as_ref() {
            if *completed_id != id {
                return Some(Err(TransferError::InvalidEndpoint));
            }
            return self.completed.take().map(|(_, result)| result);
        }

        let active = self.active.take()?;
        if active.id != id {
            self.active = Some(active);
            return Some(Err(TransferError::InvalidEndpoint));
        }
        self.poll_active_request(active)
    }

    fn register_waker(&self, id: RequestId, cx: &mut Context<'_>) {
        if self.active.as_ref().is_some_and(|active| active.id == id) {
            self.channel_completions.register_waker(self.channel, cx);
        }
    }

    fn cancel_request(&mut self, id: RequestId) -> core::result::Result<(), TransferError> {
        if self
            .completed
            .as_ref()
            .is_some_and(|(done_id, _)| *done_id == id)
        {
            self.completed = Some((id, Err(TransferError::Cancelled)));
            return Ok(());
        }
        let Some(active) = self.active.take() else {
            return Err(TransferError::InvalidEndpoint);
        };
        if active.id != id {
            self.active = Some(active);
            return Err(TransferError::InvalidEndpoint);
        }
        let value = self.regs.channel_read32(self.channel, HCCHAR);
        if value & HCCHAR_CHENA != 0 {
            self.regs
                .channel_write32(self.channel, HCCHAR, value | HCCHAR_CHENA | HCCHAR_CHDIS);
        }
        self.dma_pool.reclaim(active.transfer);
        self.channel_completions.end_request(self.channel);
        self.completed = Some((id, Err(TransferError::Cancelled)));
        self.channel_completions.publish(self.channel, HCINT_CHHLTD);
        Ok(())
    }
}

fn successful_packet_count(actual: usize, requested: usize, max_packet_size: u16) -> u32 {
    if requested == 0 || actual == 0 {
        1
    } else {
        packet_count(actual, max_packet_size)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{
        alloc::{alloc_zeroed, dealloc},
        vec,
        vec::Vec,
    };
    use core::{
        alloc::Layout,
        num::NonZeroUsize,
        ptr::NonNull,
        sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
    };

    use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp};
    use usb_if::{
        descriptor::EndpointType,
        endpoint::{EndpointAddress, EndpointInfo, TransferRequest},
        host::{ControlSetup, hub::Speed},
        transfer::{Direction, Recipient, Request, RequestType},
    };

    use super::{
        DWC2_RUNTIME_GINTMSK, DataStageRetryPolicy, DataToggle, Dwc2ChannelCompletions,
        Dwc2DmaBuffer, Dwc2DmaBufferPool, Dwc2Endpoint, Dwc2EndpointParams, Dwc2EpType,
        Dwc2EventHandler, Dwc2FifoSizes, Dwc2HostParams, Dwc2NewParams, Dwc2Pid, Dwc2PortStatus,
        Dwc2Stats, Dwc2TransferFault, Dwc2TransferStage, GINTMSK, GINTSTS, GINTSTS_HCHINT, HAINT,
        HAINTMSK, HCCHAR, HCCHAR_CHDIS, HCCHAR_CHENA, HCINT, HCINT_AHBERR, HCINT_BBLERR,
        HCINT_CHHLTD, HCINT_DMA_IRQ_MASK, HCINT_NAK, HCINT_STALL, HCINT_XACTERR, HCINT_XFERCOMPL,
        HCINTMSK, HPRT_CONN_DET, HPRT_CONN_STS, HPRT_ENA, HPRT_ENA_CHG, HPRT_OVRCUR_CHG,
        build_channel_gates, build_control_plan, build_gahbcfg_internal_dma,
        data_stage_retry_policy, fifo_register_plan, hcchar, hcint_fault, hctsiz, packet_count,
        split_dma_lengths, stage_actual_length, successful_packet_count,
    };
    use crate::backend::{
        kmod::osal::Kernel,
        ty::{Event, EventHandlerOp, ep::EndpointOp},
    };

    struct TestKernel;

    static TEST_DMA_ADDR: AtomicU64 = AtomicU64::new(0x1000);

    impl DmaOp for TestKernel {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            // SAFETY: The test kernel models contiguous DMA with the same
            // heap-backed allocation used for coherent DMA below.
            unsafe { self.alloc_coherent(constraints, layout) }
        }

        unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
            // SAFETY: Handles returned by `alloc_contiguous` are created by
            // `alloc_coherent`, so they must be released through the same mock
            // deallocation path.
            unsafe { self.dealloc_coherent(handle) }
        }

        unsafe fn alloc_coherent(
            &self,
            constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            // SAFETY: Unit tests request valid `Layout` values. The returned
            // pointer is either null or points to a heap allocation owned by the
            // DMA handle until `dealloc_coherent` consumes it.
            let ptr = unsafe { alloc_zeroed(layout) };
            let ptr = NonNull::new(ptr)?;
            let align = constraints.align.max(layout.align()).max(1) as u64;
            let size = layout.size().max(1) as u64;
            let current = TEST_DMA_ADDR.fetch_add(size + align, AtomicOrdering::Relaxed);
            let dma_addr = (current + align - 1) & !(align - 1);
            // SAFETY: `ptr` and `layout` describe the allocation above, and
            // `dma_addr` is a deterministic fake bus address used only by unit
            // tests that never reaches real hardware.
            Some(unsafe { DmaAllocHandle::new(ptr, dma_addr.into(), layout) })
        }

        unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
            // SAFETY: The mock only creates coherent handles from
            // `alloc_zeroed` with the stored layout, so deallocating with the
            // same layout releases exactly that allocation.
            unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) }
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            addr: NonNull<u8>,
            size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> core::result::Result<DmaMapHandle, DmaError> {
            let layout = Layout::from_size_align(size.get(), 1)?;
            // SAFETY: This mock streaming map does not transfer ownership of
            // `addr`; it records the caller-provided live buffer and a fake bus
            // address for tests that only inspect programming values.
            Ok(unsafe { DmaMapHandle::new(addr, (addr.as_ptr() as u64).into(), layout, None) })
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
    }

    impl super::KernelOp for TestKernel {
        fn delay(&self, _duration: core::time::Duration) {}
    }

    static TEST_KERNEL: TestKernel = TestKernel;

    const GAHBCFG_GLBL_INTR_EN: u32 = 1 << 0;
    const GAHBCFG_HBSTLEN_INCR16: u32 = 7 << 1;
    const GAHBCFG_DMA_EN: u32 = 1 << 5;

    fn test_regs() -> (Vec<u32>, super::Dwc2Registers) {
        let mut regs = vec![0u32; 1024];
        let base = NonNull::new(regs.as_mut_ptr().cast::<u8>()).unwrap();
        (regs, super::Dwc2Registers { base })
    }

    fn capture_and_service(handler: &Dwc2EventHandler) -> Event {
        let (event, masked) = match handler.capture_irq() {
            rdif_irq::IrqCapture::Captured { event, masked } => (event, masked),
            capture => panic!("expected captured DWC2 event, got {capture:?}"),
        };
        let activity = handler
            .service_host_events(event)
            .expect("captured DWC2 event must service");
        handler
            .rearm_sources(masked.expect("DWC2 capture masks its source"))
            .expect("serviced DWC2 source must rearm");
        activity
    }

    #[test]
    fn gahbcfg_requires_internal_dma_and_enables_incr16() {
        let value = build_gahbcfg_internal_dma(2).expect("internal DMA architecture is supported");

        assert_eq!(
            value & (GAHBCFG_GLBL_INTR_EN | GAHBCFG_HBSTLEN_INCR16 | GAHBCFG_DMA_EN),
            GAHBCFG_GLBL_INTR_EN | GAHBCFG_HBSTLEN_INCR16 | GAHBCFG_DMA_EN
        );
        assert!(build_gahbcfg_internal_dma(1).is_err());
    }

    #[test]
    fn fifo_register_plan_matches_sg2002_dtb_defaults() {
        let plan = fifo_register_plan(Dwc2FifoSizes::sg2002_default());

        assert_eq!(plan.grxfsiz, 536);
        assert_eq!(plan.gnptxfsiz, (32 << 16) | 536);
        assert_eq!(plan.hptxfsiz, (768 << 16) | (536 + 32));
    }

    #[test]
    fn runtime_irq_mask_enables_topology_and_host_channel_events() {
        let (_backing, regs) = test_regs();
        let mut host = super::Dwc2::new(Dwc2NewParams {
            mmio: regs.base,
            kernel: &TEST_KERNEL,
            params: Dwc2HostParams::sg2002(),
        })
        .unwrap();

        crate::backend::kmod::kcore::CoreOp::enable_irq(&mut host).unwrap();

        assert_eq!(regs.read32(GINTMSK), DWC2_RUNTIME_GINTMSK);
    }

    #[test]
    fn device_descriptor_base_is_parsed_without_unaligned_struct_access() {
        let desc =
            super::device_descriptor_base_from_bytes([18, 1, 0x00, 0x02, 0x08, 0x06, 0x50, 64]);

        assert_eq!(desc.length, 18);
        assert_eq!(desc.descriptor_type, 1);
        assert_eq!(desc.usb_version, 0x0200);
        assert_eq!(desc.class, 0x08);
        assert_eq!(desc.subclass, 0x06);
        assert_eq!(desc.protocol, 0x50);
        assert_eq!(desc.max_packet_size_0, 64);
    }

    #[test]
    fn hctsiz_encodes_pid_packet_count_and_transfer_size() {
        assert_eq!(hctsiz(Dwc2Pid::Setup, 1, 8), (3 << 29) | (1 << 19) | 8);
        assert_eq!(
            hctsiz(Dwc2Pid::Data1, 2, 1024),
            (2 << 29) | (2 << 19) | 1024
        );
    }

    #[test]
    fn hcchar_encodes_control_bulk_and_interrupt_dma_channels() {
        let control = hcchar(0, 0, Direction::Out, Dwc2EpType::Control, 64, false);
        assert_eq!(control & 0x7ff, 64);
        assert_eq!((control >> 18) & 0b11, 0);
        assert_eq!((control >> 22) & 0x7f, 0);

        let bulk_in = hcchar(5, 2, Direction::In, Dwc2EpType::Bulk, 512, false);
        assert_eq!(bulk_in & 0x7ff, 512);
        assert_eq!((bulk_in >> 11) & 0x0f, 2);
        assert_eq!((bulk_in >> 15) & 1, 1);
        assert_eq!((bulk_in >> 18) & 0b11, 2);
        assert_eq!((bulk_in >> 22) & 0x7f, 5);

        let interrupt_low = hcchar(3, 1, Direction::In, Dwc2EpType::Interrupt, 8, true);
        assert_eq!((interrupt_low >> 17) & 1, 1);
        assert_eq!((interrupt_low >> 18) & 0b11, 3);
    }

    #[test]
    fn control_in_plan_uses_dma_for_setup_data_and_status() {
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

        let plan = build_control_plan(&request, 0, 64, 0x1000, 0x2000).unwrap();

        assert_eq!(plan.setup.dma_addr, 0x1000);
        assert_eq!(plan.setup.len, 8);
        assert_eq!(plan.setup.hctsiz, hctsiz(Dwc2Pid::Setup, 1, 8));
        let data = plan.data.first().expect("control IN has a data stage");
        assert_eq!(data.dma_addr, 0x2000);
        assert_eq!(data.hctsiz, hctsiz(Dwc2Pid::Data1, 1, 18));
        assert_eq!((data.hcchar >> 15) & 1, 1);
        assert_eq!(plan.status.dma_addr, 0x1000);
        assert_eq!(plan.status.hctsiz, hctsiz(Dwc2Pid::Data1, 1, 0));
        assert_eq!((plan.status.hcchar >> 15) & 1, 0);
    }

    #[test]
    fn dma_buffer_bounces_in_data_through_coherent_dma_memory() {
        let kernel = Kernel::new(u64::MAX, &TEST_KERNEL);
        let mut pool = Dwc2DmaBufferPool::default();
        let stats = Dwc2Stats::new();
        let mut data = [0u8; 4];
        let request = TransferRequest::bulk_in(&mut data);
        let mut dma = Dwc2DmaBuffer::new(&kernel, &mut pool, &stats, &request).unwrap();

        assert_eq!(dma.buffer_len(), 4);
        assert_ne!(dma.dma_addr(), data.as_ptr() as u64);
        dma.coherent
            .as_mut()
            .unwrap()
            .write_with_cpu(4, |dst| dst.copy_from_slice(&[1, 2, 3, 4]));
        dma.copy_in_to_request(3).unwrap();

        assert_eq!(data, [1, 2, 3, 0]);
    }

    #[test]
    fn dma_buffer_pool_reuses_existing_dma_allocation() {
        let kernel = Kernel::new(u64::MAX, &TEST_KERNEL);
        let stats = Dwc2Stats::new();
        let mut pool = Dwc2DmaBufferPool::default();
        let mut first = [0u8; 64];
        let first_request = TransferRequest::bulk_in(&mut first);
        let first_dma = Dwc2DmaBuffer::new(&kernel, &mut pool, &stats, &first_request).unwrap();
        let first_addr = first_dma.dma_addr();
        pool.reclaim(first_dma);

        let mut smaller = [0u8; 32];
        let smaller_request = TransferRequest::bulk_in(&mut smaller);
        let smaller_dma = Dwc2DmaBuffer::new(&kernel, &mut pool, &stats, &smaller_request).unwrap();
        assert_eq!(smaller_dma.dma_addr(), first_addr);
        pool.reclaim(smaller_dma);

        let mut larger = [0u8; 128];
        let larger_request = TransferRequest::bulk_in(&mut larger);
        let larger_dma = Dwc2DmaBuffer::new(&kernel, &mut pool, &stats, &larger_request).unwrap();
        assert_eq!(larger_dma.buffer_len(), 128);
        pool.reclaim(larger_dma);

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.dma_allocs, 2);
        assert_eq!(snapshot.bounce_from_device_bytes, 64 + 32 + 128);
    }

    #[test]
    fn stats_records_transfer_faults_and_wait_iterations() {
        let stats = Dwc2Stats::new();

        stats.record_transfer();
        stats.record_stage();
        stats.record_fault(Dwc2TransferFault::Nak);
        stats.record_fault(Dwc2TransferFault::Xact);
        stats.record_timeout();
        stats.record_transfer_busy_wait_iters(17);
        stats.record_init_wait_iters(3);
        stats.record_irq_event();
        stats.record_channel_completion();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.transfers, 1);
        assert_eq!(snapshot.stages, 1);
        assert_eq!(snapshot.naks, 1);
        assert_eq!(snapshot.xact_errors, 1);
        assert_eq!(snapshot.timeouts, 1);
        assert_eq!(snapshot.transfer_busy_wait_iters, 17);
        assert_eq!(snapshot.init_wait_iters, 3);
        assert_eq!(snapshot.wait_iters, 20);
        assert_eq!(snapshot.irq_events, 1);
        assert_eq!(snapshot.channel_completions, 1);
    }

    #[test]
    fn hchint_event_publishes_channel_completion_without_endpoint_polling_hcint() {
        let (_backing, regs) = test_regs();
        let completions = Dwc2ChannelCompletions::new();
        let stats = Dwc2Stats::new();
        let handler = Dwc2EventHandler::new(regs, completions.clone(), stats.clone());
        let hcint = HCINT_CHHLTD | HCINT_XFERCOMPL;

        regs.write32(GINTMSK, GINTSTS_HCHINT);
        regs.write32(GINTSTS, GINTSTS_HCHINT);
        regs.write32(HAINTMSK, 1);
        regs.write32(HAINT, 1);
        regs.channel_write32(0, HCINTMSK, hcint);
        regs.channel_write32(0, HCINT, hcint);

        match capture_and_service(&handler) {
            Event::TransferActivity { count } => assert_eq!(count, 1),
            event => panic!("expected transfer activity, got {event:?}"),
        }
        assert_eq!(completions.take(0), Some(hcint));

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.irq_events, 1);
        assert_eq!(snapshot.channel_completions, 1);
    }

    #[test]
    fn chhltd_completion_preserves_unmasked_raw_hcint_reason() {
        let (_backing, regs) = test_regs();
        let completions = Dwc2ChannelCompletions::new();
        let stats = Dwc2Stats::new();
        let handler = Dwc2EventHandler::new(regs, completions.clone(), stats.clone());

        regs.write32(GINTMSK, GINTSTS_HCHINT);
        regs.write32(GINTSTS, GINTSTS_HCHINT);
        regs.write32(HAINTMSK, 1);
        regs.write32(HAINT, 1);
        regs.channel_write32(0, HCINTMSK, HCINT_CHHLTD);
        regs.channel_write32(0, HCINT, HCINT_CHHLTD | HCINT_XFERCOMPL);

        match capture_and_service(&handler) {
            Event::TransferActivity { count } => assert_eq!(count, 1),
            event => panic!("expected transfer activity, got {event:?}"),
        }
        assert_eq!(completions.take(0), Some(HCINT_CHHLTD | HCINT_XFERCOMPL));
    }

    #[test]
    fn xfercomplete_without_channel_halt_requests_halt_before_publish() {
        let (_backing, regs) = test_regs();
        let completions = Dwc2ChannelCompletions::new();
        let stats = Dwc2Stats::new();
        let handler = Dwc2EventHandler::new(regs, completions.clone(), stats.clone());

        regs.write32(GINTMSK, GINTSTS_HCHINT);
        regs.write32(GINTSTS, GINTSTS_HCHINT);
        regs.write32(HAINTMSK, 1);
        regs.write32(HAINT, 1);
        regs.channel_write32(0, HCCHAR, HCCHAR_CHENA);
        regs.channel_write32(0, HCINTMSK, HCINT_CHHLTD | HCINT_XFERCOMPL);
        regs.channel_write32(0, HCINT, HCINT_XFERCOMPL);

        match capture_and_service(&handler) {
            Event::TransferActivity { .. } => {}
            event => panic!("expected transfer activity, got {event:?}"),
        }

        assert_eq!(completions.take(0), None);
        assert_eq!(regs.channel_read32(0, HCCHAR) & HCCHAR_CHDIS, HCCHAR_CHDIS);
        assert_eq!(stats.snapshot().channel_completions, 0);

        regs.write32(GINTSTS, GINTSTS_HCHINT);
        regs.write32(HAINT, 1);
        regs.channel_write32(0, HCCHAR, 0);
        regs.channel_write32(0, HCINT, HCINT_CHHLTD);

        match capture_and_service(&handler) {
            Event::TransferActivity { count } => assert_eq!(count, 1),
            event => panic!("expected transfer activity, got {event:?}"),
        }

        assert_eq!(completions.take(0), Some(HCINT_CHHLTD | HCINT_XFERCOMPL));
        assert_eq!(stats.snapshot().channel_completions, 1);
    }

    #[test]
    fn endpoint_submit_waits_for_irq_completion_before_reclaiming() {
        let (_backing, regs) = test_regs();
        let kernel = Kernel::new(u64::MAX, &TEST_KERNEL);
        let completions = Dwc2ChannelCompletions::new();
        let stats = Dwc2Stats::new();
        let mut endpoint = Dwc2Endpoint::new(Dwc2EndpointParams {
            regs,
            kernel,
            device_address: 2,
            port_speed: Speed::High,
            channel: 1,
            info: EndpointInfo {
                address: EndpointAddress::new(0x81),
                transfer_type: EndpointType::Bulk,
                direction: Direction::In,
                max_packet_size: 512,
                packets_per_microframe: 1,
                interval: 0,
            },
            channel_gate: build_channel_gates(2)[1].clone(),
            channel_completions: completions.clone(),
            stats: stats.clone(),
        })
        .unwrap();
        let mut data = [0u8; 512];
        let id = endpoint
            .submit_request(TransferRequest::bulk_in(&mut data))
            .unwrap();

        let hcintmsk = regs.channel_read32(1, HCINTMSK);
        assert_eq!(hcintmsk, HCINT_DMA_IRQ_MASK);
        assert_eq!(hcintmsk & HCINT_NAK, 0);
        assert_eq!(hcintmsk & HCINT_XFERCOMPL, 0);
        assert!(endpoint.reclaim_request(id).is_none());

        completions.publish(1, HCINT_CHHLTD | HCINT_XFERCOMPL);
        assert!(endpoint.reclaim_request(id).is_some());
        assert_eq!(stats.snapshot().transfer_busy_wait_iters, 0);
    }

    #[test]
    fn bulk_and_interrupt_data_stages_retry_nak_like_linux_hcd_flow_control() {
        assert_eq!(
            data_stage_retry_policy(Dwc2EpType::Bulk),
            DataStageRetryPolicy { nak_retries: 64 }
        );
        assert_eq!(
            data_stage_retry_policy(Dwc2EpType::Interrupt),
            DataStageRetryPolicy { nak_retries: 64 }
        );
    }

    #[test]
    fn channel_gates_are_per_channel_instead_of_one_global_gate() {
        let gates = build_channel_gates(4);

        assert_eq!(gates.len(), 4);
        assert!(alloc::sync::Arc::ptr_eq(&gates[1], &gates[1]));
        assert!(!alloc::sync::Arc::ptr_eq(&gates[1], &gates[2]));
    }

    #[test]
    fn out_stage_completion_reports_requested_length() {
        let stage = Dwc2TransferStage {
            hcchar: hcchar(2, 2, Direction::Out, Dwc2EpType::Bulk, 512, false),
            hctsiz: hctsiz(Dwc2Pid::Data0, 1, 31),
            dma_addr: 0,
            len: 31,
        };

        assert_eq!(
            stage_actual_length(stage, hctsiz(Dwc2Pid::Data0, 1, 31)),
            31
        );

        let in_stage = Dwc2TransferStage {
            hcchar: hcchar(2, 1, Direction::In, Dwc2EpType::Bulk, 512, false),
            hctsiz: hctsiz(Dwc2Pid::Data0, 1, 31),
            dma_addr: 0,
            len: 31,
        };

        assert_eq!(
            stage_actual_length(in_stage, hctsiz(Dwc2Pid::Data0, 1, 13)),
            18
        );
    }

    #[test]
    fn control_data_stage_splits_at_hctsiz_packet_limit_and_toggles_pid() {
        let mut data = [0u8; 2048];
        let request = TransferRequest::control_in(
            ControlSetup {
                request_type: RequestType::Standard,
                recipient: Recipient::Device,
                request: Request::GetDescriptor,
                value: 0x0200,
                index: 0,
            },
            &mut data,
        );

        let plan = build_control_plan(&request, 2, 8, 0x1000, 0x2000).unwrap();

        assert_eq!(plan.data.len(), 1);
        assert_eq!(plan.data[0].hctsiz >> 29, Dwc2Pid::Data1.bits());
        assert_eq!(split_dma_lengths(8192, 8), [8184, 8]);
    }

    #[test]
    fn data_toggle_advances_by_packet_count() {
        let mut toggle = DataToggle::data0();

        assert_eq!(toggle.pid(), Dwc2Pid::Data0);
        toggle.advance(packet_count(512, 512));
        assert_eq!(toggle.pid(), Dwc2Pid::Data1);
        toggle.advance(packet_count(1024, 512));
        assert_eq!(toggle.pid(), Dwc2Pid::Data1);
        toggle.advance(packet_count(1, 512));
        assert_eq!(toggle.pid(), Dwc2Pid::Data0);
        assert_eq!(successful_packet_count(0, 64, 64), 1);
    }

    #[test]
    fn hprt_status_decodes_speed_and_preserves_w1c_bits_for_rmw() {
        let status = Dwc2PortStatus::from_raw(
            HPRT_CONN_STS | HPRT_ENA | HPRT_CONN_DET | HPRT_ENA_CHG | HPRT_OVRCUR_CHG | (2 << 17),
        );

        assert!(status.connected());
        assert!(status.enabled());
        assert_eq!(status.speed(), Speed::Low);
        assert_eq!(
            status.rmw_preserving_w1c() & (HPRT_CONN_DET | HPRT_ENA | HPRT_ENA_CHG),
            0
        );
    }

    #[test]
    fn hcint_fault_maps_nak_stall_xact_and_bus_errors() {
        assert_eq!(hcint_fault(HCINT_NAK), Some(Dwc2TransferFault::Nak));
        assert_eq!(hcint_fault(HCINT_STALL), Some(Dwc2TransferFault::Stall));
        assert_eq!(hcint_fault(HCINT_XACTERR), Some(Dwc2TransferFault::Xact));
        assert_eq!(hcint_fault(HCINT_AHBERR), Some(Dwc2TransferFault::Ahb));
        assert_eq!(hcint_fault(HCINT_BBLERR), Some(Dwc2TransferFault::Babble));
    }
}
