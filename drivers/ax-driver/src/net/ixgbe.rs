use alloc::{boxed::Box, collections::VecDeque, format, sync::Arc};
use core::{alloc::Layout, cmp, ptr::NonNull, time::Duration};

use ax_kspin::SpinRaw as Mutex;
use dma_api::{DmaAddr, DmaAllocHandle, DmaConstraints, DmaOp};
use ixgbe_driver::{
    INTEL_82599, INTEL_VEND, IxgbeDevice, IxgbeError, IxgbeHal, IxgbeNetBuf, MemPool, NicDevice,
    PhysAddr,
};
use pcie::CommandRegister;
use rd_net::{DmaBuffer, Event, IRxQueue, ITxQueue, NetError, QueueConfig};
use rdrive::{
    DriverGeneric,
    probe::{
        OnProbeError,
        pci::{FnOnProbe, ProbePci},
    },
};

use crate::{PciIrqRequirement, net::ProbePciNet};

const DRIVER_NAME: &str = "ixgbe";
const QUEUE_SIZE: usize = 512;
const QUEUE_ID: u16 = 0;
const RECV_BATCH_SIZE: usize = 64;
const RX_BUFFER_QUEUE_SIZE: usize = 1024;
const MEM_POOL_ENTRIES: usize = 4096;
const MEM_POOL_ENTRY_SIZE: usize = 2048;
const DMA_MASK: u64 = u64::MAX;

crate::model_register!(
    name: "Intel 82599 PCI Network",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci as FnOnProbe,
    }],
);

fn probe_pci(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    let endpoint = probe.endpoint_mut();
    if endpoint.vendor_id() != INTEL_VEND || endpoint.device_id() != INTEL_82599 {
        return Err(OnProbeError::NotMatch);
    }

    let address = endpoint.address();
    let Some(bar) = endpoint.bar_mmio(0) else {
        return Err(OnProbeError::other("ixgbe BAR0 MMIO region missing"));
    };
    let bar_start = bar.start;
    let bar_len = bar.end.saturating_sub(bar_start);

    endpoint.update_command(|mut cmd| {
        cmd.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        cmd.remove(CommandRegister::INTERRUPT_DISABLE);
        cmd
    });

    let mmio = axklib::mmio::ioremap_raw(bar_start.into(), bar_len)
        .map_err(|err| OnProbeError::other(format!("failed to map ixgbe BAR0: {err:?}")))?;

    let dev = IxgbeNet::new(mmio.as_ptr() as usize, bar_len)
        .map_err(|err| OnProbeError::other(format!("failed to initialize ixgbe: {err:?}")))?;
    let irq = probe.register_net(DRIVER_NAME, dev, PciIrqRequirement::Required)?;
    log::info!("registered ixgbe PCI network device at {address} with irq {irq:?}");
    Ok(())
}

struct IxgbeNet {
    inner: Arc<Mutex<IxgbeInner>>,
    tx_created: bool,
    rx_created: bool,
    irq_enabled: bool,
}

impl IxgbeNet {
    fn new(base: usize, len: usize) -> Result<Self, IxgbeError> {
        let mem_pool = MemPool::allocate::<IxgbeOsHal>(MEM_POOL_ENTRIES, MEM_POOL_ENTRY_SIZE)?;
        let device = IxgbeDevice::<IxgbeOsHal, QUEUE_SIZE>::init(
            base,
            len,
            QUEUE_ID + 1,
            QUEUE_ID + 1,
            &mem_pool,
        )?;
        Ok(Self {
            inner: Arc::new(Mutex::new(IxgbeInner {
                device,
                mem_pool,
                rx_ready: VecDeque::with_capacity(RX_BUFFER_QUEUE_SIZE),
                rx_buffers: VecDeque::with_capacity(RX_BUFFER_QUEUE_SIZE),
                tx_done: VecDeque::with_capacity(QUEUE_SIZE),
            })),
            tx_created: false,
            rx_created: false,
            irq_enabled: false,
        })
    }
}

impl DriverGeneric for IxgbeNet {
    fn name(&self) -> &str {
        DRIVER_NAME
    }
}

impl rd_net::Interface for IxgbeNet {
    fn mac_address(&self) -> [u8; 6] {
        self.inner.lock().device.get_mac_addr()
    }

    fn create_tx_queue(&mut self) -> Option<alloc::boxed::Box<dyn ITxQueue>> {
        if self.tx_created {
            return None;
        }
        self.tx_created = true;
        Some(Box::new(IxgbeTxQueue {
            inner: self.inner.clone(),
        }))
    }

    fn create_rx_queue(&mut self) -> Option<alloc::boxed::Box<dyn IRxQueue>> {
        if self.rx_created {
            return None;
        }
        self.rx_created = true;
        Some(Box::new(IxgbeRxQueue {
            inner: self.inner.clone(),
        }))
    }

    fn enable_irq(&mut self) {
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        let mut event = Event::none();
        event.tx_queue.insert(QUEUE_ID as usize);
        event.rx_queue.insert(QUEUE_ID as usize);
        event
    }
}

struct IxgbeInner {
    device: IxgbeDevice<IxgbeOsHal, QUEUE_SIZE>,
    mem_pool: Arc<MemPool>,
    rx_ready: VecDeque<IxgbeNetBuf>,
    rx_buffers: VecDeque<RuntimeNetBuffer>,
    tx_done: VecDeque<u64>,
}

unsafe impl Send for IxgbeInner {}

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

struct IxgbeTxQueue {
    inner: Arc<Mutex<IxgbeInner>>,
}

impl ITxQueue for IxgbeTxQueue {
    fn id(&self) -> usize {
        QUEUE_ID as usize
    }

    fn config(&self) -> QueueConfig {
        ixgbe_queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        let mut inner = self.inner.lock();
        let _ = inner.device.recycle_tx_buffers(QUEUE_ID);
        let mut tx = IxgbeNetBuf::alloc(&inner.mem_pool, buffer.len).map_err(map_error)?;
        let source = unsafe { core::slice::from_raw_parts(buffer.virt.as_ptr(), buffer.len) };
        tx.packet_mut().copy_from_slice(source);
        inner.device.send(QUEUE_ID, tx).map_err(map_error)?;
        inner.tx_done.push_back(buffer.bus_addr);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        let mut inner = self.inner.lock();
        let _ = inner.device.recycle_tx_buffers(QUEUE_ID);
        inner.tx_done.pop_front()
    }
}

struct IxgbeRxQueue {
    inner: Arc<Mutex<IxgbeInner>>,
}

impl IRxQueue for IxgbeRxQueue {
    fn id(&self) -> usize {
        QUEUE_ID as usize
    }

    fn config(&self) -> QueueConfig {
        ixgbe_queue_config()
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

        if inner.rx_ready.is_empty() {
            let mut received = VecDeque::with_capacity(RECV_BATCH_SIZE);
            inner
                .device
                .receive_packets(QUEUE_ID, RECV_BATCH_SIZE, |packet| {
                    received.push_back(packet);
                })
                .ok()?;
            inner.rx_ready.extend(received);
        }

        let packet = inner.rx_ready.pop_front()?;
        let buffer = inner.rx_buffers.pop_front()?;
        let len = cmp::min(packet.packet_len(), buffer.len);
        unsafe {
            core::ptr::copy_nonoverlapping(packet.packet().as_ptr(), buffer.virt as *mut u8, len);
        }
        let bus_addr = buffer.bus_addr;
        drop(packet);
        Some((bus_addr, len))
    }
}

fn ixgbe_queue_config() -> QueueConfig {
    QueueConfig {
        dma_mask: DMA_MASK,
        align: 0x1000,
        buf_size: MEM_POOL_ENTRY_SIZE,
        ring_size: QUEUE_SIZE,
    }
}

fn map_error(err: IxgbeError) -> NetError {
    match err {
        IxgbeError::QueueFull | IxgbeError::NotReady => NetError::Retry,
        IxgbeError::NoMemory => NetError::NoMemory,
        IxgbeError::QueueNotAligned | IxgbeError::PageNotAligned | IxgbeError::InvalidQueue => {
            NetError::Other(Box::new(rd_net::KError::Unknown("ixgbe error")))
        }
    }
}

struct IxgbeOsHal;

unsafe impl IxgbeHal for IxgbeOsHal {
    fn dma_alloc(size: usize) -> (PhysAddr, NonNull<u8>) {
        let layout =
            Layout::from_size_align(size.max(1), 0x1000).expect("ixgbe DMA layout should be valid");
        let handle =
            unsafe { axklib::dma::op().alloc_coherent(DmaConstraints::new(DMA_MASK), layout) }
                .expect("ixgbe DMA allocation failed");
        let paddr = handle.dma_addr().as_u64() as usize;
        let vaddr = handle.as_ptr();
        (paddr, vaddr)
    }

    unsafe fn dma_dealloc(paddr: PhysAddr, vaddr: NonNull<u8>, size: usize) -> i32 {
        let Ok(layout) = Layout::from_size_align(size.max(1), 0x1000) else {
            return -1;
        };
        let handle = unsafe { DmaAllocHandle::new(vaddr, DmaAddr::from(paddr as u64), layout) };
        unsafe { axklib::dma::op().dealloc_coherent(handle) };
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, size: usize) -> NonNull<u8> {
        axklib::mmio::ioremap_raw(paddr.into(), size)
            .expect("ixgbe MMIO mapping failed")
            .as_nonnull_ptr()
    }

    unsafe fn mmio_virt_to_phys(vaddr: NonNull<u8>, _size: usize) -> PhysAddr {
        axklib::mem::virt_to_phys((vaddr.as_ptr() as usize).into()).as_usize()
    }

    fn wait_until(duration: Duration) -> Result<(), &'static str> {
        axklib::time::busy_wait(duration);
        Ok(())
    }
}
