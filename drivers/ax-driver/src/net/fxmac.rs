use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec, vec::Vec};
use core::{alloc::Layout, cmp};

use ax_kspin::SpinRaw as Mutex;
use dma_api::{DmaAddr, DmaAllocHandle, DmaConstraints, DmaOp};
use fxmac_rs::{FXmac, FXmacGetMacAddress, FXmacLwipPortTx, FXmacRecvHandler, xmac_init};
use rd_net::{DmaBuffer, Event, IRxQueue, ITxQueue, NetError, QueueConfig};
use rdrive::{DriverGeneric, PlatformDevice};

#[cfg(plat_dyn)]
use crate::binding_info_from_fdt;
use crate::net::PlatformDeviceNet;

pub const DEVICE_NAME: &str = "fxmac";

const DRIVER_NAME: &str = "cdns,phytium-gem-1.0";
const QUEUE_ID: usize = 0;
const QUEUE_SIZE: usize = 64;
const BUFFER_SIZE: usize = 2048;
const DMA_ALIGN: usize = 0x1000;
const DMA_MASK: u64 = u64::MAX;
const PAGE_SIZE: usize = 0x1000;

#[cfg(plat_dyn)]
crate::model_register!(
    name: "FXMAC FDT Network",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &[DRIVER_NAME],
        on_probe: probe_fdt,
    }],
);

#[cfg(plat_dyn)]
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
    inner: Arc<Mutex<FxmacInner>>,
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
            inner: Arc::new(Mutex::new(FxmacInner {
                device,
                hwaddr,
                rx_buffers: VecDeque::with_capacity(QUEUE_SIZE),
                rx_packets: VecDeque::with_capacity(QUEUE_SIZE),
                tx_done: VecDeque::with_capacity(QUEUE_SIZE),
            })),
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
        self.inner.lock().hwaddr
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        if self.tx_created {
            return None;
        }
        self.tx_created = true;
        Some(Box::new(FxmacTxQueue {
            inner: self.inner.clone(),
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if self.rx_created {
            return None;
        }
        self.rx_created = true;
        Some(Box::new(FxmacRxQueue {
            inner: self.inner.clone(),
        }))
    }

    fn enable_irq(&mut self) {
        let mut inner = self.inner.lock();
        inner.device.enable_irq();
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        let mut inner = self.inner.lock();
        inner.device.disable_irq();
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        let mut inner = self.inner.lock();
        inner.device.handle_irq();
        let mut event = Event::none();
        event.tx_queue.insert(QUEUE_ID);
        event.rx_queue.insert(QUEUE_ID);
        event
    }
}

struct FxmacInner {
    device: &'static mut FXmac,
    hwaddr: [u8; 6],
    rx_buffers: VecDeque<RuntimeNetBuffer>,
    rx_packets: VecDeque<Vec<u8>>,
    tx_done: VecDeque<u64>,
}

unsafe impl Send for FxmacInner {}

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
    inner: Arc<Mutex<FxmacInner>>,
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
        let mut inner = self.inner.lock();
        let ret = FXmacLwipPortTx(inner.device, vec![packet.to_vec()]);
        if ret < 0 {
            return Err(NetError::Retry);
        }
        inner.tx_done.push_back(buffer.bus_addr);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        self.inner.lock().tx_done.pop_front()
    }
}

struct FxmacRxQueue {
    inner: Arc<Mutex<FxmacInner>>,
}

impl IRxQueue for FxmacRxQueue {
    fn id(&self) -> usize {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        fxmac_queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        self.inner.lock().rx_buffers.push_back(buffer.into());
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        let mut inner = self.inner.lock();
        if inner.rx_buffers.is_empty() {
            return None;
        }

        if inner.rx_packets.is_empty()
            && let Some(packets) = FXmacRecvHandler(inner.device)
        {
            inner.rx_packets.extend(packets);
        }

        let packet = inner.rx_packets.pop_front()?;
        let buffer = inner.rx_buffers.pop_front()?;
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
