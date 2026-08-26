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
use rd_net::{
    DmaBuffer, FixedNetControl, IRxQueue, ITxQueue, NetDevice, NetDeviceInfo, NetDeviceParts,
    NetError, NetHardIrqEndpoint, NetHardIrqHandler, NetHardIrqResult, NetIrqSnapshot,
    NetIrqSourceId, NetPollGroupId, NetPollGroupParts, NetPollIrqControl, NetQueueId,
    NetQueuePairParts, NetRearmResult, QueueConfig, RxCompletion, SubmitError,
};
use rdrive::{DriverGeneric, probe::fdt::ResourcePrepareConfig};

use crate::{binding_info_from_fdt, net::PlatformDeviceNet};

pub const DEVICE_NAME: &str = "fxmac";

const DRIVER_NAME: &str = "cdns,phytium-gem-1.0";
const QUEUE_ID: NetQueueId = NetQueueId::new(0);
const GROUP_ID: NetPollGroupId = NetPollGroupId::new(0);
const IRQ_SOURCE: NetIrqSourceId = NetIrqSourceId::new(0);
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
        .register_net_with_info(DRIVER_NAME, dev, dma, info)?;
    log::info!("registered FXmac FDT network device");
    Ok(())
}

struct FxmacNet {
    hw: Arc<Mutex<FxmacHw>>,
    tx_state: Arc<Mutex<FxmacTxState>>,
    rx_state: Arc<Mutex<FxmacRxState>>,
    irq_state: Arc<FxmacIrqState>,
    irq_endpoint: FxmacIrqEndpoint,
    hwaddr: [u8; 6],
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
            irq_endpoint,
            hwaddr,
        })
    }
}

impl DriverGeneric for FxmacNet {
    fn name(&self) -> &str {
        DRIVER_NAME
    }
}

impl NetDevice for FxmacNet {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError> {
        let Self {
            hw,
            tx_state,
            rx_state,
            irq_state,
            irq_endpoint,
            hwaddr,
        } = *self;

        Ok(NetDeviceParts {
            info: NetDeviceInfo::new(DRIVER_NAME, hwaddr),
            control: Box::new(FixedNetControl::new(hwaddr)),
            wifi_control: None,
            poll_groups: vec![NetPollGroupParts {
                id: GROUP_ID,
                queues: NetQueuePairParts {
                    tx: Box::new(FxmacTxQueue {
                        hw: Arc::clone(&hw),
                        tx_state,
                        irq_state: Arc::clone(&irq_state),
                    }),
                    rx: Box::new(FxmacRxQueue {
                        hw: Arc::clone(&hw),
                        rx_state,
                        irq_state: Arc::clone(&irq_state),
                    }),
                },
                irq_control: Box::new(FxmacIrqControl {
                    hw: Arc::clone(&hw),
                    irq_state: Arc::clone(&irq_state),
                }),
                owner_startup: None,
                irq_endpoints: vec![NetHardIrqEndpoint::new(
                    IRQ_SOURCE,
                    Box::new(FxmacIrqHandler {
                        endpoint: irq_endpoint,
                        irq_state,
                    }),
                )],
            }],
        })
    }
}

struct FxmacHw {
    device: FXmac,
}

unsafe impl Send for FxmacHw {}

struct FxmacTxState {
    tx_done: VecDeque<DmaBuffer>,
}

struct FxmacRxState {
    rx_buffers: VecDeque<DmaBuffer>,
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

    fn drain_pending_irq(&self, hw: &mut FxmacHw) -> NetIrqSnapshot {
        let status = self.take_pending_status();
        if status.is_empty() {
            return NetIrqSnapshot::empty();
        }
        hw.device.process_irq_snapshot(status);
        Self::snapshot(status)
    }

    fn publish(&self, status: FXmacIrqStatus) -> NetIrqSnapshot {
        if status.is_empty() {
            return NetIrqSnapshot::empty();
        }
        self.pending_status
            .fetch_or(status.raw(), Ordering::Release);
        Self::snapshot(status)
    }

    fn snapshot(status: FXmacIrqStatus) -> NetIrqSnapshot {
        let mut snapshot = NetIrqSnapshot::empty();
        if status.tx_ready() {
            snapshot = snapshot.union(NetIrqSnapshot::TX);
        }
        if status.rx_ready() {
            snapshot = snapshot.union(NetIrqSnapshot::RX);
        }
        if snapshot == NetIrqSnapshot::empty() {
            NetIrqSnapshot::ERROR
        } else {
            snapshot
        }
    }

    fn pending_snapshot(&self) -> NetIrqSnapshot {
        Self::snapshot(FXmacIrqStatus::from_raw(
            self.pending_status.load(Ordering::Acquire),
        ))
    }

    fn take_pending_status(&self) -> FXmacIrqStatus {
        FXmacIrqStatus::from_raw(self.pending_status.swap(0, Ordering::AcqRel))
    }
}

struct FxmacIrqHandler {
    endpoint: FxmacIrqEndpoint,
    irq_state: Arc<FxmacIrqState>,
}

impl NetHardIrqHandler for FxmacIrqHandler {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        let status = self.endpoint.snapshot_and_mask();
        if status.is_empty() {
            return NetHardIrqResult::Spurious;
        }
        NetHardIrqResult::Schedule(self.irq_state.publish(status))
    }
}

struct FxmacIrqControl {
    hw: Arc<Mutex<FxmacHw>>,
    irq_state: Arc<FxmacIrqState>,
}

impl NetPollIrqControl for FxmacIrqControl {
    fn quiesce(&mut self) -> Result<(), NetError> {
        // SAFETY: lifecycle calls are serialized on the group owner CPU.
        unsafe { self.hw.lock_raw() }.device.disable_irq();
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), NetError> {
        // The current FXMAC core exposes no DMA-idle completion proof. Keep
        // every descriptor and buffer quarantined after masking the source.
        unsafe { self.hw.lock_raw() }.device.disable_irq();
        Err(NetError::DmaShutdownUnconfirmed)
    }

    fn rearm_and_check(&mut self) -> Result<NetRearmResult, NetError> {
        // SAFETY: queue polling and rearm share one non-reentrant owner.
        let mut hw = unsafe { self.hw.lock_raw() };
        let mut pending = self.irq_state.drain_pending_irq(&mut hw);
        hw.device.enable_irq();
        let status = hw.device.handle_irq();
        pending = pending
            .union(self.irq_state.publish(status))
            .union(self.irq_state.pending_snapshot());
        if pending == NetIrqSnapshot::empty() {
            Ok(NetRearmResult::Idle)
        } else {
            hw.device.disable_irq();
            Ok(NetRearmResult::WorkPending(pending))
        }
    }
}

struct FxmacTxQueue {
    hw: Arc<Mutex<FxmacHw>>,
    tx_state: Arc<Mutex<FxmacTxState>>,
    irq_state: Arc<FxmacIrqState>,
}

impl ITxQueue for FxmacTxQueue {
    fn id(&self) -> NetQueueId {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        fxmac_queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        let packet = buffer.read_with_cpu(buffer.len(), |packet| packet.to_vec());
        // SAFETY: TX submission is serialized against local device re-entry.
        let mut hw = unsafe { self.hw.lock_raw() };
        let _ = self.irq_state.drain_pending_irq(&mut hw);
        let ret = FXmacLwipPortTx(&mut hw.device, vec![packet]);
        let _ = self.irq_state.drain_pending_irq(&mut hw);
        if ret < 0 {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }
        drop(hw);
        // SAFETY: the queue path excludes local re-entry while publishing completion.
        unsafe { self.tx_state.lock_raw() }
            .tx_done
            .push_back(buffer);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<DmaBuffer> {
        // SAFETY: completion processing is serialized by the queue owner.
        let mut hw = unsafe { self.hw.lock_raw() };
        let _ = self.irq_state.drain_pending_irq(&mut hw);
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
    fn id(&self) -> NetQueueId {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        fxmac_queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        // SAFETY: RX buffer installation is serialized by the queue owner.
        unsafe { self.rx_state.lock_raw() }
            .rx_buffers
            .push_back(buffer);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<RxCompletion> {
        // SAFETY: RX dequeue runs in the queue owner's non-reentrant context.
        let mut rx_state = unsafe { self.rx_state.lock_raw() };
        if rx_state.rx_buffers.is_empty() {
            return None;
        }

        // SAFETY: RX polling excludes local device re-entry.
        let mut hw = unsafe { self.hw.lock_raw() };
        let pending = self.irq_state.drain_pending_irq(&mut hw);
        if (pending.contains(NetIrqSnapshot::RX) || rx_state.rx_packets.is_empty())
            && let Some(packets) = FXmacRecvHandler(&mut hw.device)
        {
            rx_state.rx_packets.extend(packets);
        }
        let _ = self.irq_state.drain_pending_irq(&mut hw);
        drop(hw);

        let packet = rx_state.rx_packets.pop_front()?;
        let mut buffer = rx_state.rx_buffers.pop_front()?;
        let len = cmp::min(packet.len(), buffer.capacity());
        buffer.write_with_cpu(|target| target[..len].copy_from_slice(&packet[..len]));
        Some(RxCompletion {
            buffer,
            packet_len: len,
        })
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
        let snapshot = state.publish(FXmacIrqStatus::from_raw(0));

        assert_eq!(snapshot, NetIrqSnapshot::empty());
        assert!(state.take_pending_status().is_empty());
    }

    #[test]
    fn irq_state_publishes_only_reported_queues() {
        let state = FxmacIrqState::new();

        let tx_status = FXmacIrqStatus::from_raw(1 << 7);
        let tx_event = state.publish(tx_status);
        assert!(tx_event.contains(NetIrqSnapshot::TX));
        assert!(!tx_event.contains(NetIrqSnapshot::RX));
        assert_eq!(state.take_pending_status(), tx_status);

        let rx_status = FXmacIrqStatus::from_raw(1 << 1);
        let rx_event = state.publish(rx_status);
        assert!(!rx_event.contains(NetIrqSnapshot::TX));
        assert!(rx_event.contains(NetIrqSnapshot::RX));
        assert_eq!(state.take_pending_status(), rx_status);
    }

    #[test]
    fn control_status_schedules_the_deferred_owner() {
        let state = FxmacIrqState::new();
        let status = FXmacIrqStatus::from_raw(1 << 9);

        let snapshot = state.publish(status);

        assert_eq!(snapshot, NetIrqSnapshot::ERROR);
        assert_eq!(state.take_pending_status(), status);
    }

    #[test]
    fn irq_state_coalesces_raw_status_for_the_deferred_owner() {
        let state = FxmacIrqState::new();

        let _ = state.publish(FXmacIrqStatus::from_raw(0x20));
        let _ = state.publish(FXmacIrqStatus::from_raw(0x80));

        assert_eq!(state.take_pending_status().raw(), 0xa0);
        assert!(state.take_pending_status().is_empty());
    }
}
