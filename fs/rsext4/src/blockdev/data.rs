//! Coherent independent file-data endpoint without journal/metadata access.

use super::{cached_device::BlockDev, *};

pub(crate) struct FileDataEndpoint<B: BlockIo> {
    device: BlockDev<B>,
}

impl<B: BlockIo> FileDataEndpoint<B> {
    pub(super) fn new(endpoint: B, block_size: usize) -> Ext4Result<Self> {
        let mut device = BlockDev::new(endpoint);
        device.set_filesystem_block_size(block_size)?;
        Ok(Self { device })
    }

    pub(crate) fn read_blocks(
        &mut self,
        bytes: &mut [u8],
        block: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<()> {
        self.device.read_blocks(bytes, block, count)
    }

    pub(crate) fn write_blocks(
        &mut self,
        bytes: &[u8],
        block: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<()> {
        self.device.write_blocks(bytes, block, count)
    }
}
