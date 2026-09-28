//! Partition bounds remain enforced on independently forked I/O endpoints.

use alloc::vec::Vec;
use std::sync::Mutex;

use super::*;

#[test]
fn forked_partition_reads_translate_the_original_relative_sector_once() {
    let device = RecordingDevice::new(512);
    let observed = device.reads.clone();
    let partition = RegionBlockDevice::new(device, BlockRegion::new(17, 8));
    let mut first = partition.fork_region().unwrap();
    let mut second = first.fork_io().unwrap();
    let mut bytes = [0; 1024];

    first.read_block(1, &mut bytes).unwrap();
    assert_eq!(bytes, [0x5a; 1024]);
    second.read_block(2, &mut bytes).unwrap();

    assert_eq!(*observed.lock().unwrap(), [(18, 1024), (19, 1024)]);
    assert_eq!(second.num_blocks(), 8);
}

#[test]
fn forked_partition_rejects_end_alignment_overflow_and_zero_geometry_without_io() {
    let device = RecordingDevice::new(512);
    let observed = device.reads.clone();
    let partition = RegionBlockDevice::new(device, BlockRegion::new(17, 8));
    let mut endpoint = partition.fork_region().unwrap();

    assert_eq!(
        endpoint.read_block(7, &mut [0; 1024]),
        Err(BlockError::InvalidRequest)
    );
    assert_eq!(
        endpoint.read_block(0, &mut [0; 1]),
        Err(BlockError::InvalidRequest)
    );
    assert_eq!(
        endpoint.read_block(u64::MAX, &mut [0; 512]),
        Err(BlockError::InvalidState)
    );
    assert_eq!(
        endpoint.read_block(9, &mut []),
        Err(BlockError::InvalidRequest)
    );
    assert!(observed.lock().unwrap().is_empty());

    let invalid = RecordingDevice::new(0);
    let observed = invalid.reads.clone();
    let mut endpoint = RegionBlockDevice::new(invalid, BlockRegion::new(17, 8))
        .fork_region()
        .unwrap();
    assert_eq!(
        endpoint.read_block(0, &mut [0; 512]),
        Err(BlockError::InvalidRequest)
    );
    assert!(observed.lock().unwrap().is_empty());
}

#[derive(Clone)]
struct RecordingDevice {
    block_size: usize,
    reads: Arc<Mutex<Vec<(u64, usize)>>>,
}

impl RecordingDevice {
    fn new(block_size: usize) -> Self {
        Self {
            block_size,
            reads: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl FsBlockDevice for RecordingDevice {
    fn fork_io(&self) -> BlockResult<Box<dyn FsBlockDevice>> {
        Ok(Box::new(self.clone()))
    }

    fn name(&self) -> &str {
        "partition-fork"
    }

    fn num_blocks(&self) -> u64 {
        64
    }

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn read_block(&mut self, block_id: u64, bytes: &mut [u8]) -> BlockResult {
        self.reads.lock().unwrap().push((block_id, bytes.len()));
        bytes.fill(0x5a);
        Ok(())
    }

    fn write_block(&mut self, _: u64, _: &[u8]) -> BlockResult {
        unreachable!("read-only fixture")
    }

    fn flush(&mut self) -> BlockResult {
        unreachable!("read-only fixture")
    }
}
