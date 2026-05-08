mod fs;
mod inode;
mod util;

use ax_driver::{AxBlockDevice, PartitionBlockDevice, PartitionRegion, prelude::BlockDriverOps};
pub use fs::*;
pub use inode::*;
use lwext4_rust::{BlockDevice, Ext4Error, Ext4Result, ffi::EIO};

pub(crate) struct Ext4Disk(PartitionBlockDevice<AxBlockDevice>);

impl Ext4Disk {
    pub(crate) const fn new(dev: AxBlockDevice, region: PartitionRegion) -> Self {
        Self(PartitionBlockDevice::new(dev, region))
    }

    fn check_buffer_len(&self, buf_len: usize) -> Ext4Result<()> {
        let block_size = self.0.block_size();
        if block_size == 0 || !buf_len.is_multiple_of(block_size) {
            return Err(Ext4Error::new(EIO as _, None));
        }
        Ok(())
    }
}

impl BlockDevice for Ext4Disk {
    fn read_blocks(&mut self, block_id: u64, buf: &mut [u8]) -> Ext4Result<usize> {
        self.check_buffer_len(buf.len())?;
        self.0
            .read_block(block_id, buf)
            .map_err(|_| Ext4Error::new(EIO as _, None))?;
        Ok(buf.len())
    }

    fn write_blocks(&mut self, block_id: u64, buf: &[u8]) -> Ext4Result<usize> {
        self.check_buffer_len(buf.len())?;
        self.0
            .write_block(block_id, buf)
            .map_err(|_| Ext4Error::new(EIO as _, None))?;
        Ok(buf.len())
    }

    fn num_blocks(&self) -> Ext4Result<u64> {
        Ok(self.0.num_blocks())
    }
}
