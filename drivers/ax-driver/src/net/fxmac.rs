use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec, vec::Vec};
use core::{
    alloc::Layout,
    cmp,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_sync::SpinLock as Mutex;
use dma_api::{DmaAddr, DmaAllocHandle, DmaConstraints, DmaOp};
use fxmac_rs::{FXmac, FXmacGetMacAddress, FXmacLwipPortTx, FXmacRecvHandler, xmac_init};
use rd_net::{
    DmaBuffer, FixedNetControl, IRxQueue, ITxQueue, NetDevice, NetDeviceInfo, NetDeviceParts,
    NetError, NetHardIrqEndpoint, NetHardIrqHandler, NetHardIrqResult, NetIrqSnapshot,
    NetIrqSourceId, NetPollGroupId, NetPollGroupParts, NetPollIrqControl, NetQueueId,
    NetQueuePairParts, NetRearmResult, QueueConfig, RxCompletion, SubmitError,
};
use rdrive::{DriverGeneric, PlatformDevice};

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
    let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        crate::binding_resolver::dma_coherency_from_fdt(probe.info()),
        dma_api::DmaConstraints::new(u64::MAX),
    ));
    let info = binding_info_from_fdt(probe.info())?;
    let dev = FxmacNet::new();
    probe
        .into_platform_device()
        .register_net_with_info(DRIVER_NAME, dev, dma, info);
    log::info!("registered FXmac FDT network device");
    Ok(())
}

pub fn register(plat_dev: PlatformDevice) {
    let dev = FxmacNet::new();
    let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        dma_api::DmaCoherency::NonCoherent,
        dma_api::DmaConstraints::new(u64::MAX),
    ));
    plat_dev.register_net(DRIVER_NAME, dev, dma);
    log::info!("registered FXmac network device");
}

struct FxmacNet {
    hw: Arc<Mutex<FxmacHw>>,
    tx_state: Arc<Mutex<FxmacTxState>>,
    rx_state: Arc<Mutex<FxmacRxState>>,
    irq_state: Arc<FxmacIrqState>,
    hwaddr: [u8; 6],
}

impl FxmacNet {
    fn new() -> Self {
        let mut hwaddr = [0; 6];
        FXmacGetMacAddress(&mut hwaddr, 0);
        let device = xmac_init(&hwaddr);
        device.disable_irq();
        Self {
            hw: Arc::new(Mutex::new(FxmacHw { device })),
            tx_state: Arc::new(Mutex::new(FxmacTxState {
                tx_done: VecDeque::with_capacity(QUEUE_SIZE),
            })),
            rx_state: Arc::new(Mutex::new(FxmacRxState {
                rx_buffers: VecDeque::with_capacity(QUEUE_SIZE),
                rx_packets: VecDeque::with_capacity(QUEUE_SIZE),
            })),
            irq_state: Arc::new(FxmacIrqState::new()),
            hwaddr,
        }
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
                    Box::new(FxmacIrqHandler { hw, irq_state }),
                )],
            }],
        })
    }
}

struct FxmacHw {
    device: &'static mut FXmac,
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
    rx_pending: AtomicBool,
    tx_pending: AtomicBool,
    irq_ack_pending: AtomicBool,
}

impl FxmacIrqState {
    fn new() -> Self {
        Self {
            rx_pending: AtomicBool::new(false),
            tx_pending: AtomicBool::new(false),
            irq_ack_pending: AtomicBool::new(false),
        }
    }

    fn mark_irq_ack_pending(&self) {
        self.irq_ack_pending.store(true, Ordering::Release);
    }

    fn drain_pending_irq_ack(&self, hw: &mut FxmacHw) -> NetIrqSnapshot {
        if self.irq_ack_pending.swap(false, Ordering::AcqRel) {
            let status = hw.device.handle_irq();
            return self.publish(status.tx_ready(), status.rx_ready());
        }
        NetIrqSnapshot::empty()
    }

    fn publish(&self, tx_ready: bool, rx_ready: bool) -> NetIrqSnapshot {
        let mut snapshot = NetIrqSnapshot::empty();
        if tx_ready {
            self.tx_pending.store(true, Ordering::Release);
            snapshot = snapshot.union(NetIrqSnapshot::TX);
        }
        if rx_ready {
            self.rx_pending.store(true, Ordering::Release);
            snapshot = snapshot.union(NetIrqSnapshot::RX);
        }
        snapshot
    }

    fn take_rx_pending(&self) -> bool {
        self.rx_pending.swap(false, Ordering::AcqRel)
    }

    fn take_tx_pending(&self) -> bool {
        self.tx_pending.swap(false, Ordering::AcqRel)
    }

    fn pending_snapshot(&self) -> NetIrqSnapshot {
        let mut snapshot = NetIrqSnapshot::empty();
        if self.tx_pending.load(Ordering::Acquire) {
            snapshot = snapshot.union(NetIrqSnapshot::TX);
        }
        if self.rx_pending.load(Ordering::Acquire) {
            snapshot = snapshot.union(NetIrqSnapshot::RX);
        }
        snapshot
    }
}

struct FxmacIrqHandler {
    hw: Arc<Mutex<FxmacHw>>,
    irq_state: Arc<FxmacIrqState>,
}

impl NetHardIrqHandler for FxmacIrqHandler {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        // SAFETY: the IRQ handler already runs in a non-reentrant local context.
        if let Some(mut hw) = unsafe { self.hw.try_lock_raw() } {
            let status = hw.device.handle_irq();
            let snapshot = self.irq_state.publish(status.tx_ready(), status.rx_ready());
            if snapshot == NetIrqSnapshot::empty() {
                return NetHardIrqResult::Spurious;
            }
            hw.device.disable_irq();
            return NetHardIrqResult::Schedule(snapshot);
        }
        self.irq_state.mark_irq_ack_pending();
        NetHardIrqResult::ProbeDeferred
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

    fn rearm_and_check(&mut self, _now_nanos: u64) -> Result<NetRearmResult, NetError> {
        // SAFETY: queue polling and rearm share one non-reentrant owner.
        let mut hw = unsafe { self.hw.lock_raw() };
        let mut pending = self.irq_state.drain_pending_irq_ack(&mut hw);
        hw.device.enable_irq();
        let status = hw.device.handle_irq();
        pending = pending
            .union(self.irq_state.publish(status.tx_ready(), status.rx_ready()))
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
        let _ = self.irq_state.drain_pending_irq_ack(&mut hw);
        let ret = FXmacLwipPortTx(hw.device, vec![packet]);
        let _ = self.irq_state.drain_pending_irq_ack(&mut hw);
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
        let _ = self.irq_state.take_tx_pending();
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
        let _ = self.irq_state.drain_pending_irq_ack(&mut hw);
        let rx_pending = self.irq_state.take_rx_pending();
        if (rx_pending || rx_state.rx_packets.is_empty())
            && let Some(packets) = FXmacRecvHandler(hw.device)
        {
            rx_state.rx_packets.extend(packets);
        }
        let _ = self.irq_state.drain_pending_irq_ack(&mut hw);
        drop(hw);

        let packet = rx_state.rx_packets.pop_front()?;
        let buffer = rx_state.rx_buffers.pop_front()?;
        let len = cmp::min(packet.len(), buffer.capacity());
        unsafe {
            core::ptr::copy_nonoverlapping(packet.as_ptr(), buffer.as_ptr().as_ptr(), len);
        }
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
        let snapshot = state.publish(false, false);

        assert_eq!(snapshot, NetIrqSnapshot::empty());
        assert!(!state.take_tx_pending());
        assert!(!state.take_rx_pending());
    }

    #[test]
    fn irq_state_publishes_only_reported_queues() {
        let state = FxmacIrqState::new();

        let tx_event = state.publish(true, false);
        assert!(tx_event.contains(NetIrqSnapshot::TX));
        assert!(!tx_event.contains(NetIrqSnapshot::RX));
        assert!(state.take_tx_pending());
        assert!(!state.take_rx_pending());

        let rx_event = state.publish(false, true);
        assert!(!rx_event.contains(NetIrqSnapshot::TX));
        assert!(rx_event.contains(NetIrqSnapshot::RX));
        assert!(!state.take_tx_pending());
        assert!(state.take_rx_pending());
    }
}

struct FxmacKernelFunc;

const _: FxmacKernelFunc = FxmacKernelFunc;

#[ax_crate_interface::impl_interface]
impl fxmac_rs::KernelFunc for FxmacKernelFunc {
    fn virt_to_phys(addr: usize) -> usize {
        axklib::mem::virt_to_phys(addr.into()).as_usize()
    }

    fn phys_to_virt(addr: usize) -> usize {
        let base = addr & !(PAGE_SIZE - 1);
        let offset = addr - base;
        axklib::mem::iomap(base.into(), PAGE_SIZE)
            .map(|virt| virt.as_usize() + offset)
            .unwrap_or(addr)
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
        let handle =
            unsafe { DmaAllocHandle::new(vaddr, vaddr, DmaAddr::from(paddr as u64), layout) };
        if let Err(err) = unsafe { axklib::dma::op().dealloc_coherent(handle) } {
            log::error!("FXmac DMA release failed; allocation quarantined: {err}");
        }
    }
}
