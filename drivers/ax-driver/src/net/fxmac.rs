use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec, vec::Vec};
use core::{
    cmp,
    num::NonZeroU32,
    sync::atomic::{AtomicU32, Ordering},
};

use ax_sync::SpinLock as Mutex;
use dma_api::DeviceDma;
use fxmac_rs::{
    FXMAC_MMIO_REQUIRED_SIZE, FXmac, FXmacIrqStatus, FXmacLwipPortTx, FXmacRecvHandler,
    FxmacHardwareConfig, FxmacIrqEndpoint, xmac_init,
};
use mmio_api::Mmio;
use rd_net::{DmaBuffer, Event, IRxQueue, ITxQueue, NetError, QueueConfig};
use rdrive::{DriverGeneric, probe::fdt::ResourcePrepareConfig};

use crate::{binding_info_from_fdt, net::PlatformDeviceNet};

pub const DEVICE_NAME: &str = "fxmac";

const DRIVER_NAME: &str = "cdns,phytium-gem-1.0";
const QUEUE_ID: usize = 0;
const QUEUE_SIZE: usize = 64;
const BUFFER_SIZE: usize = 2048;
const DMA_ALIGN: usize = 0x1000;
const DMA_MASK: u64 = u64::MAX;

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
    let probe_info = probe.info();
    let info = binding_info_from_fdt(probe_info)?;
    if info.irq().is_none() {
        return Err(rdrive::probe::OnProbeError::other(alloc::format!(
            "[{}] FXMAC requires an interrupt binding",
            probe_info.node.name()
        )));
    }
    let reg = probe_info.node.regs().into_iter().next().ok_or_else(|| {
        rdrive::probe::OnProbeError::other(alloc::format!(
            "[{}] has no FXMAC register aperture",
            probe_info.node.name()
        ))
    })?;
    let mmio_size = reg.size.ok_or_else(|| {
        rdrive::probe::OnProbeError::other(alloc::format!(
            "[{}] FXMAC register aperture has no size",
            probe_info.node.name()
        ))
    })?;
    let mmio_size = usize::try_from(mmio_size).map_err(|_| {
        rdrive::probe::OnProbeError::other(alloc::format!(
            "[{}] FXMAC register aperture is too large: {mmio_size:#x}",
            probe_info.node.name()
        ))
    })?;
    if mmio_size < FXMAC_MMIO_REQUIRED_SIZE {
        return Err(rdrive::probe::OnProbeError::other(alloc::format!(
            "[{}] FXMAC register aperture is too small: {mmio_size:#x} < \
             {FXMAC_MMIO_REQUIRED_SIZE:#x}",
            probe_info.node.name()
        )));
    }
    let mmio_address = usize::try_from(reg.address).map_err(|_| {
        rdrive::probe::OnProbeError::other(alloc::format!(
            "[{}] FXMAC register address is not representable: {:#x}",
            probe_info.node.name(),
            reg.address
        ))
    })?;
    let mmio = axklib::mmio::ioremap(mmio_address.into(), mmio_size).map_err(|err| {
        rdrive::probe::OnProbeError::other(alloc::format!(
            "failed to map FXMAC registers at {:#x}: {err}",
            reg.address
        ))
    })?;
    let resources = probe_info
        .prepare_resources(ResourcePrepareConfig::default().with_named_clock_rate("pclk"))?;
    let pclk_hz = resources.clock_rate("pclk").ok_or_else(|| {
        rdrive::probe::OnProbeError::other(alloc::format!(
            "[{}] has no prepared FXMAC pclk rate",
            probe_info.node.name()
        ))
    })?;
    let pclk_hz = u32::try_from(pclk_hz).map_err(|_| {
        rdrive::probe::OnProbeError::other(alloc::format!(
            "[{}] FXMAC pclk rate is out of range: {pclk_hz} Hz",
            probe_info.node.name()
        ))
    })?;
    let pclk_hz = NonZeroU32::new(pclk_hz).ok_or_else(|| {
        rdrive::probe::OnProbeError::other(alloc::format!(
            "[{}] FXMAC pclk rate is zero",
            probe_info.node.name()
        ))
    })?;
    let hardware = FxmacHardwareConfig::new(pclk_hz);
    let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        crate::binding_resolver::dma_coherency_from_fdt(probe_info),
        dma_api::DmaConstraints::new(u64::MAX),
    ));
    let dev = FxmacNet::new(dma.clone(), mmio, hardware)
        .map_err(|err| rdrive::probe::OnProbeError::Other(alloc::format!("{err}").into()))?;
    probe
        .into_platform_device()
        .register_net_with_info(DRIVER_NAME, dev, dma, info);
    log::info!("registered FXmac FDT network device");
    Ok(())
}

struct FxmacNet {
    hw: Arc<Mutex<FxmacHw>>,
    tx_state: Arc<Mutex<FxmacTxState>>,
    rx_state: Arc<Mutex<FxmacRxState>>,
    irq_state: Arc<FxmacIrqState>,
    irq_endpoint: Option<FxmacIrqEndpoint>,
    hwaddr: [u8; 6],
    tx_created: bool,
    rx_created: bool,
    irq_enabled: bool,
}

impl FxmacNet {
    fn new(
        dma: DeviceDma,
        mmio: Mmio,
        hardware: FxmacHardwareConfig,
    ) -> Result<Self, fxmac_rs::FxmacInitError> {
        let (device, irq_endpoint) = xmac_init(dma, mmio, hardware)?;
        let hwaddr = device.mac_address();
        Ok(Self {
            hw: Arc::new(Mutex::new(FxmacHw { device })),
            tx_state: Arc::new(Mutex::new(FxmacTxState {
                tx_done: VecDeque::with_capacity(QUEUE_SIZE),
            })),
            rx_state: Arc::new(Mutex::new(FxmacRxState {
                rx_buffers: VecDeque::with_capacity(QUEUE_SIZE),
                rx_packets: VecDeque::with_capacity(QUEUE_SIZE),
            })),
            irq_state: Arc::new(FxmacIrqState::new()),
            irq_endpoint: Some(irq_endpoint),
            hwaddr,
            tx_created: false,
            rx_created: false,
            irq_enabled: false,
        })
    }
}

impl DriverGeneric for FxmacNet {
    fn name(&self) -> &str {
        DRIVER_NAME
    }
}

impl rd_net::Interface for FxmacNet {
    fn mac_address(&self) -> [u8; 6] {
        self.hwaddr
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        if self.tx_created {
            return None;
        }
        self.tx_created = true;
        Some(Box::new(FxmacTxQueue {
            hw: Arc::clone(&self.hw),
            tx_state: Arc::clone(&self.tx_state),
            irq_state: Arc::clone(&self.irq_state),
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if self.rx_created {
            return None;
        }
        self.rx_created = true;
        Some(Box::new(FxmacRxQueue {
            hw: Arc::clone(&self.hw),
            rx_state: Arc::clone(&self.rx_state),
            irq_state: Arc::clone(&self.irq_state),
        }))
    }

    fn enable_irq(&mut self) {
        // SAFETY: device IRQ setup is serialized before the handler is exposed.
        unsafe { self.hw.lock_raw() }.device.enable_irq();
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        // SAFETY: device teardown excludes handler re-entry.
        unsafe { self.hw.lock_raw() }.device.disable_irq();
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        // SAFETY: this direct-polling adapter is used only outside the runtime
        // owned IRQ path and has exclusive access to the interface.
        let status = unsafe { self.hw.lock_raw() }.device.handle_irq();
        self.irq_state.publish(status)
    }

    fn take_irq_handler(&mut self) -> Option<rd_net::BIrqHandler> {
        self.irq_endpoint.take().map(|endpoint| {
            Box::new(FxmacIrqHandler {
                endpoint,
                irq_state: Arc::clone(&self.irq_state),
            }) as rd_net::BIrqHandler
        })
    }
}

struct FxmacHw {
    device: FXmac,
}

unsafe impl Send for FxmacHw {}

struct FxmacTxState {
    tx_done: VecDeque<u64>,
}

struct FxmacRxState {
    rx_buffers: VecDeque<RuntimeNetBuffer>,
    rx_packets: VecDeque<Vec<u8>>,
}

struct FxmacIrqState {
    pending_status: AtomicU32,
}

impl FxmacIrqState {
    fn new() -> Self {
        Self {
            pending_status: AtomicU32::new(0),
        }
    }

    fn drain_pending_irq(&self, hw: &mut FxmacHw) {
        let status = self.take_pending_status();
        if !status.is_empty() {
            hw.device.handle_deferred_irq(status);
        }
    }

    fn publish(&self, status: FXmacIrqStatus) -> Event {
        if status.is_empty() {
            return Event::none();
        }
        self.pending_status
            .fetch_or(status.raw(), Ordering::Release);

        let mut event = Event::none();
        if status.tx_ready() {
            event.tx_queue.insert(QUEUE_ID);
        }
        if status.rx_ready() {
            event.rx_queue.insert(QUEUE_ID);
        }
        if !status.tx_ready() && !status.rx_ready() {
            event.rx_queue.insert(QUEUE_ID);
        }
        event
    }

    fn take_pending_status(&self) -> FXmacIrqStatus {
        FXmacIrqStatus::from_raw(self.pending_status.swap(0, Ordering::AcqRel))
    }
}

struct FxmacIrqHandler {
    endpoint: FxmacIrqEndpoint,
    irq_state: Arc<FxmacIrqState>,
}

impl rd_net::InterfaceIrqHandler for FxmacIrqHandler {
    fn handle_irq(&mut self) -> Event {
        self.irq_state.publish(self.endpoint.snapshot_and_mask())
    }
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
    irq_state: Arc<FxmacIrqState>,
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
        // SAFETY: TX submission is serialized against local device re-entry.
        let mut hw = unsafe { self.hw.lock_raw() };
        self.irq_state.drain_pending_irq(&mut hw);
        let ret = FXmacLwipPortTx(&mut hw.device, vec![packet.to_vec()]);
        self.irq_state.drain_pending_irq(&mut hw);
        if ret < 0 {
            return Err(NetError::Retry);
        }
        drop(hw);
        // SAFETY: the queue path excludes local re-entry while publishing completion.
        unsafe { self.tx_state.lock_raw() }
            .tx_done
            .push_back(buffer.bus_addr);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        // SAFETY: completion processing is serialized by the queue owner.
        let mut hw = unsafe { self.hw.lock_raw() };
        self.irq_state.drain_pending_irq(&mut hw);
        drop(hw);
        // SAFETY: the queue consumer excludes local re-entry.
        unsafe { self.tx_state.lock_raw() }.tx_done.pop_front()
    }
}

struct FxmacRxQueue {
    hw: Arc<Mutex<FxmacHw>>,
    rx_state: Arc<Mutex<FxmacRxState>>,
    irq_state: Arc<FxmacIrqState>,
}

impl IRxQueue for FxmacRxQueue {
    fn id(&self) -> usize {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        fxmac_queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        // SAFETY: RX buffer installation is serialized by the queue owner.
        unsafe { self.rx_state.lock_raw() }
            .rx_buffers
            .push_back(buffer.into());
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        // SAFETY: RX polling excludes local device re-entry.
        let mut hw = unsafe { self.hw.lock_raw() };
        self.irq_state.drain_pending_irq(&mut hw);

        // SAFETY: RX dequeue runs in the queue owner's non-reentrant context.
        let mut rx_state = unsafe { self.rx_state.lock_raw() };
        if rx_state.rx_buffers.is_empty() {
            return None;
        }
        if rx_state.rx_packets.is_empty()
            && let Some(packets) = FXmacRecvHandler(&mut hw.device)
        {
            rx_state.rx_packets.extend(packets);
        }
        self.irq_state.drain_pending_irq(&mut hw);
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irq_state_does_not_publish_empty_snapshot() {
        let state = FxmacIrqState::new();
        let event = state.publish(FXmacIrqStatus::from_raw(0));

        assert!(!event.tx_queue.contains(QUEUE_ID));
        assert!(!event.rx_queue.contains(QUEUE_ID));
        assert!(state.take_pending_status().is_empty());
    }

    #[test]
    fn irq_state_publishes_only_reported_queues() {
        let state = FxmacIrqState::new();

        let tx_status = FXmacIrqStatus::from_raw(1 << 7);
        let tx_event = state.publish(tx_status);
        assert!(tx_event.tx_queue.contains(QUEUE_ID));
        assert!(!tx_event.rx_queue.contains(QUEUE_ID));
        assert_eq!(state.take_pending_status(), tx_status);

        let rx_status = FXmacIrqStatus::from_raw(1 << 1);
        let rx_event = state.publish(rx_status);
        assert!(!rx_event.tx_queue.contains(QUEUE_ID));
        assert!(rx_event.rx_queue.contains(QUEUE_ID));
        assert_eq!(state.take_pending_status(), rx_status);
    }

    #[test]
    fn irq_state_routes_control_status_to_the_deferred_owner() {
        let state = FxmacIrqState::new();
        let link_status = FXmacIrqStatus::from_raw(1 << 9);

        let event = state.publish(link_status);

        assert!(!event.tx_queue.contains(QUEUE_ID));
        assert!(event.rx_queue.contains(QUEUE_ID));
        assert_eq!(state.take_pending_status(), link_status);
    }

    #[test]
    fn irq_state_coalesces_raw_status_for_the_deferred_owner() {
        let state = FxmacIrqState::new();
        let first = fxmac_rs::FXmacIrqStatus::from_raw(0x20);
        let second = fxmac_rs::FXmacIrqStatus::from_raw(0x80);

        let _ = state.publish(first);
        let _ = state.publish(second);

        assert_eq!(state.take_pending_status().raw(), 0xa0);
        assert!(state.take_pending_status().is_empty());
    }
}
