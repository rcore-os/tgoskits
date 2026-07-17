//! Synchronous filesystem-facing block service.
//!
//! Queue submission, DMA ownership, IRQ handling, completion routing, and
//! watchdog recovery belong to the runtime that implements [`BlockDevice`].
//! Filesystem code only observes a blocking, thread-safe device service.

#[cfg(any(feature = "ext4", feature = "fat"))]
use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};

/// Immutable geometry exposed by a ready block device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockDeviceMetadata {
    num_blocks: u64,
    block_size: usize,
}

impl BlockDeviceMetadata {
    /// Validates and constructs block geometry.
    ///
    /// # Errors
    ///
    /// Returns [`AxError::InvalidInput`] when the device has no addressable
    /// blocks or its logical block size is zero or not a power of two.
    pub fn new(num_blocks: u64, block_size: usize) -> AxResult<Self> {
        if num_blocks == 0 || block_size == 0 || !block_size.is_power_of_two() {
            return Err(AxError::InvalidInput);
        }
        Ok(Self {
            num_blocks,
            block_size,
        })
    }

    /// Returns the number of addressable logical blocks.
    pub const fn num_blocks(self) -> u64 {
        self.num_blocks
    }

    /// Returns the logical block size in bytes.
    pub const fn block_size(self) -> usize {
        self.block_size
    }

    /// Checks that a byte buffer describes an aligned, in-range transfer.
    ///
    /// # Errors
    ///
    /// Returns [`AxError::InvalidInput`] for an empty or unaligned buffer, an
    /// arithmetic overflow, or a transfer beyond the published capacity.
    pub fn validate_transfer(self, start_block: u64, buffer_len: usize) -> AxResult {
        if buffer_len == 0 || !buffer_len.is_multiple_of(self.block_size) {
            return Err(AxError::InvalidInput);
        }
        let block_count =
            u64::try_from(buffer_len / self.block_size).map_err(|_| AxError::InvalidInput)?;
        let end_block = start_block
            .checked_add(block_count)
            .ok_or(AxError::InvalidInput)?;
        if end_block > self.num_blocks {
            return Err(AxError::InvalidInput);
        }
        Ok(())
    }
}

/// Blocking block service consumed by filesystem implementations.
///
/// The implementation may submit an inline request or wait for an IRQ-driven
/// runtime request, but it must not expose that distinction to this boundary.
/// Inline devices complete in the calling stack. Interrupt-backed devices wait
/// only on the accepted request's generation-scoped completion; a global drain
/// notification or completion polling is not a valid implementation. Every
/// accepted call returns only after its request reaches one terminal result.
/// Implementations must serialize or parallelize calls according to the
/// underlying queue contract. A returned error is terminal for that request;
/// filesystem code must propagate it instead of blindly resubmitting because
/// only the block runtime owns controller recovery and queue-epoch changes.
pub trait BlockDevice: Send + Sync {
    /// Returns a stable diagnostic name for the device.
    fn name(&self) -> &str;

    /// Returns immutable geometry published after device activation.
    fn metadata(&self) -> BlockDeviceMetadata;

    /// Reads one or more complete logical blocks.
    fn read_blocks(&self, start_block: u64, buffer: &mut [u8]) -> AxResult;

    /// Writes one or more complete logical blocks.
    fn write_blocks(&self, start_block: u64, buffer: &[u8]) -> AxResult;

    /// Persists all previously accepted writes.
    fn flush(&self) -> AxResult;
}

/// A contiguous logical block range within a device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockRegion {
    pub start_lba: u64,
    pub end_lba: u64,
}

impl BlockRegion {
    /// Covers a complete device with `num_blocks` logical blocks.
    pub const fn from_num_blocks(num_blocks: u64) -> Self {
        Self {
            start_lba: 0,
            end_lba: num_blocks,
        }
    }

    /// Constructs a range from a start and length.
    ///
    /// An overflowing end wraps below `start_lba`, deliberately encoding an
    /// invalid range that validated consumers reject instead of silently
    /// truncating the requested capacity.
    pub const fn new(start_lba: u64, num_blocks: u64) -> Self {
        Self {
            start_lba,
            end_lba: start_lba.wrapping_add(num_blocks),
        }
    }

    /// Returns the number of logical blocks in the range.
    pub const fn num_blocks(self) -> u64 {
        self.end_lba.saturating_sub(self.start_lba)
    }
}

/// Restricts a block service to one validated region.
#[cfg(any(feature = "ext4", feature = "fat"))]
pub(crate) struct RegionBlockDevice {
    inner: Arc<dyn BlockDevice>,
    region: BlockRegion,
}

#[cfg(any(feature = "ext4", feature = "fat"))]
impl RegionBlockDevice {
    pub(crate) fn new(inner: Arc<dyn BlockDevice>, region: BlockRegion) -> AxResult<Self> {
        let metadata = inner.metadata();
        BlockDeviceMetadata::new(region.num_blocks(), metadata.block_size())?;
        if region.start_lba > region.end_lba || region.end_lba > metadata.num_blocks() {
            return Err(AxError::InvalidInput);
        }
        Ok(Self { inner, region })
    }

    fn physical_start(&self, block_id: u64, buffer_len: usize) -> AxResult<u64> {
        let inner = self.inner.metadata();
        let region_blocks = self.region.num_blocks();
        let region_metadata = BlockDeviceMetadata::new(region_blocks, inner.block_size())?;
        region_metadata.validate_transfer(block_id, buffer_len)?;

        let physical = self
            .region
            .start_lba
            .checked_add(block_id)
            .ok_or(AxError::InvalidInput)?;
        inner.validate_transfer(physical, buffer_len)?;
        Ok(physical)
    }
}

#[cfg(any(feature = "ext4", feature = "fat"))]
impl BlockDevice for RegionBlockDevice {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn metadata(&self) -> BlockDeviceMetadata {
        // The constructor receives a region selected from validated volume
        // metadata. An empty region cannot be mounted and is rejected before
        // filesystem construction.
        BlockDeviceMetadata {
            num_blocks: self.region.num_blocks(),
            block_size: self.inner.metadata().block_size(),
        }
    }

    fn read_blocks(&self, start_block: u64, buffer: &mut [u8]) -> AxResult {
        let physical = self.physical_start(start_block, buffer.len())?;
        self.inner.read_blocks(physical, buffer)
    }

    fn write_blocks(&self, start_block: u64, buffer: &[u8]) -> AxResult {
        let physical = self.physical_start(start_block, buffer.len())?;
        self.inner.write_blocks(physical, buffer)
    }

    fn flush(&self) -> AxResult {
        self.inner.flush()
    }
}

#[cfg(all(test, any(feature = "ext4", feature = "fat")))]
mod tests {
    use alloc::{sync::Arc, vec};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use ax_kspin::SpinNoPreempt;

    use super::*;

    struct MemoryDevice {
        bytes: SpinNoPreempt<alloc::vec::Vec<u8>>,
        reads: AtomicUsize,
    }

    struct MaxGeometryDevice;

    impl BlockDevice for MaxGeometryDevice {
        fn name(&self) -> &str {
            "max-geometry"
        }

        fn metadata(&self) -> BlockDeviceMetadata {
            BlockDeviceMetadata::new(u64::MAX, 512).unwrap()
        }

        fn read_blocks(&self, _start_block: u64, _buffer: &mut [u8]) -> AxResult {
            unreachable!("overflowing regions must be rejected before I/O")
        }

        fn write_blocks(&self, _start_block: u64, _buffer: &[u8]) -> AxResult {
            unreachable!("overflowing regions must be rejected before I/O")
        }

        fn flush(&self) -> AxResult {
            Ok(())
        }
    }

    impl MemoryDevice {
        fn new() -> Self {
            Self {
                bytes: SpinNoPreempt::new(vec![0; 8 * 512]),
                reads: AtomicUsize::new(0),
            }
        }
    }

    impl BlockDevice for MemoryDevice {
        fn name(&self) -> &str {
            "memory"
        }

        fn metadata(&self) -> BlockDeviceMetadata {
            BlockDeviceMetadata::new(8, 512).unwrap()
        }

        fn read_blocks(&self, start_block: u64, buffer: &mut [u8]) -> AxResult {
            self.metadata()
                .validate_transfer(start_block, buffer.len())?;
            self.reads.fetch_add(1, Ordering::Relaxed);
            let start = start_block as usize * 512;
            buffer.copy_from_slice(&self.bytes.lock()[start..start + buffer.len()]);
            Ok(())
        }

        fn write_blocks(&self, start_block: u64, buffer: &[u8]) -> AxResult {
            self.metadata()
                .validate_transfer(start_block, buffer.len())?;
            let start = start_block as usize * 512;
            self.bytes.lock()[start..start + buffer.len()].copy_from_slice(buffer);
            Ok(())
        }

        fn flush(&self) -> AxResult {
            Ok(())
        }
    }

    #[test]
    fn region_maps_requests_without_exposing_runtime_details() {
        let inner = Arc::new(MemoryDevice::new());
        inner.write_blocks(3, &[0x5a; 512]).unwrap();
        let region = RegionBlockDevice::new(inner.clone(), BlockRegion::new(2, 4)).unwrap();
        let mut buffer = [0; 512];

        region.read_blocks(1, &mut buffer).unwrap();

        assert_eq!(buffer, [0x5a; 512]);
        assert_eq!(inner.reads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn region_rejects_unaligned_and_out_of_range_io() {
        let region =
            RegionBlockDevice::new(Arc::new(MemoryDevice::new()), BlockRegion::new(2, 4)).unwrap();

        assert_eq!(
            region.read_blocks(0, &mut [0; 511]),
            Err(AxError::InvalidInput)
        );
        assert_eq!(
            region.read_blocks(4, &mut [0; 512]),
            Err(AxError::InvalidInput)
        );
        assert!(matches!(
            RegionBlockDevice::new(Arc::new(MemoryDevice::new()), BlockRegion::new(7, 2)),
            Err(AxError::InvalidInput)
        ));
    }

    #[test]
    fn region_rejects_lba_range_overflow_instead_of_truncating_it() {
        assert!(matches!(
            RegionBlockDevice::new(
                Arc::new(MaxGeometryDevice),
                BlockRegion::new(u64::MAX - 1, 4),
            ),
            Err(AxError::InvalidInput)
        ));
    }
}
