use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use core::alloc::Layout;

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use dma_api::{ContiguousArray, DeviceDma, DmaDirection};
use log::{error, warn};
use rdif_block::{
    BlkError, Buffer, IReadQueue, IWriteQueue, Interface, QueueInfo, RequestId, RequestRead,
    RequestStatus, RequestWrite,
};
use rdrive::Device;

pub struct Block {
    name: String,
    irq_num: Option<usize>,
    irq_enabled: bool,
    #[cfg(feature = "irq")]
    irq_handler: Option<BlockIrqHandler>,
    interface: Box<dyn Interface>,
    queues: SpinNoIrq<BlockQueues>,
}

struct BlockQueues {
    read: Box<dyn IReadQueue>,
    write: Box<dyn IWriteQueue>,
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
pub struct BlockIrqHandler {
    handler: Box<dyn rdif_block::IrqHandler>,
}

#[cfg(feature = "irq")]
impl BlockIrqHandler {
    fn new(handler: Box<dyn rdif_block::IrqHandler>) -> Self {
        Self { handler }
    }

    pub fn handle(&self) -> rdif_block::Event {
        self.handler.handle_irq()
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

    pub fn enable_irq(&mut self) {
        self.interface.enable_irq();
        self.irq_enabled = self.interface.is_irq_enabled();
    }

    pub fn disable_irq(&mut self) {
        self.interface.disable_irq();
        self.irq_enabled = self.interface.is_irq_enabled();
    }

    pub const fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
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
        self.queues.lock().read.num_blocks() as _
    }

    pub fn block_size(&self) -> usize {
        self.queues.lock().read.block_size()
    }

    pub fn flush(&mut self) -> AxResult {
        Ok(())
    }

    pub fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        let mut queues = self.queues.lock();
        validate_io(&*queues.read, block_id, buf.len())?;

        let block_size = queues.read.block_size();
        for (offset, block_buf) in buf.chunks_exact_mut(block_size).enumerate() {
            let mut dma_buffer = queues.pool.alloc(DmaDirection::FromDevice)?;
            let request = RequestRead {
                block_id: checked_block_id(block_id, offset)?,
                buffer: buffer_from_dma(&mut dma_buffer, block_size),
            };
            let request_id = queues
                .read
                .submit_read(request)
                .map_err(map_blk_err_to_ax_err)?;
            queues.poll_read_until_complete(request_id)?;
            dma_buffer.sync_for_cpu(0, block_size);
            dma_buffer.read_with(block_size, |data| block_buf.copy_from_slice(data));
        }
        Ok(())
    }

    pub fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        let mut queues = self.queues.lock();
        validate_io(&*queues.write, block_id, buf.len())?;

        let block_size = queues.write.block_size();
        for (offset, block_buf) in buf.chunks_exact(block_size).enumerate() {
            let mut dma_buffer = queues.pool.alloc(DmaDirection::ToDevice)?;
            dma_buffer.write_with(block_size, |data| data.copy_from_slice(block_buf));
            dma_buffer.sync_for_device(0, block_size);
            let request = RequestWrite {
                block_id: checked_block_id(block_id, offset)?,
                buffer: buffer_from_dma(&mut dma_buffer, block_size),
            };
            let request_id = queues
                .write
                .submit_write(request)
                .map_err(map_blk_err_to_ax_err)?;
            queues.poll_write_until_complete(request_id)?;
        }
        Ok(())
    }
}

impl BlockQueues {
    fn new(read: Box<dyn IReadQueue>, write: Box<dyn IWriteQueue>) -> AxResult<Self> {
        if read.block_size() != write.block_size() || read.num_blocks() != write.num_blocks() {
            return Err(AxError::BadState);
        }
        let config = read.buffer_config();
        let block_size = read.block_size();
        if block_size == 0 || config.size < block_size {
            return Err(AxError::BadState);
        }
        let layout = Layout::from_size_align(config.size, config.align.max(1))
            .map_err(|_| AxError::BadState)?;
        Ok(Self {
            read,
            write,
            pool: BlockBufferPool {
                dma: DeviceDma::new(config.dma_mask, axklib::dma::op()),
                size: layout.size(),
                align: layout.align(),
            },
        })
    }

    fn poll_read_until_complete(&mut self, request: RequestId) -> AxResult {
        loop {
            match self
                .read
                .poll_read(request)
                .map_err(map_blk_err_to_ax_err)?
            {
                RequestStatus::Complete => return Ok(()),
                RequestStatus::Pending => core::hint::spin_loop(),
            }
        }
    }

    fn poll_write_until_complete(&mut self, request: RequestId) -> AxResult {
        loop {
            match self
                .write
                .poll_write(request)
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
        let mut interface = dev.interface.take().ok_or(AxError::BadState)?;
        let read = interface.create_read_queue().ok_or(AxError::BadState)?;
        let write = interface.create_write_queue().ok_or(AxError::BadState)?;
        let queues = BlockQueues::new(read, write)?;

        #[cfg(feature = "irq")]
        let irq_handler = irq_num
            .and_then(|_| interface.take_irq_handler())
            .map(BlockIrqHandler::new);
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
            irq_enabled: interface.is_irq_enabled(),
            #[cfg(feature = "irq")]
            irq_handler,
            interface,
            queues: SpinNoIrq::new(queues),
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

fn validate_io(queue: &dyn QueueInfo, block_id: u64, len: usize) -> AxResult {
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
