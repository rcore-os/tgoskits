mod fs;
mod inode;
mod util;

#[allow(unused_imports)]
use ax_driver::{AxBlockDevice, prelude::BlockDriverOps};
use ax_driver_block::partition::PartitionRegion;
pub use fs::*;
pub use inode::*;
use lwext4_rust::{BlockDevice, Ext4Error, Ext4Result, ffi::EIO};

pub(crate) struct Ext4Disk {
    dev: AxBlockDevice,
    region: Option<PartitionRegion>,
}

impl Ext4Disk {
    pub(crate) const fn new(dev: AxBlockDevice, region: Option<PartitionRegion>) -> Self {
        Self { dev, region }
    }

    fn block_size(&self) -> usize {
        self.dev.block_size()
    }

    fn visible_blocks(&self) -> u64 {
        self.region
            .map_or_else(|| self.dev.num_blocks(), PartitionRegion::num_blocks)
    }

    fn translate_range(&self, block_id: u64, buf_len: usize) -> Ext4Result<u64> {
        let block_size = self.block_size();
        if block_size == 0 || !buf_len.is_multiple_of(block_size) {
            return Err(Ext4Error::new(EIO as _, None));
        }

        let blocks =
            u64::try_from(buf_len / block_size).map_err(|_| Ext4Error::new(EIO as _, None))?;
        let end_block = block_id
            .checked_add(blocks)
            .ok_or_else(|| Ext4Error::new(EIO as _, None))?;
        if end_block > self.visible_blocks() {
            return Err(Ext4Error::new(EIO as _, None));
        }

        Ok(self
            .region
            .map_or(block_id, |region| region.start_lba + block_id))
    }
}

impl BlockDevice for Ext4Disk {
    fn read_blocks(&mut self, block_id: u64, buf: &mut [u8]) -> Ext4Result<usize> {
        let translated = self.translate_range(block_id, buf.len())?;
        self.dev
            .read_block(translated, buf)
            .map_err(|_| Ext4Error::new(EIO as _, None))?;
        Ok(buf.len())
    }

    fn write_blocks(&mut self, block_id: u64, buf: &[u8]) -> Ext4Result<usize> {
        let translated = self.translate_range(block_id, buf.len())?;
        self.dev
            .write_block(translated, buf)
            .map_err(|_| Ext4Error::new(EIO as _, None))?;
        Ok(buf.len())
    }

    fn num_blocks(&self) -> Ext4Result<u64> {
        Ok(self.visible_blocks())
    }
}
