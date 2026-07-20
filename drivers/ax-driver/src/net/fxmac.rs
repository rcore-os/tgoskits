use alloc::{boxed::Box, collections::VecDeque, format, sync::Arc, vec, vec::Vec};
use core::{
    alloc::Layout,
    cmp,
    sync::atomic::{AtomicU64, Ordering},
};

use ax_kspin::SpinNoIrq as Mutex;
use dma_api::{DmaAddr, DmaAllocHandle, DmaConstraints, DmaOp};
use fxmac_rs::{
    FXMAC_MMIO_PHYS_BASE, FXMAC_MMIO_SIZE, FXMAC_RUNTIME_IRQ_MASK, FXmac, FXmacInitPoll,
    FXmacInitSchedule, FXmacInitialization, FXmacIrqPort, FXmacIrqStatus, FXmacLwipPortTx,
    FXmacPending, FXmacRecvHandler, begin_xmac_init, discover_xmac, poll_xmac_init,
};
use rd_net::{
    ContainmentCause, DmaBuffer, EthernetIrqFault, Event, IRxQueue, ITxQueue, InterfaceIrqEndpoint,
    IrqCapture, MaskedSource, NetError, OwnerInitInput, OwnerInitPoll, OwnerInitSchedule,
    QueueConfig, QueueMemoryMode,
};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};

use crate::{binding_info_from_fdt, net::PlatformDeviceNet};

pub const DEVICE_NAME: &str = "fxmac";

const DRIVER_NAME: &str = "cdns,phytium-gem-1.0";
const QUEUE_ID: usize = 0;
const QUEUE_SIZE: usize = 64;
const BUFFER_SIZE: usize = 2048;
const DMA_ALIGN: usize = 0x1000;
const DMA_MASK: u64 = u64::MAX;
const PAGE_SIZE: usize = 0x1000;

crate::model_register!(
    name: "FXMAC FDT Network",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &[DRIVER_NAME],
        on_probe: probe_fdt,
    }],
);

fn probe_fdt(probe: rdrive::register::ProbeFdt<'_>) -> Result<(), rdrive::probe::OnProbeError> {
    let info = binding_info_from_fdt(probe.info())?;
    let register = probe
        .info()
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other("FXMAC FDT node has no register aperture"))?;
    let mapped_size = usize::try_from(register.size.unwrap_or(FXMAC_MMIO_SIZE as u64))
        .map_err(|_| OnProbeError::other("FXMAC register aperture does not fit usize"))?;
    let dev = FxmacNet::new(register.address as usize, mapped_size)?;
    probe
        .into_platform_device()
        .register_net_with_info(DRIVER_NAME, dev, info);
    log::info!("registered FXmac FDT network device");
    Ok(())
}

pub fn register(plat_dev: PlatformDevice) {
    match FxmacNet::new(FXMAC_MMIO_PHYS_BASE, FXMAC_MMIO_SIZE) {
        Ok(dev) => {
            plat_dev.register_net(DRIVER_NAME, dev);
            log::info!("registered FXmac network device");
        }
        Err(error) => log::error!("failed to discover FXmac network device: {error}"),
    }
}

struct FxmacNet {
    hw: Arc<Mutex<FxmacHw>>,
    irq_port: Option<FXmacIrqPort>,
    tx_state: Arc<Mutex<FxmacTxState>>,
    rx_state: Arc<Mutex<FxmacRxState>>,
    irq_epoch: Arc<FxmacIrqEpoch>,
    hwaddr: [u8; 6],
    tx_created: bool,
    rx_created: bool,
    irq_enabled: bool,
}

impl FxmacNet {
    fn new(physical_base: usize, mapped_size: usize) -> Result<Self, OnProbeError> {
        let registers = Arc::new(
            axklib::mmio::ioremap(physical_base.into(), mapped_size).map_err(|error| {
                OnProbeError::other(format!("failed to map FXMAC registers: {error}"))
            })?,
        );
        let discovery = unsafe {
            // SAFETY: `registers` is the unique owning mapping lease and is
            // retained by both the owner state and IRQ endpoint adapter until
            // runtime teardown/quarantine has stopped all device access.
            discover_xmac(registers.as_nonnull_ptr(), registers.size())
        }
        .map_err(|error| OnProbeError::other(format!("invalid FXMAC mapping: {error}")))?;
        let (pending, irq_port) = discovery.into_parts();
        Ok(Self {
            hw: Arc::new(Mutex::new(FxmacHw {
                _registers: Arc::clone(&registers),
                state: FxmacHwState::Pending(pending),
            })),
            irq_port: Some(irq_port),
            tx_state: Arc::new(Mutex::new(FxmacTxState {
                tx_done: VecDeque::with_capacity(QUEUE_SIZE),
            })),
            rx_state: Arc::new(Mutex::new(FxmacRxState {
                rx_buffers: VecDeque::with_capacity(QUEUE_SIZE),
                rx_packets: VecDeque::with_capacity(QUEUE_SIZE),
            })),
            irq_epoch: Arc::new(FxmacIrqEpoch::new()),
            hwaddr: [0; 6],
            tx_created: false,
            rx_created: false,
            irq_enabled: false,
        })
    }

    fn poll_initialization(&mut self, now_ns: u64) -> OwnerInitPoll {
        let progress = self.hw.lock().poll_initialization(now_ns);
        match progress {
            FxmacOwnerInitProgress::Ready(hwaddr) => {
                self.hwaddr = hwaddr;
                OwnerInitPoll::Ready
            }
            FxmacOwnerInitProgress::Pending(schedule) if schedule.run_again => {
                OwnerInitPoll::Pending(OwnerInitSchedule::run_again())
            }
            FxmacOwnerInitProgress::Pending(schedule) => {
                let Some(wake_at_ns) = schedule.wake_at_ns else {
                    return OwnerInitPoll::Failed(NetError::Other(Box::new(
                        rd_net::KError::Unknown("FXMAC init returned no activation source"),
                    )));
                };
                OwnerInitPoll::Pending(OwnerInitSchedule::wait_until(wake_at_ns))
            }
            FxmacOwnerInitProgress::Failed(error) => {
                OwnerInitPoll::Failed(NetError::Other(Box::new(error)))
            }
        }
    }
}

impl DriverGeneric for FxmacNet {
    fn name(&self) -> &str {
        DRIVER_NAME
    }
}

impl rd_net::Interface for FxmacNet {
    fn poll_owner_init(&mut self, input: OwnerInitInput) -> OwnerInitPoll {
        self.poll_initialization(input.now_ns)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.hwaddr
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        if self.tx_created || !self.hw.lock().is_ready() {
            return None;
        }
        self.tx_created = true;
        Some(Box::new(FxmacTxQueue {
            hw: Arc::clone(&self.hw),
            tx_state: Arc::clone(&self.tx_state),
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if self.rx_created || !self.hw.lock().is_ready() {
            return None;
        }
        self.rx_created = true;
        Some(Box::new(FxmacRxQueue {
            hw: Arc::clone(&self.hw),
            rx_state: Arc::clone(&self.rx_state),
        }))
    }

    fn enable_irq(&mut self) -> Result<(), NetError> {
        let mut hw = self.hw.lock();
        let device = hw.device_mut().ok_or_else(fxmac_not_ready)?;
        device.enable_irq();
        self.irq_enabled = true;
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<(), NetError> {
        self.hw.lock().disable_irq()?;
        self.irq_enabled = false;
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn take_irq_endpoint(&mut self) -> Option<rd_net::BIrqEndpoint> {
        let irq_port = self.irq_port.take()?;
        Some(Box::new(FxmacIrqHandler {
            _registers: Arc::clone(&self.hw.lock()._registers),
            irq_port,
            irq_epoch: Arc::clone(&self.irq_epoch),
        }))
    }

    fn service_irq_event(&mut self, event: Event) -> Result<(), NetError> {
        let mut hw = self.hw.lock();
        let device = hw.device_mut().ok_or_else(fxmac_not_ready)?;
        device.service_irq_status(FXmacIrqStatus::from_raw(event.device_status as u32));
        Ok(())
    }

    fn rearm_irq_source(&mut self, source: MaskedSource) -> Result<(), NetError> {
        let mut hw = self.hw.lock();
        self.irq_epoch.finish_masked_source(source)?;
        hw.device_mut().ok_or_else(fxmac_not_ready)?.enable_irq();
        Ok(())
    }
}

struct FxmacHw {
    _registers: Arc<mmio_api::Mmio>,
    state: FxmacHwState,
}

unsafe impl Send for FxmacHw {}

enum FxmacHwState {
    Pending(FXmacPending),
    Initializing(FXmacInitialization),
    Transitioning,
    Ready(FXmac),
}

enum FxmacOwnerInitProgress {
    Ready([u8; 6]),
    Pending(FXmacInitSchedule),
    Failed(fxmac_rs::FXmacInitError),
}

impl FxmacHw {
    fn is_ready(&self) -> bool {
        matches!(self.state, FxmacHwState::Ready(_))
    }

    fn device_mut(&mut self) -> Option<&mut FXmac> {
        match &mut self.state {
            FxmacHwState::Ready(device) => Some(device),
            FxmacHwState::Pending(_)
            | FxmacHwState::Initializing(_)
            | FxmacHwState::Transitioning => None,
        }
    }

    fn disable_irq(&mut self) -> Result<(), NetError> {
        match &mut self.state {
            FxmacHwState::Pending(pending) => pending.disable_irq(),
            FxmacHwState::Ready(device) => device.disable_irq(),
            FxmacHwState::Initializing(_) | FxmacHwState::Transitioning => {
                return Err(fxmac_not_ready());
            }
        }
        Ok(())
    }

    fn poll_initialization(&mut self, now_ns: u64) -> FxmacOwnerInitProgress {
        let state = core::mem::replace(&mut self.state, FxmacHwState::Transitioning);
        let mut initialization = match state {
            FxmacHwState::Pending(pending) => begin_xmac_init(pending),
            FxmacHwState::Initializing(initialization) => initialization,
            FxmacHwState::Ready(device) => {
                let hwaddr = device.config.mac;
                self.state = FxmacHwState::Ready(device);
                return FxmacOwnerInitProgress::Ready(hwaddr);
            }
            FxmacHwState::Transitioning => {
                return FxmacOwnerInitProgress::Failed(fxmac_rs::FXmacInitError::AlreadyFinished);
            }
        };

        match poll_xmac_init(&mut initialization, now_ns) {
            FXmacInitPoll::Ready => {
                let device = initialization
                    .take_ready()
                    .expect("ready FXMAC initialization retained its controller");
                let hwaddr = device.config.mac;
                self.state = FxmacHwState::Ready(device);
                FxmacOwnerInitProgress::Ready(hwaddr)
            }
            FXmacInitPoll::Pending(schedule) => {
                self.state = FxmacHwState::Initializing(initialization);
                FxmacOwnerInitProgress::Pending(schedule)
            }
            FXmacInitPoll::Failed(error) => {
                // The runtime quarantines a failed owner session. Retain the
                // complete initialization object so DMA/MMIO ownership cannot
                // be dropped while hardware quiescence is unproven.
                self.state = FxmacHwState::Initializing(initialization);
                FxmacOwnerInitProgress::Failed(error)
            }
        }
    }
}

fn fxmac_not_ready() -> NetError {
    NetError::Other(Box::new(rd_net::KError::Unknown(
        "FXMAC controller is not ready",
    )))
}

struct FxmacTxState {
    tx_done: VecDeque<u64>,
}

struct FxmacRxState {
    rx_buffers: VecDeque<RuntimeNetBuffer>,
    rx_packets: VecDeque<Vec<u8>>,
}

struct FxmacIrqEpoch {
    next_generation: AtomicU64,
    masked_generation: AtomicU64,
}

impl FxmacIrqEpoch {
    fn new() -> Self {
        Self {
            next_generation: AtomicU64::new(1),
            masked_generation: AtomicU64::new(0),
        }
    }

    fn is_masked(&self) -> bool {
        self.masked_generation.load(Ordering::Acquire) != 0
    }

    fn begin_capture_epoch(&self) -> Result<MaskedSource, EthernetIrqFault> {
        // This endpoint is non-reentrant on one fixed CPU. The zero value can
        // only be observed after a full u64 wrap; skip it because zero is the
        // explicit "no masked source" state.
        let mut generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        if generation == 0 {
            generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        }
        self.masked_generation
            .compare_exchange(0, generation, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| EthernetIrqFault::Containment)?;
        MaskedSource::try_new(generation, u64::from(FXMAC_RUNTIME_IRQ_MASK))
            .map_err(|_| EthernetIrqFault::Containment)
    }

    fn containment_source(&self) -> Result<MaskedSource, EthernetIrqFault> {
        let active = self.masked_generation.load(Ordering::Acquire);
        if active != 0 {
            return MaskedSource::try_new(active, u64::from(FXMAC_RUNTIME_IRQ_MASK))
                .map_err(|_| EthernetIrqFault::Containment);
        }
        self.begin_capture_epoch()
    }

    fn finish_masked_source(&self, source: MaskedSource) -> Result<(), NetError> {
        let generation = source.generation().get();
        if source.bitmap().get() != u64::from(FXMAC_RUNTIME_IRQ_MASK)
            || self
                .masked_generation
                .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(NetError::Other(Box::new(rd_net::KError::Unknown(
                "stale FXMAC IRQ source",
            ))));
        }
        Ok(())
    }
}

struct FxmacIrqHandler {
    _registers: Arc<mmio_api::Mmio>,
    irq_port: FXmacIrqPort,
    irq_epoch: Arc<FxmacIrqEpoch>,
}

impl InterfaceIrqEndpoint for FxmacIrqHandler {
    type Event = Event;
    type Fault = EthernetIrqFault;

    fn capture(&mut self) -> IrqCapture<Event, EthernetIrqFault> {
        // An active epoch already owns the masked source. Its captured facts
        // are in the owner mailbox, so another callback must not read/W1C the
        // same hardware state or publish the linear token again.
        if self.irq_epoch.is_masked() {
            return IrqCapture::Unhandled;
        }
        let status = self.irq_port.capture_and_mask();
        if status.is_empty() {
            return IrqCapture::Unhandled;
        }
        let event = event_from_status(status);
        match self.irq_epoch.begin_capture_epoch() {
            Ok(masked) => IrqCapture::Captured {
                event,
                masked: Some(masked),
            },
            Err(reason) => {
                let containment = self
                    .irq_epoch
                    .containment_source()
                    .map(rd_net::FaultContainment::DeviceSourceMasked)
                    .unwrap_or(rd_net::FaultContainment::Uncontained);
                IrqCapture::Fault {
                    reason,
                    containment,
                }
            }
        }
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, EthernetIrqFault> {
        self.irq_port.mask();
        self.irq_epoch.containment_source()
    }
}

fn event_from_status(status: FXmacIrqStatus) -> Event {
    let mut event = Event::none();
    if status.tx_ready() {
        event.tx_queue.insert(QUEUE_ID);
    }
    if status.rx_ready() {
        event.rx_queue.insert(QUEUE_ID);
    }
    event.device_status = u64::from(status.raw());
    event
}

#[derive(Clone, Copy)]
struct RuntimeNetBuffer {
    virt: usize,
    bus_addr: u64,
    len: usize,
}

impl From<DmaBuffer> for RuntimeNetBuffer {
    fn from(buffer: DmaBuffer) -> Self {
        Self {
            virt: buffer.virt.as_ptr() as usize,
            bus_addr: buffer.bus_addr,
            len: buffer.len,
        }
    }
}

struct FxmacTxQueue {
    hw: Arc<Mutex<FxmacHw>>,
    tx_state: Arc<Mutex<FxmacTxState>>,
}

impl ITxQueue for FxmacTxQueue {
    fn id(&self) -> usize {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        fxmac_queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        let packet = unsafe { core::slice::from_raw_parts(buffer.virt.as_ptr(), buffer.len) };
        let mut hw = self.hw.lock();
        let ret = FXmacLwipPortTx(
            hw.device_mut().ok_or_else(fxmac_not_ready)?,
            vec![packet.to_vec()],
        );
        if ret < 0 {
            return Err(NetError::Retry);
        }
        drop(hw);
        self.tx_state.lock().tx_done.push_back(buffer.bus_addr);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        self.tx_state.lock().tx_done.pop_front()
    }
}

struct FxmacRxQueue {
    hw: Arc<Mutex<FxmacHw>>,
    rx_state: Arc<Mutex<FxmacRxState>>,
}

impl IRxQueue for FxmacRxQueue {
    fn id(&self) -> usize {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        fxmac_queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        self.rx_state.lock().rx_buffers.push_back(buffer.into());
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        let mut rx_state = self.rx_state.lock();
        if rx_state.rx_buffers.is_empty() {
            return None;
        }

        let mut hw = self.hw.lock();
        if rx_state.rx_packets.is_empty()
            && let Some(packets) = FXmacRecvHandler(hw.device_mut()?)
        {
            rx_state.rx_packets.extend(packets);
        }
        drop(hw);

        let packet = rx_state.rx_packets.pop_front()?;
        let buffer = rx_state.rx_buffers.pop_front()?;
        let len = cmp::min(packet.len(), buffer.len);
        unsafe {
            core::ptr::copy_nonoverlapping(packet.as_ptr(), buffer.virt as *mut u8, len);
        }
        Some((buffer.bus_addr, len))
    }
}

fn fxmac_queue_config() -> QueueConfig {
    QueueConfig {
        dma_mask: DMA_MASK,
        align: DMA_ALIGN,
        buf_size: BUFFER_SIZE,
        ring_size: QUEUE_SIZE,
        memory_mode: QueueMemoryMode::OwnerCopy,
    }
}

struct FxmacKernelFunc;

const _: FxmacKernelFunc = FxmacKernelFunc;

#[ax_crate_interface::impl_interface]
impl fxmac_rs::KernelFunc for FxmacKernelFunc {
    fn virt_to_phys(addr: usize) -> usize {
        axklib::mem::virt_to_phys(addr.into()).as_usize()
    }

    fn dma_alloc_coherent(pages: usize) -> (usize, usize) {
        let Some(size) = pages.checked_mul(PAGE_SIZE) else {
            log::error!("FXmac DMA allocation size overflow: {pages} pages");
            return (0, 0);
        };
        let Ok(layout) = Layout::from_size_align(size.max(1), DMA_ALIGN) else {
            log::error!("FXmac DMA allocation layout is invalid: {size} bytes");
            return (0, 0);
        };
        let Some(handle) =
            (unsafe { axklib::dma::op().alloc_coherent(DmaConstraints::new(DMA_MASK), layout) })
        else {
            log::error!("FXmac DMA allocation failed: {pages} pages");
            return (0, 0);
        };
        (
            handle.as_ptr().as_ptr() as usize,
            handle.dma_addr().as_u64() as usize,
        )
    }

    fn dma_free_coherent(vaddr: usize, pages: usize) {
        let Some(size) = pages.checked_mul(PAGE_SIZE) else {
            log::error!("FXmac DMA free size overflow: {pages} pages");
            return;
        };
        let Ok(layout) = Layout::from_size_align(size.max(1), DMA_ALIGN) else {
            log::error!("FXmac DMA free layout is invalid: {size} bytes");
            return;
        };
        let Some(vaddr) = core::ptr::NonNull::new(vaddr as *mut u8) else {
            return;
        };
        let paddr = axklib::mem::virt_to_phys((vaddr.as_ptr() as usize).into()).as_usize();
        let handle = unsafe { DmaAllocHandle::new(vaddr, DmaAddr::from(paddr as u64), layout) };
        unsafe { axklib::dma::op().dealloc_coherent(handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_irq_status_does_not_publish_queue_work() {
        let event = event_from_status(FXmacIrqStatus::from_raw(0));

        assert!(!event.tx_queue.contains(QUEUE_ID));
        assert!(!event.rx_queue.contains(QUEUE_ID));
    }

    #[test]
    fn irq_status_publishes_only_reported_queues() {
        let tx_event = event_from_status(FXmacIrqStatus::from_raw(1 << 7));
        assert!(tx_event.tx_queue.contains(QUEUE_ID));
        assert!(!tx_event.rx_queue.contains(QUEUE_ID));

        let rx_event = event_from_status(FXmacIrqStatus::from_raw(1 << 1));
        assert!(!rx_event.tx_queue.contains(QUEUE_ID));
        assert!(rx_event.rx_queue.contains(QUEUE_ID));
    }

    #[test]
    fn masked_source_rearm_rejects_stale_generation() {
        let state = FxmacIrqEpoch::new();
        let source = state.begin_capture_epoch().unwrap();

        state.finish_masked_source(source).unwrap();
        assert!(state.finish_masked_source(source).is_err());
    }

    #[test]
    fn active_capture_epoch_is_linear_but_containment_is_idempotent() {
        let state = FxmacIrqEpoch::new();
        let source = state.begin_capture_epoch().unwrap();

        assert!(state.is_masked());
        assert!(state.begin_capture_epoch().is_err());
        assert_eq!(state.containment_source().unwrap(), source);
    }
}
