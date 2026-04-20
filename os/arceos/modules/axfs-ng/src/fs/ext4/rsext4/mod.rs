mod fs;
mod inode;
mod util;

use ax_driver::{AxBlockDevice, PartitionBlockDevice, PartitionRegion, prelude::BlockDriverOps};
pub use fs::*;
pub use inode::*;
use rsext4::{
    BlockDevice,
    bmalloc::AbsoluteBN,
    config::BLOCK_SIZE,
    disknode::Ext4Timestamp,
    error::{Ext4Error, Ext4Result},
};

pub(crate) struct Ext4Disk(PartitionBlockDevice<AxBlockDevice>);

impl Ext4Disk {
    pub(crate) const fn new(dev: AxBlockDevice, region: PartitionRegion) -> Self {
        Self(PartitionBlockDevice::new(dev, region))
    }
}

impl BlockDevice for Ext4Disk {
    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let dev_block = self.0.block_size();
        if !BLOCK_SIZE.is_multiple_of(dev_block as usize) {
            return Err(Ext4Error::invalid_input());
        }
        let factor = (BLOCK_SIZE / dev_block as usize) as u64;
        let required_size = BLOCK_SIZE * count as usize;
        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        let start_block = block_id.raw() * factor;
        self.0
            .write_block(start_block, &buffer[..required_size])
            .map_err(|_| Ext4Error::io())
    }

    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let dev_block = self.0.block_size();
        if !BLOCK_SIZE.is_multiple_of(dev_block as usize) {
            return Err(Ext4Error::invalid_input());
        }
        let factor = (BLOCK_SIZE / dev_block as usize) as u64;
        let required_size = BLOCK_SIZE * count as usize;
        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        let start_block = block_id.raw() * factor;
        self.0
            .read_block(start_block, &mut buffer[..required_size])
            .map_err(|_| Ext4Error::io())
    }

    fn open(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
        self.flush()
    }

    fn total_blocks(&self) -> u64 {
        let dev_block = self.0.block_size() as u64;
        let total_bytes = self.0.num_blocks().saturating_mul(dev_block);
        total_bytes / BLOCK_SIZE as u64
    }

    fn block_size(&self) -> u32 {
        BLOCK_SIZE as u32
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.0.flush().map_err(|_| Ext4Error::io())
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        if cfg!(feature = "times") {
            let dur = ax_hal::time::wall_time();
            Ok(Ext4Timestamp::new(dur.as_secs() as i64, dur.subsec_nanos()))
        } else {
            Ok(Ext4Timestamp::new(0, 0))
        }
    }
}
