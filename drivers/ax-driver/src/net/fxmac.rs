use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec, vec::Vec};
use core::{
    alloc::Layout,
    cmp,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_kspin::SpinRaw as Mutex;
use dma_api::{DmaAddr, DmaAllocHandle, DmaConstraints, DmaOp};
use fxmac_rs::{FXmac, FXmacGetMacAddress, FXmacLwipPortTx, FXmacRecvHandler, xmac_init};
use rd_net::{DmaBuffer, Event, IRxQueue, ITxQueue, NetError, QueueConfig};
use rdrive::{DriverGeneric, PlatformDevice};

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
    let dev = FxmacNet::new();
    probe
        .into_platform_device()
        .register_net_with_info(DRIVER_NAME, dev, info);
    log::info!("registered FXmac FDT network device");
    Ok(())
}

pub fn register(plat_dev: PlatformDevice) {
    let dev = FxmacNet::new();
    plat_dev.register_net(DRIVER_NAME, dev);
    log::info!("registered FXmac network device");
}

struct FxmacNet {
    hw: Arc<Mutex<FxmacHw>>,
    tx_state: Arc<Mutex<FxmacTxState>>,
    rx_state: Arc<Mutex<FxmacRxState>>,
    irq_state: Arc<FxmacIrqState>,
    hwaddr: [u8; 6],
    tx_created: bool,
    rx_created: bool,
    irq_enabled: bool,
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
            tx_created: false,
            rx_created: false,
            irq_enabled: false,
        }
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
        self.hw.lock().device.enable_irq();
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.hw.lock().device.disable_irq();
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        let mut handler = FxmacIrqHandler {
            hw: Arc::clone(&self.hw),
            irq_state: Arc::clone(&self.irq_state),
        };
        rd_net::InterfaceIrqHandler::handle_irq(&mut handler)
    }

    fn take_irq_handler(&mut self) -> Option<rd_net::BIrqHandler> {
        Some(Box::new(FxmacIrqHandler {
            hw: Arc::clone(&self.hw),
            irq_state: Arc::clone(&self.irq_state),
        }))
    }
}

struct FxmacHw {
    device: &'static mut FXmac,
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

    fn drain_pending_irq_ack(&self, hw: &mut FxmacHw) {
        if self.irq_ack_pending.swap(false, Ordering::AcqRel) {
            let status = hw.device.handle_irq();
            let _ = self.publish(status.tx_ready(), status.rx_ready());
        }
    }

    fn publish(&self, tx_ready: bool, rx_ready: bool) -> Event {
        let mut event = Event::none();
        if tx_ready {
            self.tx_pending.store(true, Ordering::Release);
            event.tx_queue.insert(QUEUE_ID);
        }
        if rx_ready {
            self.rx_pending.store(true, Ordering::Release);
            event.rx_queue.insert(QUEUE_ID);
        }
        event
    }

    fn take_rx_pending(&self) -> bool {
        self.rx_pending.swap(false, Ordering::AcqRel)
    }

    fn take_tx_pending(&self) -> bool {
        self.tx_pending.swap(false, Ordering::AcqRel)
    }
}

struct FxmacIrqHandler {
    hw: Arc<Mutex<FxmacHw>>,
    irq_state: Arc<FxmacIrqState>,
}

impl rd_net::InterfaceIrqHandler for FxmacIrqHandler {
    fn handle_irq(&mut self) -> Event {
        if let Some(mut hw) = self.hw.try_lock() {
            let status = hw.device.handle_irq();
            return self.irq_state.publish(status.tx_ready(), status.rx_ready());
        }
        self.irq_state.mark_irq_ack_pending();
        Event::none()
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
        let mut hw = self.hw.lock();
        self.irq_state.drain_pending_irq_ack(&mut hw);
        let ret = FXmacLwipPortTx(hw.device, vec![packet.to_vec()]);
        self.irq_state.drain_pending_irq_ack(&mut hw);
        if ret < 0 {
            return Err(NetError::Retry);
        }
        drop(hw);
        self.tx_state.lock().tx_done.push_back(buffer.bus_addr);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        let _ = self.irq_state.take_tx_pending();
        self.tx_state.lock().tx_done.pop_front()
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
        self.rx_state.lock().rx_buffers.push_back(buffer.into());
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        let mut rx_state = self.rx_state.lock();
        if rx_state.rx_buffers.is_empty() {
            return None;
        }

        let mut hw = self.hw.lock();
        self.irq_state.drain_pending_irq_ack(&mut hw);
        let rx_pending = self.irq_state.take_rx_pending();
        if (rx_pending || rx_state.rx_packets.is_empty())
            && let Some(packets) = FXmacRecvHandler(hw.device)
        {
            rx_state.rx_packets.extend(packets);
        }
        self.irq_state.drain_pending_irq_ack(&mut hw);
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
        let event = state.publish(false, false);

        assert!(!event.tx_queue.contains(QUEUE_ID));
        assert!(!event.rx_queue.contains(QUEUE_ID));
        assert!(!state.take_tx_pending());
        assert!(!state.take_rx_pending());
    }

    #[test]
    fn irq_state_publishes_only_reported_queues() {
        let state = FxmacIrqState::new();

        let tx_event = state.publish(true, false);
        assert!(tx_event.tx_queue.contains(QUEUE_ID));
        assert!(!tx_event.rx_queue.contains(QUEUE_ID));
        assert!(state.take_tx_pending());
        assert!(!state.take_rx_pending());

        let rx_event = state.publish(false, true);
        assert!(!rx_event.tx_queue.contains(QUEUE_ID));
        assert!(rx_event.rx_queue.contains(QUEUE_ID));
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
        let handle = unsafe { DmaAllocHandle::new(vaddr, DmaAddr::from(paddr as u64), layout) };
        unsafe { axklib::dma::op().dealloc_coherent(handle) };
    }
}
