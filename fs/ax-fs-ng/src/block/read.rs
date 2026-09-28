//! Independent filesystem reads through the existing block runtime.

use alloc::sync::Arc;

use super::{BlockRegion, runtime::BlockDeviceHandle};
use crate::BlockResult;

/// A reader whose requests retain the backing device's completion and flush ordering.
pub(crate) trait FsBlockReader: Send + Sync {
    fn read_block(&self, block_id: u64, buf: &mut [u8]) -> BlockResult;
}

pub(super) struct RegionBlockReader {
    inner: Arc<dyn FsBlockReader>,
    region: BlockRegion,
    block_size: usize,
}

impl RegionBlockReader {
    pub(super) fn new(
        inner: Arc<dyn FsBlockReader>,
        region: BlockRegion,
        block_size: usize,
    ) -> Self {
        Self {
            inner,
            region,
            block_size,
        }
    }
}

impl FsBlockReader for RegionBlockReader {
    fn read_block(&self, block_id: u64, buf: &mut [u8]) -> BlockResult {
        let physical = self
            .region
            .checked_lba(self.block_size, block_id, buf.len())?;
        self.inner.read_block(physical, buf)
    }
}

impl FsBlockReader for BlockDeviceHandle {
    fn read_block(&self, block_id: u64, buf: &mut [u8]) -> BlockResult {
        self.read_blocks(block_id, buf)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::BlockError;

    #[test]
    fn independent_reader_translates_partition_relative_sectors() {
        let device = Arc::new(Reader::default());
        let partition = RegionBlockReader::new(device.clone(), BlockRegion::new(17, 8), 512);
        let mut bytes = [0; 1024];
        partition.read_block(1, &mut bytes).unwrap();
        assert_eq!(bytes, [0x5a; 1024]);
        assert_eq!(*device.reads.lock().unwrap(), [(18, 1024)]);
    }

    #[test]
    fn independent_reader_rejects_bounds_alignment_and_overflow_before_io() {
        let device = Arc::new(Reader::default());
        let partition = RegionBlockReader::new(device.clone(), BlockRegion::new(17, 8), 512);
        assert_eq!(
            partition.read_block(7, &mut [0; 1024]),
            Err(BlockError::InvalidRequest)
        );
        assert_eq!(
            partition.read_block(0, &mut [0; 1]),
            Err(BlockError::InvalidRequest)
        );
        assert_eq!(
            partition.read_block(u64::MAX, &mut [0; 512]),
            Err(BlockError::InvalidState)
        );
        let invalid = RegionBlockReader::new(device.clone(), BlockRegion::new(17, 8), 0);
        assert_eq!(
            invalid.read_block(0, &mut [0; 512]),
            Err(BlockError::InvalidRequest)
        );
        assert!(device.reads.lock().unwrap().is_empty());
    }

    #[derive(Default)]
    struct Reader {
        reads: std::sync::Mutex<Vec<(u64, usize)>>,
    }

    impl FsBlockReader for Reader {
        fn read_block(&self, block_id: u64, bytes: &mut [u8]) -> BlockResult {
            self.reads.lock().unwrap().push((block_id, bytes.len()));
            bytes.fill(0x5a);
            Ok(())
        }
    }
}
