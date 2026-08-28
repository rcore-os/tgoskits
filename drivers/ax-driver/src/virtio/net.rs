extern crate alloc;

use alloc::{boxed::Box, collections::BTreeMap, format, sync::Arc, vec};
use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_sync::PreemptIrqSaveGuard;
use rd_net::{
    DmaBuffer, FixedNetControl, IRxQueue, ITxQueue, NetDevice, NetDeviceInfo, NetDeviceParts,
    NetError, NetHardIrqEndpoint, NetHardIrqHandler, NetHardIrqResult, NetIrqSnapshot,
    NetIrqSourceId, NetPollGroupId, NetPollGroupParts, NetPollIrqControl, NetQueueId,
    NetQueuePairParts, NetRearmResult, QueueConfig, RxCompletion, SubmitError,
};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(feature = "pci")]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    Error as VirtIoError,
    device::net::VirtIONetRaw,
    transport::{InterruptStatus, Transport},
};

#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};
use crate::{
    binding_info_from_fdt,
    net::PlatformDeviceNet,
    virtio::{self, VirtIoHalImpl, VirtIoTransport},
};

const QUEUE_SIZE: usize = 64;
const BUFFER_SIZE: usize = 2048;
const QUEUE_ID0: NetQueueId = NetQueueId::new(0);
const GROUP_ID0: NetPollGroupId = NetPollGroupId::new(0);
const IRQ_SOURCE0: NetIrqSourceId = NetIrqSourceId::new(0);

#[cfg(feature = "pci")]
crate::model_register!(
    name: "VirtIO Net",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

struct VirtIoNetDevice<T: VirtIoTransport> {
    inner: Arc<VirtioNetInnerCell<T>>,
}

impl<T: VirtIoTransport> VirtIoNetDevice<T> {
    fn new(transport: T) -> Result<Self, VirtIoError> {
        let mut raw = VirtIONetRaw::new(transport)?;
        raw.disable_interrupts();
        Ok(Self {
            inner: Arc::new(VirtioNetInnerCell::new(NetInner::new(raw))),
        })
    }
}

impl<T: VirtIoTransport> DriverGeneric for VirtIoNetDevice<T> {
    fn name(&self) -> &str {
        "virtio-net"
    }
}

impl<T: VirtIoTransport + 'static> NetDevice for VirtIoNetDevice<T> {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError> {
        let inner = self.inner;
        let mac = inner.with_task(|state| state.raw.mac_address());

        Ok(NetDeviceParts {
            info: NetDeviceInfo::new("virtio-net", mac),
            control: Box::new(FixedNetControl::new(mac)),
            wifi_control: None,
            poll_groups: vec![NetPollGroupParts {
                id: GROUP_ID0,
                queues: NetQueuePairParts {
                    tx: Box::new(NetTxQueue {
                        inner: Arc::clone(&inner),
                    }),
                    rx: Box::new(NetRxQueue {
                        inner: Arc::clone(&inner),
                    }),
                },
                irq_control: Box::new(VirtioNetIrqControl {
                    inner: Arc::clone(&inner),
                }),
                owner_startup: None,
                irq_endpoints: vec![NetHardIrqEndpoint::new(
                    IRQ_SOURCE0,
                    Box::new(VirtioNetIrqHandler { inner }),
                )],
            }],
        })
    }
}

struct VirtioNetInnerCell<T: VirtIoTransport> {
    inner: UnsafeCell<NetInner<T>>,
    access_active: AtomicBool,
    irq_ack_pending: AtomicBool,
}

unsafe impl<T: VirtIoTransport> Send for VirtioNetInnerCell<T> {}
unsafe impl<T: VirtIoTransport> Sync for VirtioNetInnerCell<T> {}

impl<T: VirtIoTransport> VirtioNetInnerCell<T> {
    fn new(inner: NetInner<T>) -> Self {
        Self {
            inner: UnsafeCell::new(inner),
            access_active: AtomicBool::new(false),
            irq_ack_pending: AtomicBool::new(false),
        }
    }

    fn with_task<R>(&self, f: impl FnOnce(&mut NetInner<T>) -> R) -> R {
        let _guard = PreemptIrqSaveGuard::new();
        let _active = VirtioNetAccessGuard::enter_task(&self.access_active);
        // SAFETY: `access_active` serializes all mutable access to the shared
        // raw transport. Task-side callers also keep local IRQ/preemption off.
        let inner = unsafe { &mut *self.inner.get() };
        self.flush_pending_irq_ack(inner);
        let ret = f(inner);
        self.flush_pending_irq_ack(inner);
        ret
    }

    fn try_with_irq<R>(&self, f: impl FnOnce(&mut NetInner<T>) -> R) -> Option<R> {
        let _active = VirtioNetAccessGuard::try_enter_irq(&self.access_active)?;
        // SAFETY: `access_active` serializes IRQ-side access with task-side
        // queue operations. IRQ context never waits for task-side access.
        Some(f(unsafe { &mut *self.inner.get() }))
    }

    fn handle_irq(&self) -> NetHardIrqResult {
        let Some(queue_interrupt) = self.try_with_irq(|inner| {
            self.irq_ack_pending.store(false, Ordering::Release);
            let queue_interrupt = inner
                .raw
                .ack_interrupt()
                .contains(InterruptStatus::QUEUE_INTERRUPT);
            if queue_interrupt {
                inner.raw.disable_interrupts();
            }
            queue_interrupt
        }) else {
            return irq_gate_miss(&self.irq_ack_pending);
        };

        if !queue_interrupt {
            return NetHardIrqResult::Spurious;
        }
        NetHardIrqResult::Schedule(NetIrqSnapshot::all_queue_work())
    }

    fn flush_pending_irq_ack(&self, inner: &mut NetInner<T>) {
        if self.irq_ack_pending.swap(false, Ordering::AcqRel) {
            let _ = inner.raw.ack_interrupt();
        }
    }
}

fn irq_gate_miss(irq_ack_pending: &AtomicBool) -> NetHardIrqResult {
    irq_ack_pending.store(true, Ordering::Release);
    NetHardIrqResult::ProbeDeferred
}

struct VirtioNetAccessGuard<'a>(&'a AtomicBool);

impl<'a> VirtioNetAccessGuard<'a> {
    fn enter_task(active: &'a AtomicBool) -> Self {
        Self::enter(active)
    }

    fn try_enter_irq(active: &'a AtomicBool) -> Option<Self> {
        active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
            .then_some(Self(active))
    }

    fn enter(active: &'a AtomicBool) -> Self {
        while active
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        Self(active)
    }
}

struct VirtioNetIrqHandler<T: VirtIoTransport> {
    inner: Arc<VirtioNetInnerCell<T>>,
}

impl<T: VirtIoTransport + 'static> NetHardIrqHandler for VirtioNetIrqHandler<T> {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        self.inner.handle_irq()
    }
}

struct VirtioNetIrqControl<T: VirtIoTransport> {
    inner: Arc<VirtioNetInnerCell<T>>,
}

impl<T: VirtIoTransport + 'static> NetPollIrqControl for VirtioNetIrqControl<T> {
    fn quiesce(&mut self) -> Result<(), NetError> {
        self.inner.with_task(|inner| inner.raw.disable_interrupts());
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), NetError> {
        self.inner.with_task(|inner| inner.raw.disable_interrupts());
        // The pinned virtio-drivers API only unsets queues from Raw::drop.
        // Quarantine the shared state until an explicit transport reset can
        // prove that its inflight DMA tokens are unreachable.
        Err(NetError::DmaShutdownUnconfirmed)
    }

    fn rearm_and_check(&mut self, _now_nanos: u64) -> Result<NetRearmResult, NetError> {
        let pending = self.inner.with_task(|inner| {
            inner.raw.enable_interrupts();
            let interrupt = inner
                .raw
                .ack_interrupt()
                .contains(InterruptStatus::QUEUE_INTERRUPT);
            interrupt || inner.has_pending_completion()
        });
        if pending {
            self.inner.with_task(|inner| inner.raw.disable_interrupts());
            Ok(NetRearmResult::WorkPending(NetIrqSnapshot::all_queue_work()))
        } else {
            Ok(NetRearmResult::Idle)
        }
    }
}

impl Drop for VirtioNetAccessGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct NetInner<T: VirtIoTransport> {
    raw: VirtIONetRaw<VirtIoHalImpl, T, QUEUE_SIZE>,
    tx_inflight: BTreeMap<u16, TxInflight>,
    rx_inflight: BTreeMap<u16, RxInflight>,
}

unsafe impl<T: VirtIoTransport> Send for NetInner<T> {}

impl<T: VirtIoTransport> NetInner<T> {
    fn new(raw: VirtIONetRaw<VirtIoHalImpl, T, QUEUE_SIZE>) -> Self {
        Self {
            raw,
            tx_inflight: BTreeMap::new(),
            rx_inflight: BTreeMap::new(),
        }
    }

    fn queue_config() -> QueueConfig {
        QueueConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            buf_size: BUFFER_SIZE,
            ring_size: QUEUE_SIZE,
        }
    }

    fn submit_tx(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        let header_len = match self.raw_header_len() {
            Ok(header_len) => header_len,
            Err(error) => return Err(SubmitError::new(buffer, error)),
        };
        let packet_len = buffer.len();
        let mut staging = alloc::vec![0; header_len + packet_len];
        let header_len = match self.raw.fill_buffer_header(&mut staging) {
            Ok(header_len) => header_len,
            Err(error) => return Err(SubmitError::new(buffer, map_net_error(error))),
        };
        buffer.read_with_cpu(packet_len, |packet| {
            staging[header_len..header_len + packet_len].copy_from_slice(packet);
        });
        let token = match unsafe { self.raw.transmit_begin(&staging) } {
            Ok(token) => token,
            Err(error) => return Err(SubmitError::new(buffer, map_net_error(error))),
        };
        self.tx_inflight
            .insert(token, TxInflight { buffer, staging });
        Ok(())
    }

    fn reclaim_tx(&mut self) -> Option<DmaBuffer> {
        let token = self.raw.poll_transmit()?;
        let inflight = self.tx_inflight.remove(&token)?;
        let _ = unsafe { self.raw.transmit_complete(token, &inflight.staging) };
        Some(inflight.buffer)
    }

    fn submit_rx(&mut self, mut buffer: DmaBuffer) -> Result<(), SubmitError> {
        let rx_buffer =
            unsafe { core::slice::from_raw_parts_mut(buffer.as_ptr().as_ptr(), buffer.capacity()) };
        let token = match unsafe { self.raw.receive_begin(rx_buffer) } {
            Ok(token) => token,
            Err(error) => return Err(SubmitError::new(buffer, map_net_error(error))),
        };
        if let Err(error) = buffer.set_len(buffer.capacity()) {
            return Err(SubmitError::new(buffer, error));
        }
        self.rx_inflight.insert(token, RxInflight { buffer });
        Ok(())
    }

    fn reclaim_rx(&mut self) -> Option<RxCompletion> {
        let token = self.raw.poll_receive()?;
        let inflight = self.rx_inflight.remove(&token)?;
        let bytes = unsafe {
            core::slice::from_raw_parts_mut(
                inflight.buffer.as_ptr().as_ptr(),
                inflight.buffer.capacity(),
            )
        };
        let (header_len, packet_len) =
            unsafe { self.raw.receive_complete(token, bytes) }.unwrap_or((0, 0));
        if packet_len > 0 {
            bytes.copy_within(header_len..header_len + packet_len, 0);
        }
        Some(RxCompletion {
            buffer: inflight.buffer,
            packet_len,
        })
    }

    fn raw_header_len(&mut self) -> Result<usize, NetError> {
        let mut header = [0_u8; 16];
        self.raw
            .fill_buffer_header(&mut header)
            .map_err(map_net_error)
    }

    fn has_pending_completion(&mut self) -> bool {
        // `VirtIONetRaw::poll_*` are non-consuming used-ring probes backed by
        // `VirtQueue::peek_used`. Queue reclaim remains the only path that
        // calls `*_complete` and releases the corresponding inflight token.
        self.raw.poll_receive().is_some() || self.raw.poll_transmit().is_some()
    }
}

struct NetTxQueue<T: VirtIoTransport> {
    inner: Arc<VirtioNetInnerCell<T>>,
}

impl<T: VirtIoTransport> ITxQueue for NetTxQueue<T> {
    fn id(&self) -> NetQueueId {
        QUEUE_ID0
    }

    fn config(&self) -> QueueConfig {
        NetInner::<T>::queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        self.inner.with_task(|inner| inner.submit_tx(buffer))
    }

    fn reclaim(&mut self) -> Option<DmaBuffer> {
        self.inner.with_task(NetInner::reclaim_tx)
    }
}

struct NetRxQueue<T: VirtIoTransport> {
    inner: Arc<VirtioNetInnerCell<T>>,
}

impl<T: VirtIoTransport> IRxQueue for NetRxQueue<T> {
    fn id(&self) -> NetQueueId {
        QUEUE_ID0
    }

    fn config(&self) -> QueueConfig {
        NetInner::<T>::queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        self.inner.with_task(|inner| inner.submit_rx(buffer))
    }

    fn reclaim(&mut self) -> Option<RxCompletion> {
        self.inner.with_task(NetInner::reclaim_rx)
    }
}

struct TxInflight {
    buffer: DmaBuffer,
    staging: alloc::vec::Vec<u8>,
}

struct RxInflight {
    buffer: DmaBuffer,
}

#[cfg(feature = "pci")]
fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    let transport = crate::pci::take_virtio_transport(probe.endpoint_mut(), DeviceType::Network)?;
    register_pci_transport(probe, transport)
}

pub fn register_transport<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    let net = make_net(transport)?;
    let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        dma_api::DmaCoherency::NonCoherent,
        dma_api::DmaConstraints::new(u64::MAX),
    ));
    let irq = plat_dev.register_net("virtio-net", net, dma);
    log::info!("registered virtio network device irq={irq:?}");
    Ok(())
}

pub fn register_fdt_transport<T: Transport + 'static>(
    info: &rdrive::register::FdtInfo<'_>,
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    let net = make_net(transport)?;
    let binding = binding_info_from_fdt(info)?;
    let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        crate::binding_resolver::dma_coherency_from_fdt(info),
        dma_api::DmaConstraints::new(u64::MAX),
    ));
    let irq = plat_dev.register_net_with_info("virtio-net", net, dma, binding);
    log::info!("registered virtio network device irq={irq:?}");
    Ok(())
}

#[cfg(feature = "pci")]
fn register_pci_transport<T: Transport + 'static>(
    probe: rdrive::probe::pci::ProbePci<'_>,
    transport: T,
) -> Result<(), OnProbeError> {
    let dma = crate::pci::device_dma(probe.info(), u64::MAX);
    let info = binding_info_from_pci(probe.info(), PciIrqRequirement::Required)?;
    let net = make_net(transport)?;
    let irq = probe
        .into_platform_device()
        .register_net_with_info("virtio-net", net, dma, info);
    log::info!("registered virtio network device irq={irq:?}");
    Ok(())
}

fn make_net<T: Transport + 'static>(transport: T) -> Result<VirtIoNetDevice<T>, OnProbeError> {
    VirtIoNetDevice::new(transport).map_err(|err| {
        OnProbeError::other(format!(
            "failed to initialize static VirtIO net device: {err:?}"
        ))
    })
}

fn map_net_error(err: VirtIoError) -> NetError {
    match err {
        VirtIoError::QueueFull | VirtIoError::NotReady => NetError::Retry,
        VirtIoError::DmaError => NetError::NoMemory,
        VirtIoError::Unsupported => NetError::NotSupported,
        other => NetError::Other(Box::new(rd_net::KError::Unknown(virtio::map_virtio_error(
            other,
        )))),
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::sync::atomic::{AtomicBool, Ordering};

    use rd_net::NetHardIrqResult;

    use super::{VirtioNetAccessGuard, irq_gate_miss};

    #[test]
    fn irq_access_returns_none_when_task_access_is_active() {
        let active = AtomicBool::new(false);
        let task_guard = VirtioNetAccessGuard::enter_task(&active);

        assert!(VirtioNetAccessGuard::try_enter_irq(&active).is_none());
        drop(task_guard);
        assert!(VirtioNetAccessGuard::try_enter_irq(&active).is_some());
    }

    #[test]
    fn skipped_irq_access_records_pending_ack_without_queue_event() {
        let access_active = AtomicBool::new(false);
        let irq_ack_pending = AtomicBool::new(false);
        let task_guard = VirtioNetAccessGuard::enter_task(&access_active);

        let result = if VirtioNetAccessGuard::try_enter_irq(&access_active).is_none() {
            irq_gate_miss(&irq_ack_pending)
        } else {
            NetHardIrqResult::Schedule(rd_net::NetIrqSnapshot::all_queue_work())
        };

        assert_eq!(result, NetHardIrqResult::ProbeDeferred);
        assert!(irq_ack_pending.load(Ordering::Acquire));
        drop(task_guard);
        assert!(VirtioNetAccessGuard::try_enter_irq(&access_active).is_some());
        assert!(irq_ack_pending.swap(false, Ordering::AcqRel));
        assert!(!irq_ack_pending.load(Ordering::Acquire));
    }
}
