#[cfg(feature = "irq")]
use alloc::sync::Arc;
use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use core::alloc::Layout;
#[cfg(feature = "irq")]
use core::cell::UnsafeCell;

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use dma_api::{ContiguousArray, DeviceDma, DmaDirection};
use log::{error, warn};
use rdif_block::{
    BlkError, Buffer, IQueue, Interface, Request, RequestId, RequestKind, RequestStatus,
};
use rdrive::Device;

pub struct Block {
    name: String,
    irq_num: Option<usize>,
    #[cfg(feature = "irq")]
    irq_handler: Option<BlockIrqHandler>,
    queue: SpinNoIrq<BlockQueue>,
}

struct BlockQueue {
    raw: Box<dyn IQueue>,
    pool: BlockBufferPool,
}

struct BlockBufferPool {
    dma: DeviceDma,
    size: usize,
    align: usize,
}

pub struct PlatformBlockDevice {
    name: String,
    interface: Option<Box<dyn Interface>>,
    irq_num: Option<usize>,
}

impl PlatformBlockDevice {
    fn new(name: String, interface: Box<dyn Interface>, irq_num: Option<usize>) -> Self {
        Self {
            name,
            interface: Some(interface),
            irq_num,
        }
    }
}

impl rdrive::DriverGeneric for PlatformBlockDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(feature = "irq")]
struct BlockInterfaceOwner(UnsafeCell<Box<dyn Interface>>);

#[cfg(feature = "irq")]
// SAFETY: ax-driver creates the queue before exporting the handler. After that,
// task-side block I/O only touches the queue object; the shared interface owner
// is used exclusively for IRQ control and IRQ event extraction.
unsafe impl Send for BlockInterfaceOwner {}
#[cfg(feature = "irq")]
// SAFETY: See the `Send` impl. The IRQ callback path uses no lock in this owner.
unsafe impl Sync for BlockInterfaceOwner {}

#[cfg(feature = "irq")]
impl BlockInterfaceOwner {
    fn new(interface: Box<dyn Interface>) -> Self {
        Self(UnsafeCell::new(interface))
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut dyn Interface) -> R) -> R {
        // SAFETY: The owner is constructed before queue creation and then only
        // accessed by the IRQ control path. The queue itself is independent.
        let interface = unsafe { &mut *self.0.get() };
        f(&mut **interface)
    }
}

#[cfg(feature = "irq")]
pub struct BlockIrqHandler {
    interface: Arc<BlockInterfaceOwner>,
}

#[cfg(feature = "irq")]
impl BlockIrqHandler {
    fn new(interface: Arc<BlockInterfaceOwner>) -> Self {
        Self { interface }
    }

    pub fn enable_irq(&self) {
        self.interface.with_mut(|interface| interface.enable_irq());
    }

    pub fn handle(&self) -> rdif_block::Event {
        self.interface.with_mut(|interface| interface.handle_irq())
    }
}

#[cfg(not(feature = "irq"))]
pub struct BlockIrqHandler;

impl Block {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn irq_num(&self) -> Option<usize> {
        self.irq_num
    }

    #[cfg(feature = "irq")]
    pub fn take_irq_handler(&mut self) -> Option<(usize, BlockIrqHandler)> {
        let irq_num = self.irq_num.take()?;
        let handler = self.irq_handler.take()?;
        Some((irq_num, handler))
    }

    #[cfg(not(feature = "irq"))]
    pub fn take_irq_handler(&mut self) -> Option<(usize, BlockIrqHandler)> {
        let _ = self;
        None
    }

    pub fn num_blocks(&self) -> u64 {
        self.queue.lock().raw.num_blocks() as _
    }

    pub fn block_size(&self) -> usize {
        self.queue.lock().raw.block_size()
    }

    pub fn flush(&mut self) -> AxResult {
        Ok(())
    }

    pub fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        let mut queue = self.queue.lock();
        validate_io(&*queue.raw, block_id, buf.len())?;

        let block_size = queue.raw.block_size();
        for (offset, block_buf) in buf.chunks_exact_mut(block_size).enumerate() {
            let mut dma_buffer = queue.pool.alloc(DmaDirection::FromDevice)?;
            let request = Request {
                block_id: checked_block_id(block_id, offset)?,
                kind: RequestKind::Read(buffer_from_dma(&mut dma_buffer, block_size)),
            };
            let request_id = queue
                .raw
                .submit_request(request)
                .map_err(map_blk_err_to_ax_err)?;
            queue.poll_until_complete(request_id)?;
            dma_buffer.sync_for_cpu(0, block_size);
            dma_buffer.read_with(block_size, |data| block_buf.copy_from_slice(data));
        }
        Ok(())
    }

    pub fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        let mut queue = self.queue.lock();
        validate_io(&*queue.raw, block_id, buf.len())?;

        let block_size = queue.raw.block_size();
        for (offset, block_buf) in buf.chunks_exact(block_size).enumerate() {
            let mut dma_buffer = queue.pool.alloc(DmaDirection::ToDevice)?;
            dma_buffer.write_with(block_size, |data| data.copy_from_slice(block_buf));
            dma_buffer.sync_for_device(0, block_size);
            let request = Request {
                block_id: checked_block_id(block_id, offset)?,
                kind: RequestKind::Write(buffer_from_dma(&mut dma_buffer, block_size)),
            };
            let request_id = queue
                .raw
                .submit_request(request)
                .map_err(map_blk_err_to_ax_err)?;
            queue.poll_until_complete(request_id)?;
        }
        Ok(())
    }
}

impl BlockQueue {
    fn new(raw: Box<dyn IQueue>) -> AxResult<Self> {
        let config = raw.buffer_config();
        let block_size = raw.block_size();
        if block_size == 0 || config.size < block_size {
            return Err(AxError::BadState);
        }
        let layout = Layout::from_size_align(config.size, config.align.max(1))
            .map_err(|_| AxError::BadState)?;
        Ok(Self {
            raw,
            pool: BlockBufferPool {
                dma: DeviceDma::new(config.dma_mask, axklib::dma::op()),
                size: layout.size(),
                align: layout.align(),
            },
        })
    }

    fn poll_until_complete(&mut self, request: RequestId) -> AxResult {
        loop {
            match self
                .raw
                .poll_request(request)
                .map_err(map_blk_err_to_ax_err)?
            {
                RequestStatus::Complete => return Ok(()),
                RequestStatus::Pending => core::hint::spin_loop(),
            }
        }
    }
}

impl BlockBufferPool {
    fn alloc(&self, direction: DmaDirection) -> AxResult<ContiguousArray<u8>> {
        self.dma
            .contiguous_array_zero_with_align(self.size, self.align, direction)
            .map_err(BlkError::from)
            .map_err(map_blk_err_to_ax_err)
    }
}

impl TryFrom<Device<PlatformBlockDevice>> for Block {
    type Error = AxError;

    fn try_from(base: Device<PlatformBlockDevice>) -> Result<Self, Self::Error> {
        let mut dev = base.lock().map_err(|_| AxError::BadState)?;
        let name = dev.name.clone();
        let irq_num = dev.irq_num;
        let interface = dev.interface.take().ok_or(AxError::BadState)?;

        #[cfg(feature = "irq")]
        let (queue, irq_handler) = {
            let interface = Arc::new(BlockInterfaceOwner::new(interface));
            let queue = interface.with_mut(|interface| {
                BlockQueue::new(interface.create_queue().ok_or(AxError::BadState)?)
            })?;
            let irq_handler = irq_num.map(|_| BlockIrqHandler::new(interface));
            (queue, irq_handler)
        };
        #[cfg(not(feature = "irq"))]
        let queue = {
            let mut interface = interface;
            BlockQueue::new(interface.create_queue().ok_or(AxError::BadState)?)?
        };
        drop(dev);

        #[cfg(feature = "irq")]
        let irq_num = if irq_handler.is_some() { irq_num } else { None };
        #[cfg(feature = "irq")]
        let irq_handler = irq_handler;
        #[cfg(not(feature = "irq"))]
        let irq_num = {
            let _ = irq_num;
            None
        };

        Ok(Self {
            name,
            irq_num,
            #[cfg(feature = "irq")]
            irq_handler,
            queue: SpinNoIrq::new(queue),
        })
    }
}

pub trait PlatformDeviceBlock {
    fn register_block<T: Interface>(self, dev: T);
    fn register_block_with_irq<T: Interface>(self, dev: T, irq_num: Option<usize>);
}

impl PlatformDeviceBlock for rdrive::PlatformDevice {
    fn register_block<T: Interface>(self, dev: T) {
        self.register_block_with_irq(dev, None);
    }

    fn register_block_with_irq<T: Interface>(self, dev: T, irq_num: Option<usize>) {
        let name = dev.name().to_string();
        self.register(PlatformBlockDevice::new(name, Box::new(dev), irq_num));
    }
}

pub fn decode_fdt_irq(interrupts: &[rdrive::probe::fdt::InterruptRef]) -> Option<usize> {
    let interrupt = interrupts.first()?;
    decode_irq_cells(&interrupt.specifier)
}

fn decode_irq_cells(specifier: &[u32]) -> Option<usize> {
    match specifier {
        [irq] => Some(*irq as usize),
        [kind, irq, ..] => match *kind {
            0 => Some(*irq as usize + 32),
            1 => Some(*irq as usize + 16),
            _ => Some(*irq as usize),
        },
        _ => None,
    }
}

pub fn take_block_devices() -> Vec<Block> {
    rdrive::get_list::<PlatformBlockDevice>()
        .into_iter()
        .filter_map(|dev| match Block::try_from(dev) {
            Ok(block) => Some(block),
            Err(err) => {
                warn!("failed to take block device: {err:?}");
                None
            }
        })
        .collect()
}

fn validate_io(queue: &dyn IQueue, block_id: u64, len: usize) -> AxResult {
    let block_size = queue.block_size();
    if block_size == 0 || !len.is_multiple_of(block_size) {
        return Err(AxError::InvalidInput);
    }
    let block_count = len / block_size;
    let end = block_id
        .checked_add(block_count as u64)
        .ok_or(AxError::InvalidInput)?;
    if end > queue.num_blocks() as u64 {
        return Err(AxError::InvalidInput);
    }
    Ok(())
}

fn checked_block_id(start: u64, offset: usize) -> AxResult<usize> {
    let block_id = start
        .checked_add(offset as u64)
        .ok_or(AxError::InvalidInput)?;
    usize::try_from(block_id).map_err(|_| AxError::InvalidInput)
}

fn buffer_from_dma(buffer: &mut ContiguousArray<u8>, len: usize) -> Buffer<'_> {
    unsafe { Buffer::from_raw_parts(buffer.as_ptr().as_ptr(), buffer.dma_addr().as_u64(), len) }
}

fn map_blk_err_to_ax_err(err: BlkError) -> AxError {
    match err {
        BlkError::NotSupported => AxError::Unsupported,
        BlkError::Retry => AxError::WouldBlock,
        BlkError::NoMemory => AxError::NoMemory,
        BlkError::InvalidBlockIndex(_) => AxError::InvalidInput,
        BlkError::Other(error) => {
            error!("Block device error: {error}");
            AxError::Io
        }
    }
}
