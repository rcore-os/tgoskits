mod fs;
mod inode;
mod util;
#[cfg(test)]
mod writeback_tests;

use alloc::sync::Arc;

pub use fs::*;
pub use inode::*;
use rsext4::{
    BlockDevice,
    bmalloc::AbsoluteBN,
    config::BLOCK_SIZE,
    disknode::Ext4Timestamp,
    error::{Ext4Error, Ext4Result},
};

use crate::block::{BlockDevice as FsBlockDevice, BlockRegion, RegionBlockDevice};

pub(crate) struct Ext4Disk(RegionBlockDevice);

impl Ext4Disk {
    pub fn new(dev: Arc<dyn FsBlockDevice>, region: BlockRegion) -> ax_errno::AxResult<Self> {
        Ok(Self(RegionBlockDevice::new(dev, region)?))
    }
}

impl BlockDevice for Ext4Disk {
    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let dev_block = self.0.metadata().block_size();
        if !BLOCK_SIZE.is_multiple_of(dev_block) {
            return Err(Ext4Error::invalid_input());
        }
        let factor = (BLOCK_SIZE / dev_block) as u64;
        let required_size = BLOCK_SIZE * count as usize;
        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        let start_block = block_id.raw() * factor;
        self.0
            .write_blocks(start_block, &buffer[..required_size])
            .map_err(|_| Ext4Error::io())
    }

    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let dev_block = self.0.metadata().block_size();
        if !BLOCK_SIZE.is_multiple_of(dev_block) {
            return Err(Ext4Error::invalid_input());
        }
        let factor = (BLOCK_SIZE / dev_block) as u64;
        let required_size = BLOCK_SIZE * count as usize;
        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        let start_block = block_id.raw() * factor;
        self.0
            .read_blocks(start_block, &mut buffer[..required_size])
            .map_err(|_| Ext4Error::io())
    }

    fn open(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
        self.flush()
    }

    fn total_blocks(&self) -> u64 {
        let metadata = self.0.metadata();
        let dev_block = metadata.block_size() as u64;
        let total_bytes = metadata.num_blocks().saturating_mul(dev_block);
        total_bytes / BLOCK_SIZE as u64
    }

    fn block_size(&self) -> u32 {
        BLOCK_SIZE as u32
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.0.flush().map_err(|_| Ext4Error::io())
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let dur = crate::os::wall_time();
        Ok(Ext4Timestamp::new(dur.as_secs() as i64, dur.subsec_nanos()))
    }
}
