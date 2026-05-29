extern crate alloc;

use alloc::{boxed::Box, collections::BTreeMap, format, sync::Arc};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_kernel_guard::NoPreemptIrqSave;
use rd_net::{DmaBuffer, Event, IRxQueue, ITxQueue, Interface, NetError, QueueConfig};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(any(plat_static, plat_dyn))]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{Error as VirtIoError, device::net::VirtIONetRaw, transport::Transport};

use crate::{
    net::PlatformDeviceNet,
    virtio::{self, VirtIoHalImpl, VirtIoTransport},
};

const QUEUE_SIZE: usize = 64;
const BUFFER_SIZE: usize = 2048;

#[cfg(any(plat_static, plat_dyn))]
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
        self.inner.with_irq(|inner| {
            let _ = inner.raw.ack_interrupt();

            let mut event = Event::none();
            if inner.raw.poll_transmit().is_some() {
                event.tx_queue.insert(0);
            }
            if inner.raw.poll_receive().is_some() {
                event.rx_queue.insert(0);
            }
            event
        })
    }
}

struct VirtioNetInnerCell<T: VirtIoTransport> {
    inner: UnsafeCell<NetInner<T>>,
    task_active: AtomicBool,
}

unsafe impl<T: VirtIoTransport> Send for VirtioNetInnerCell<T> {}
unsafe impl<T: VirtIoTransport> Sync for VirtioNetInnerCell<T> {}

impl<T: VirtIoTransport> VirtioNetInnerCell<T> {
    fn new(inner: NetInner<T>) -> Self {
        Self {
            inner: UnsafeCell::new(inner),
            task_active: AtomicBool::new(false),
        }
    }

    fn with_task<R>(&self, f: impl FnOnce(&mut NetInner<T>) -> R) -> R {
        let _guard = NoPreemptIrqSave::new();
        let _active = TaskAccessFlag::enter(&self.task_active);
        // SAFETY: task-side callers enter with local IRQ/preemption disabled,
        // and upper rd-net users serialize queue/device calls with SpinNoIrq.
        f(unsafe { &mut *self.inner.get() })
    }

    fn with_irq<R>(&self, f: impl FnOnce(&mut NetInner<T>) -> R) -> R {
        debug_assert!(
            !self.task_active.load(Ordering::Acquire),
            "virtio-net IRQ access interrupted task access"
        );
        // SAFETY: IRQ-side access never waits on a task lock. The task side
        // excludes local IRQ reentry while it mutates the shared raw queues.
        f(unsafe { &mut *self.inner.get() })
    }
}

struct TaskAccessFlag<'a>(&'a AtomicBool);

impl<'a> TaskAccessFlag<'a> {
    fn enter(active: &'a AtomicBool) -> Self {
        debug_assert!(
            !active.swap(true, Ordering::AcqRel),
            "virtio-net inner task access is already active"
        );
        Self(active)
    }
}

impl Drop for TaskAccessFlag<'_> {
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

#[cfg(any(plat_static, plat_dyn))]
fn probe_pci(
    endpoint: &mut rdrive::probe::pci::EndpointRc,
    plat_dev: PlatformDevice,
) -> Result<(), OnProbeError> {
    let irq = crate::net::pci_legacy_irq(endpoint);
    let transport = crate::pci::take_virtio_transport(endpoint, DeviceType::Network)?;
    register_transport_with_irq(plat_dev, transport, irq)
}

pub fn register_transport<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    register_transport_with_irq(plat_dev, transport, None)
}

pub fn register_transport_with_irq<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
    irq_num: Option<usize>,
) -> Result<(), OnProbeError> {
    let mut net = VirtIoNetDevice::new(transport).map_err(|err| {
        OnProbeError::other(format!(
            "failed to initialize static VirtIO net device: {err:?}"
        ))
    })?;
    if irq_num.is_some() {
        net.enable_irq();
    }
    plat_dev.register_net("virtio-net", net, irq_num);
    log::info!("registered virtio network device irq={irq_num:?}");
    Ok(())
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
