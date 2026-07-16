extern crate alloc;

use alloc::{boxed::Box, collections::BTreeMap, format, sync::Arc};
use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_kspin::PreemptIrqGuard;
use rd_net::{DmaBuffer, Event, IRxQueue, ITxQueue, NetError, QueueConfig};
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
    net::PlatformDeviceNet,
    virtio::{self, VirtIoHalImpl, VirtIoTransport},
};

const QUEUE_SIZE: usize = 64;
const BUFFER_SIZE: usize = 2048;

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
    irq_enabled: bool,
    tx_created: bool,
    rx_created: bool,
}

impl<T: VirtIoTransport> VirtIoNetDevice<T> {
    fn new(transport: T) -> Result<Self, VirtIoError> {
        let mut raw = VirtIONetRaw::new(transport)?;
        raw.disable_interrupts();
        Ok(Self {
            inner: Arc::new(VirtioNetInnerCell::new(NetInner::new(raw))),
            irq_enabled: false,
            tx_created: false,
            rx_created: false,
        })
    }
}

impl<T: VirtIoTransport> DriverGeneric for VirtIoNetDevice<T> {
    fn name(&self) -> &str {
        "virtio-net"
    }
}

impl<T: VirtIoTransport> rd_net::Interface for VirtIoNetDevice<T> {
    fn mac_address(&self) -> [u8; 6] {
        self.inner.with_task(|inner| inner.raw.mac_address())
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        if self.tx_created {
            return None;
        }
        self.tx_created = true;
        Some(Box::new(NetTxQueue {
            inner: Arc::clone(&self.inner),
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if self.rx_created {
            return None;
        }
        self.rx_created = true;
        Some(Box::new(NetRxQueue {
            inner: Arc::clone(&self.inner),
        }))
    }

    fn enable_irq(&mut self) {
        self.irq_enabled = true;
        self.inner.with_task(|inner| inner.raw.enable_interrupts());
    }

    fn disable_irq(&mut self) {
        self.irq_enabled = false;
        self.inner.with_task(|inner| inner.raw.disable_interrupts());
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        self.inner.handle_irq()
    }

    fn take_irq_handler(&mut self) -> Option<rd_net::BIrqHandler> {
        Some(Box::new(VirtioNetIrqHandler {
            inner: Arc::clone(&self.inner),
        }))
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
        let _guard = PreemptIrqGuard::new();
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

    fn handle_irq(&self) -> Event {
        let queue_interrupt = self
            .try_with_irq(|inner| {
                self.irq_ack_pending.store(false, Ordering::Release);
                inner
                    .raw
                    .ack_interrupt()
                    .contains(InterruptStatus::QUEUE_INTERRUPT)
            })
            .unwrap_or_else(|| {
                self.irq_ack_pending.store(true, Ordering::Release);
                // The task-side owner will acknowledge the transport before
                // and after its queue operation. Without an IRQ status snapshot
                // we must not publish a queue event from a shared interrupt.
                false
            });

        if !queue_interrupt {
            return Event::none();
        }

        let mut event = Event::none();
        event.tx_queue.insert(0);
        event.rx_queue.insert(0);
        event
    }

    fn flush_pending_irq_ack(&self, inner: &mut NetInner<T>) {
        if self.irq_ack_pending.swap(false, Ordering::AcqRel) {
            let _ = inner.raw.ack_interrupt();
        }
    }
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

impl<T: VirtIoTransport + 'static> rd_net::InterfaceIrqHandler for VirtioNetIrqHandler<T> {
    fn handle_irq(&mut self) -> Event {
        self.inner.handle_irq()
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

    fn submit_tx(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        let packet = unsafe { core::slice::from_raw_parts(buffer.virt.as_ptr(), buffer.len) };
        let mut staging = alloc::vec![0; self.raw_header_len()? + buffer.len];
        let header_len = self
            .raw
            .fill_buffer_header(&mut staging)
            .map_err(map_net_error)?;
        staging[header_len..header_len + buffer.len].copy_from_slice(packet);
        let token = unsafe { self.raw.transmit_begin(&staging) }.map_err(map_net_error)?;
        self.tx_inflight.insert(
            token,
            TxInflight {
                bus_addr: buffer.bus_addr,
                staging,
            },
        );
        Ok(())
    }

    fn reclaim_tx(&mut self) -> Option<u64> {
        let token = self.raw.poll_transmit()?;
        let inflight = self.tx_inflight.remove(&token)?;
        let _ = unsafe { self.raw.transmit_complete(token, &inflight.staging) };
        Some(inflight.bus_addr)
    }

    fn submit_rx(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        let rx_buffer =
            unsafe { core::slice::from_raw_parts_mut(buffer.virt.as_ptr(), buffer.len) };
        let token = unsafe { self.raw.receive_begin(rx_buffer) }.map_err(map_net_error)?;
        self.rx_inflight.insert(
            token,
            RxInflight {
                virt_addr: buffer.virt.as_ptr() as usize,
                bus_addr: buffer.bus_addr,
                len: buffer.len,
            },
        );
        Ok(())
    }

    fn reclaim_rx(&mut self) -> Option<(u64, usize)> {
        let token = self.raw.poll_receive()?;
        let inflight = self.rx_inflight.remove(&token)?;
        let buffer =
            unsafe { core::slice::from_raw_parts_mut(inflight.virt_addr as *mut u8, inflight.len) };
        let (header_len, packet_len) = unsafe { self.raw.receive_complete(token, buffer) }.ok()?;
        buffer.copy_within(header_len..header_len + packet_len, 0);
        Some((inflight.bus_addr, packet_len))
    }

    fn raw_header_len(&mut self) -> Result<usize, NetError> {
        let mut header = [0_u8; 16];
        self.raw
            .fill_buffer_header(&mut header)
            .map_err(map_net_error)
    }
}

struct NetTxQueue<T: VirtIoTransport> {
    inner: Arc<VirtioNetInnerCell<T>>,
}

impl<T: VirtIoTransport> ITxQueue for NetTxQueue<T> {
    fn id(&self) -> usize {
        0
    }

    fn config(&self) -> QueueConfig {
        NetInner::<T>::queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        self.inner.with_task(|inner| inner.submit_tx(buffer))
    }

    fn reclaim(&mut self) -> Option<u64> {
        self.inner.with_task(NetInner::reclaim_tx)
    }
}

struct NetRxQueue<T: VirtIoTransport> {
    inner: Arc<VirtioNetInnerCell<T>>,
}

impl<T: VirtIoTransport> IRxQueue for NetRxQueue<T> {
    fn id(&self) -> usize {
        0
    }

    fn config(&self) -> QueueConfig {
        NetInner::<T>::queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        self.inner.with_task(|inner| inner.submit_rx(buffer))
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        self.inner.with_task(NetInner::reclaim_rx)
    }
}

struct TxInflight {
    bus_addr: u64,
    staging: alloc::vec::Vec<u8>,
}

struct RxInflight {
    virt_addr: usize,
    bus_addr: u64,
    len: usize,
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
    let irq = plat_dev.register_net("virtio-net", net);
    log::info!("registered virtio network device irq={irq:?}");
    Ok(())
}

#[cfg(feature = "pci")]
fn register_pci_transport<T: Transport + 'static>(
    probe: rdrive::probe::pci::ProbePci<'_>,
    transport: T,
) -> Result<(), OnProbeError> {
    let info = binding_info_from_pci(probe.info(), PciIrqRequirement::Required)?;
    let net = make_net(transport)?;
    let irq = probe
        .into_platform_device()
        .register_net_with_info("virtio-net", net, info);
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

    use super::VirtioNetAccessGuard;

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

        let queue_interrupt = if VirtioNetAccessGuard::try_enter_irq(&access_active).is_none() {
            irq_ack_pending.store(true, Ordering::Release);
            false
        } else {
            true
        };

        assert!(!queue_interrupt);
        assert!(irq_ack_pending.load(Ordering::Acquire));
        drop(task_guard);
        assert!(VirtioNetAccessGuard::try_enter_irq(&access_active).is_some());
        assert!(irq_ack_pending.swap(false, Ordering::AcqRel));
        assert!(!irq_ack_pending.load(Ordering::Acquire));
    }
}
