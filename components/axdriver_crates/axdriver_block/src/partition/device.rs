use super::PartitionRegion;
use crate::{BaseDriverOps, BlockDriverOps, DevError, DevResult, DeviceType};

pub struct PartitionBlockDevice<T> {
    inner: T,
    region: PartitionRegion,
}

impl<T: BlockDriverOps> PartitionBlockDevice<T> {
    pub const fn new(inner: T, region: PartitionRegion) -> Self {
        Self { inner, region }
    }

    pub const fn region(&self) -> PartitionRegion {
        self.region
    }

    fn check_io_bounds(&self, block_id: u64, buf_len: usize) -> DevResult {
        let block_size = self.inner.block_size();
        if block_size == 0 || !buf_len.is_multiple_of(block_size) {
            return Err(DevError::InvalidParam);
        }

        let blocks = u64::try_from(buf_len / block_size).map_err(|_| DevError::BadState)?;
        let end_block = block_id.checked_add(blocks).ok_or(DevError::BadState)?;
        if end_block > self.num_blocks() {
            return Err(DevError::InvalidParam);
        }

        Ok(())
    }
}

impl<T: BlockDriverOps> BaseDriverOps for PartitionBlockDevice<T> {
    fn device_name(&self) -> &str {
        self.inner.device_name()
    }

    fn device_type(&self) -> DeviceType {
        self.inner.device_type()
    }

    fn irq_num(&self) -> Option<usize> {
        self.inner.irq_num()
    }
}

impl<T: BlockDriverOps> BlockDriverOps for PartitionBlockDevice<T> {
    fn num_blocks(&self) -> u64 {
        self.region.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> DevResult {
        self.check_io_bounds(block_id, buf.len())?;
        self.inner.read_block(self.region.start_lba + block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> DevResult {
        self.check_io_bounds(block_id, buf.len())?;
        self.inner
            .write_block(self.region.start_lba + block_id, buf)
    }

    fn flush(&mut self) -> DevResult {
        self.inner.flush()
    }
}
