use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use ax_errno::{AxError, AxResult};
use log::warn;
use rd_block::BlkError;
use rdrive::Device;
use spin::Mutex;

pub struct Block {
    name: String,
    irq_num: Option<usize>,
    queue: Mutex<rd_block::CmdQueue>,
}

pub struct PlatformBlockDevice {
    name: String,
    block: Option<rd_block::Block>,
    irq_num: Option<usize>,
}

impl PlatformBlockDevice {
    fn new(name: String, block: rd_block::Block, irq_num: Option<usize>) -> Self {
        Self {
            name,
            block: Some(block),
            irq_num,
        }
    }
}

impl rdrive::DriverGeneric for PlatformBlockDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

impl Block {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn irq_num(&self) -> Option<usize> {
        self.irq_num
    }

    pub fn num_blocks(&self) -> u64 {
        self.queue.lock().num_blocks() as _
    }

    pub fn block_size(&self) -> usize {
        self.queue.lock().block_size()
    }

    pub fn flush(&mut self) -> AxResult {
        Ok(())
    }

    pub fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        let block_size = self.block_size();
        if block_size == 0 || !buf.len().is_multiple_of(block_size) {
            return Err(AxError::InvalidInput);
        }

        let mut queue = self.queue.lock();
        for (offset, chunk) in buf.chunks_mut(block_size).enumerate() {
            let mut blocks = queue.read_blocks_blocking(block_id as usize + offset, 1);
            let block = blocks
                .pop()
                .ok_or(AxError::Io)?
                .map_err(map_blk_err_to_ax_err)?;
            if block.len() != chunk.len() {
                return Err(AxError::Io);
            }
            chunk.copy_from_slice(&block);
        }
        Ok(())
    }

    pub fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        let block_size = self.block_size();
        if block_size == 0 || !buf.len().is_multiple_of(block_size) {
            return Err(AxError::InvalidInput);
        }

        let mut queue = self.queue.lock();
        for (offset, chunk) in buf.chunks(block_size).enumerate() {
            for block in queue.write_blocks_blocking(block_id as usize + offset, chunk) {
                block.map_err(map_blk_err_to_ax_err)?;
            }
        }
        Ok(())
    }
}

impl TryFrom<Device<PlatformBlockDevice>> for Block {
    type Error = AxError;

    fn try_from(base: Device<PlatformBlockDevice>) -> Result<Self, Self::Error> {
        let mut dev = base.lock().map_err(|_| AxError::BadState)?;
        let name = dev.name.clone();
        let irq_num = dev.irq_num;
        let mut block = dev.block.take().ok_or(AxError::BadState)?;
        let queue = block.create_queue().ok_or(AxError::BadState)?;
        Ok(Self {
            name,
            irq_num,
            queue: Mutex::new(queue),
        })
    }
}

pub trait PlatformDeviceBlock {
    fn register_block<T: rd_block::Interface>(self, dev: T);
    fn register_block_with_irq<T: rd_block::Interface>(self, dev: T, irq_num: Option<usize>);
}

impl PlatformDeviceBlock for rdrive::PlatformDevice {
    fn register_block<T: rd_block::Interface>(self, dev: T) {
        self.register_block_with_irq(dev, None);
    }

    fn register_block_with_irq<T: rd_block::Interface>(self, dev: T, irq_num: Option<usize>) {
        let mut dev = dev;
        if irq_num.is_some() {
            dev.enable_irq();
        }
        let name = dev.name().to_string();
        let block = rd_block::Block::new(dev, axklib::dma::op());
        self.register(PlatformBlockDevice::new(name, block, irq_num));
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

fn map_blk_err_to_ax_err(err: BlkError) -> AxError {
    match err {
        BlkError::NotSupported => AxError::Unsupported,
        BlkError::Retry => AxError::WouldBlock,
        BlkError::NoMemory => AxError::NoMemory,
        BlkError::InvalidBlockIndex(_) => AxError::InvalidInput,
        BlkError::Other(_) => AxError::Io,
    }
}
