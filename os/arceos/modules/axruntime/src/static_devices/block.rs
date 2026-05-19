use alloc::{boxed::Box, string::String, vec::Vec};

use ax_errno::{AxError, AxResult};
use rd_block::BlkError;
use rdrive::DriverGeneric;
use spin::Mutex;

use crate::static_devices::dma::IDENTITY_DMA;

pub(super) struct StaticBlockDevice {
    name: String,
    queue: Mutex<rd_block::CmdQueue>,
}

impl StaticBlockDevice {
    pub(super) fn new(mut block: rd_block::Block) -> Result<Self, AxError> {
        let name = block.name().into();
        let queue = block.create_queue().ok_or(AxError::BadState)?;
        Ok(Self {
            name,
            queue: Mutex::new(queue),
        })
    }

    fn read_blocks(
        queue: &mut rd_block::CmdQueue,
        block_id: usize,
        block_count: usize,
    ) -> Vec<Result<rd_block::BlockData, BlkError>> {
        queue.read_blocks_blocking(block_id, block_count)
    }

    fn write_blocks(
        queue: &mut rd_block::CmdQueue,
        block_id: usize,
        data: &[u8],
    ) -> Vec<Result<(), BlkError>> {
        queue.write_blocks_blocking(block_id, data)
    }
}

impl StaticBlockDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_blocks(&self) -> u64 {
        self.queue.lock().num_blocks() as _
    }

    fn block_size(&self) -> usize {
        self.queue.lock().block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        let block_size = self.block_size();
        if block_size == 0 || !buf.len().is_multiple_of(block_size) {
            return Err(AxError::InvalidInput);
        }

        let mut queue = self.queue.lock();
        for (offset, chunk) in buf.chunks_mut(block_size).enumerate() {
            let mut blocks = Self::read_blocks(&mut queue, block_id as usize + offset, 1);
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

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        let block_size = self.block_size();
        if block_size == 0 || !buf.len().is_multiple_of(block_size) {
            return Err(AxError::InvalidInput);
        }

        let mut queue = self.queue.lock();
        for (offset, chunk) in buf.chunks(block_size).enumerate() {
            for block in Self::write_blocks(&mut queue, block_id as usize + offset, chunk) {
                block.map_err(map_blk_err_to_ax_err)?;
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> AxResult {
        Ok(())
    }
}

#[cfg(feature = "fs")]
impl ax_fs::FsBlockDevice for StaticBlockDevice {
    fn name(&self) -> &str {
        self.name()
    }

    fn num_blocks(&self) -> u64 {
        self.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        StaticBlockDevice::read_block(self, block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        StaticBlockDevice::write_block(self, block_id, buf)
    }

    fn flush(&mut self) -> AxResult {
        StaticBlockDevice::flush(self)
    }
}

#[cfg(feature = "fs-ng")]
impl ax_fs_ng::FsBlockDevice for StaticBlockDevice {
    fn name(&self) -> &str {
        self.name()
    }

    fn num_blocks(&self) -> u64 {
        self.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        StaticBlockDevice::read_block(self, block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        StaticBlockDevice::write_block(self, block_id, buf)
    }

    fn flush(&mut self) -> AxResult {
        StaticBlockDevice::flush(self)
    }
}

#[cfg(feature = "fs")]
pub(super) fn take_fs_block_devices() -> Vec<Box<dyn ax_fs::FsBlockDevice>> {
    take_static_blocks()
        .into_iter()
        .map(|dev| Box::new(dev) as Box<dyn ax_fs::FsBlockDevice>)
        .collect()
}

#[cfg(feature = "fs-ng")]
pub(super) fn take_fs_ng_block_devices() -> Vec<Box<dyn ax_fs_ng::FsBlockDevice>> {
    take_static_blocks()
        .into_iter()
        .map(|dev| Box::new(dev) as Box<dyn ax_fs_ng::FsBlockDevice>)
        .collect()
}

fn take_static_blocks() -> Vec<StaticBlockDevice> {
    rdrive::get_list::<rd_block::Block>()
        .into_iter()
        .map(|dev| {
            let mut guard = dev
                .lock()
                .unwrap_or_else(|err| panic!("failed to lock static block device: {err:?}"));
            let block =
                core::mem::replace(&mut *guard, rd_block::Block::new(EmptyBlock, &IDENTITY_DMA));
            StaticBlockDevice::new(block)
                .unwrap_or_else(|err| panic!("failed to adapt static block device: {err:?}"))
        })
        .collect()
}

struct EmptyBlock;

impl rdrive::DriverGeneric for EmptyBlock {
    fn name(&self) -> &str {
        "empty-block"
    }
}

impl rd_block::Interface for EmptyBlock {
    fn create_queue(&mut self) -> Option<Box<dyn rd_block::IQueue>> {
        None
    }

    fn enable_irq(&mut self) {}

    fn disable_irq(&mut self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn handle_irq(&mut self) -> rd_block::Event {
        rd_block::Event::none()
    }
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
