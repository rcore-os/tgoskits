use alloc::{boxed::Box, sync::Arc};

#[cfg(any(feature = "ext4", feature = "fat"))]
use crate::BlockError;
use crate::BlockResult;

pub mod runtime;

#[cfg(all(feature = "ext4", feature = "vfs"))]
pub(crate) mod file_image;

#[cfg(all(test, feature = "ext4"))]
mod region_tests;

#[cfg(any(feature = "ext4", feature = "fat"))]
pub(crate) mod cache;

#[cfg(any(feature = "ext4", feature = "fat"))]
use cache::BufferedBlockDevice;
#[cfg(any(feature = "ext4", feature = "fat"))]
pub use cache::sync_all_block_caches;
use runtime::BlockDeviceHandle;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockRegion {
    pub start_lba: u64,
    pub end_lba: u64,
}

impl BlockRegion {
    pub const fn from_num_blocks(num_blocks: u64) -> Self {
        Self {
            start_lba: 0,
            end_lba: num_blocks,
        }
    }

    pub const fn new(start_lba: u64, num_blocks: u64) -> Self {
        Self {
            start_lba,
            end_lba: start_lba.saturating_add(num_blocks),
        }
    }

    pub const fn num_blocks(self) -> u64 {
        self.end_lba.saturating_sub(self.start_lba)
    }
}

/// Validates the complete zeroing request before mutation and returns its
/// conservative bounding span. Empty extents are permitted and ignored.
#[cfg(feature = "ext4")]
pub(crate) fn zero_range_bounds(
    ranges: &[BlockRegion],
    capacity: u64,
) -> BlockResult<Option<BlockRegion>> {
    let mut bounds: Option<BlockRegion> = None;
    for range in ranges {
        if range.end_lba < range.start_lba || range.end_lba > capacity {
            return Err(BlockError::InvalidRequest);
        }
        if range.start_lba == range.end_lba {
            continue;
        }
        match &mut bounds {
            Some(bounds) => {
                bounds.start_lba = bounds.start_lba.min(range.start_lba);
                bounds.end_lba = bounds.end_lba.max(range.end_lba);
            }
            None => bounds = Some(*range),
        }
    }
    Ok(bounds)
}

pub(crate) trait FsBlockDevice: Send {
    fn name(&self) -> &str;
    fn num_blocks(&self) -> u64;
    fn block_size(&self) -> usize;
    #[cfg(feature = "ext4")]
    fn physical_block_size(&self) -> usize {
        self.block_size()
    }
    #[cfg(feature = "ext4")]
    fn is_read_only(&self) -> bool {
        false
    }
    #[cfg(feature = "ext4")]
    fn supports_flush(&self) -> bool {
        true
    }
    #[cfg(feature = "ext4")]
    fn supports_fua(&self) -> bool {
        false
    }
    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult;
    /// Independent endpoint for the same device and coherent cache domain.
    #[cfg(feature = "ext4")]
    fn fork_io(&self) -> BlockResult<Box<dyn FsBlockDevice>> {
        Err(BlockError::Unsupported)
    }
    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> BlockResult;
    /// Initializes all validated logical-block ranges to zero.
    /// Gaps remain untouched. Failure may be partial; no accepted I/O may remain
    /// live on return. This does not flush or imply discard guarantees.
    #[cfg(feature = "ext4")]
    fn write_zeroes(&mut self, ranges: &[BlockRegion]) -> BlockResult {
        if zero_range_bounds(ranges, self.num_blocks())?.is_none() {
            return Ok(());
        }
        if self.is_read_only() {
            return Err(BlockError::Unsupported);
        }
        let block_size = self.block_size();
        if block_size == 0 {
            return Err(BlockError::InvalidRequest);
        }
        let maximum = ranges
            .iter()
            .map(|range| range.num_blocks())
            .max()
            .unwrap_or(0);
        let buffered = (2 * 1024 * 1024 / block_size)
            .max(1)
            .min(usize::try_from(maximum).unwrap_or(usize::MAX));
        let size = buffered
            .checked_mul(block_size)
            .ok_or(BlockError::InvalidRequest)?;
        let mut zeroes = alloc::vec::Vec::new();
        zeroes
            .try_reserve_exact(size)
            .map_err(|_| BlockError::NoMemory)?;
        zeroes.resize(size, 0);
        for range in ranges {
            let mut block = range.start_lba;
            while block < range.end_lba {
                let count = (range.end_lba - block).min(buffered as u64) as usize;
                self.write_block(block, &zeroes[..count * block_size])?;
                block += count as u64;
            }
        }
        Ok(())
    }
    #[cfg(feature = "ext4")]
    fn write_block_fua(&mut self, _block_id: u64, _buf: &[u8]) -> BlockResult {
        Err(BlockError::Unsupported)
    }
    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn flush(&mut self) -> BlockResult;
}

impl<T: FsBlockDevice + ?Sized> FsBlockDevice for Box<T> {
    #[cfg(feature = "ext4")]
    fn write_zeroes(&mut self, ranges: &[BlockRegion]) -> BlockResult {
        (**self).write_zeroes(ranges)
    }
    #[cfg(feature = "ext4")]
    fn fork_io(&self) -> BlockResult<Box<dyn FsBlockDevice>> {
        (**self).fork_io()
    }
    fn name(&self) -> &str {
        (**self).name()
    }

    fn num_blocks(&self) -> u64 {
        (**self).num_blocks()
    }

    fn block_size(&self) -> usize {
        (**self).block_size()
    }

    #[cfg(feature = "ext4")]
    fn physical_block_size(&self) -> usize {
        (**self).physical_block_size()
    }

    #[cfg(feature = "ext4")]
    fn is_read_only(&self) -> bool {
        (**self).is_read_only()
    }

    #[cfg(feature = "ext4")]
    fn supports_flush(&self) -> bool {
        (**self).supports_flush()
    }

    #[cfg(feature = "ext4")]
    fn supports_fua(&self) -> bool {
        (**self).supports_fua()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult {
        (**self).read_block(block_id, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> BlockResult {
        (**self).write_block(block_id, buf)
    }

    #[cfg(feature = "ext4")]
    fn write_block_fua(&mut self, block_id: u64, buf: &[u8]) -> BlockResult {
        (**self).write_block_fua(block_id, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn flush(&mut self) -> BlockResult {
        (**self).flush()
    }
}

#[cfg(any(feature = "ext4", feature = "fat"))]
pub(crate) struct RegionBlockDevice<T> {
    inner: T,
    region: BlockRegion,
}

pub(crate) struct NativeHandleBlockDevice {
    handle: Arc<BlockDeviceHandle>,
}

impl NativeHandleBlockDevice {
    pub(crate) fn new(handle: Arc<BlockDeviceHandle>) -> Self {
        Self { handle }
    }
}

#[cfg(any(feature = "ext4", feature = "fat"))]
impl<T: FsBlockDevice> RegionBlockDevice<T> {
    pub const fn new(inner: T, region: BlockRegion) -> Self {
        Self { inner, region }
    }

    #[cfg(feature = "ext4")]
    pub(crate) fn fork_region(&self) -> BlockResult<RegionBlockDevice<Box<dyn FsBlockDevice>>> {
        Ok(RegionBlockDevice::new(self.inner.fork_io()?, self.region))
    }

    fn check_io_bounds(&self, block_id: u64, buf_len: usize) -> BlockResult {
        let block_size = self.inner.block_size();
        if block_size == 0 || !buf_len.is_multiple_of(block_size) {
            return Err(BlockError::InvalidRequest);
        }

        let blocks = u64::try_from(buf_len / block_size).map_err(|_| BlockError::InvalidState)?;
        let end_block = block_id
            .checked_add(blocks)
            .ok_or(BlockError::InvalidState)?;
        if end_block > self.num_blocks() {
            return Err(BlockError::InvalidRequest);
        }

        Ok(())
    }
}

#[cfg(any(feature = "ext4", feature = "fat"))]
impl<T: FsBlockDevice> FsBlockDevice for RegionBlockDevice<T> {
    #[cfg(feature = "ext4")]
    fn write_zeroes(&mut self, ranges: &[BlockRegion]) -> BlockResult {
        zero_range_bounds(ranges, self.num_blocks())?;
        let mut physical = alloc::vec::Vec::new();
        physical
            .try_reserve_exact(ranges.len())
            .map_err(|_| BlockError::NoMemory)?;
        for range in ranges {
            physical.push(BlockRegion {
                start_lba: self
                    .region
                    .start_lba
                    .checked_add(range.start_lba)
                    .ok_or(BlockError::InvalidRequest)?,
                end_lba: self
                    .region
                    .start_lba
                    .checked_add(range.end_lba)
                    .ok_or(BlockError::InvalidRequest)?,
            });
        }
        self.inner.write_zeroes(&physical)
    }
    #[cfg(feature = "ext4")]
    fn fork_io(&self) -> BlockResult<Box<dyn FsBlockDevice>> {
        Ok(Box::new(self.fork_region()?))
    }
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn num_blocks(&self) -> u64 {
        self.region.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    #[cfg(feature = "ext4")]
    fn physical_block_size(&self) -> usize {
        self.inner.physical_block_size()
    }

    #[cfg(feature = "ext4")]
    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    #[cfg(feature = "ext4")]
    fn supports_flush(&self) -> bool {
        self.inner.supports_flush()
    }

    #[cfg(feature = "ext4")]
    fn supports_fua(&self) -> bool {
        self.inner.supports_fua()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult {
        self.check_io_bounds(block_id, buf.len())?;
        let physical = self
            .region
            .start_lba
            .checked_add(block_id)
            .ok_or(BlockError::InvalidState)?;
        self.inner.read_block(physical, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> BlockResult {
        self.check_io_bounds(block_id, buf.len())?;
        let physical = self
            .region
            .start_lba
            .checked_add(block_id)
            .ok_or(BlockError::InvalidState)?;
        self.inner.write_block(physical, buf)
    }

    #[cfg(feature = "ext4")]
    fn write_block_fua(&mut self, block_id: u64, buf: &[u8]) -> BlockResult {
        self.check_io_bounds(block_id, buf.len())?;
        let physical = self
            .region
            .start_lba
            .checked_add(block_id)
            .ok_or(BlockError::InvalidState)?;
        self.inner.write_block_fua(physical, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn flush(&mut self) -> BlockResult {
        self.inner.flush()
    }
}

impl FsBlockDevice for NativeHandleBlockDevice {
    #[cfg(feature = "ext4")]
    fn write_zeroes(&mut self, ranges: &[BlockRegion]) -> BlockResult {
        self.handle.write_zeroes(ranges)
    }
    #[cfg(feature = "ext4")]
    fn fork_io(&self) -> BlockResult<Box<dyn FsBlockDevice>> {
        Ok(Box::new(Self::new(Arc::clone(&self.handle))))
    }
    fn name(&self) -> &str {
        self.handle.name()
    }

    fn num_blocks(&self) -> u64 {
        self.handle.device_info().num_blocks
    }

    fn block_size(&self) -> usize {
        self.handle.device_info().logical_block_size
    }

    #[cfg(feature = "ext4")]
    fn physical_block_size(&self) -> usize {
        self.handle.device_info().physical_block_size
    }

    #[cfg(feature = "ext4")]
    fn is_read_only(&self) -> bool {
        self.handle.device_info().read_only
    }

    #[cfg(feature = "ext4")]
    fn supports_flush(&self) -> bool {
        self.handle.supports_flush()
    }

    #[cfg(feature = "ext4")]
    fn supports_fua(&self) -> bool {
        self.handle.supports_fua()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult {
        self.handle.read_blocks(block_id, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> BlockResult {
        if self.handle.device_info().read_only {
            return Err(BlockError::Unsupported);
        }
        self.handle.write_blocks(block_id, buf)
    }

    #[cfg(feature = "ext4")]
    fn write_block_fua(&mut self, block_id: u64, buf: &[u8]) -> BlockResult {
        if self.is_read_only() {
            return Err(BlockError::Unsupported);
        }
        self.handle.write_blocks_fua(block_id, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn flush(&mut self) -> BlockResult {
        self.handle.flush_blocks()
    }
}

pub(crate) fn boxed_native_handle_block_device(
    handle: Arc<BlockDeviceHandle>,
) -> BlockResult<Box<dyn FsBlockDevice>> {
    #[cfg(any(feature = "ext4", feature = "fat"))]
    if let Some(cached) = cached_block_device(handle.clone())? {
        return Ok(cached);
    }
    // Also the fallback when the cache cannot wrap a malformed geometry,
    // and the only path when no filesystem consumer is enabled.
    Ok(Box::new(NativeHandleBlockDevice::new(handle)))
}

/// Wraps the device in its shared cache tree, keyed by the handle's
/// address. The wrapper keeps its own `Arc` clone alive, so the key stays
/// valid as long as any cached consumer of the device exists.
#[cfg(any(feature = "ext4", feature = "fat"))]
fn cached_block_device(
    handle: Arc<BlockDeviceHandle>,
) -> BlockResult<Option<Box<dyn FsBlockDevice>>> {
    let device_key = Arc::as_ptr(&handle) as usize;
    let endpoint = Box::new(NativeHandleBlockDevice::new(handle.clone()));
    cached_block_device_from_parts(device_key, endpoint, NativeHandleBlockDevice::new(handle))
}

#[cfg(any(feature = "ext4", feature = "fat"))]
fn cached_block_device_from_parts<T: FsBlockDevice + 'static>(
    device_key: usize,
    endpoint: Box<dyn FsBlockDevice>,
    inner: T,
) -> BlockResult<Option<Box<dyn FsBlockDevice>>> {
    resolve_cache_creation(BufferedBlockDevice::with_device_key(
        device_key, endpoint, inner,
    ))
    .map(|cached| cached.map(|buffered| Box::new(buffered) as Box<dyn FsBlockDevice>))
}

#[cfg(any(feature = "ext4", feature = "fat"))]
fn resolve_cache_creation<T>(result: BlockResult<T>) -> BlockResult<Option<T>> {
    match result {
        Ok(cached) => Ok(Some(cached)),
        Err(BlockError::InvalidRequest) => {
            error!("block cache geometry is invalid, using uncached device");
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(test, any(feature = "ext4", feature = "fat")))]
mod tests {
    use super::*;

    struct TestBlockDevice {
        block_size: usize,
    }

    impl FsBlockDevice for TestBlockDevice {
        fn name(&self) -> &str {
            "test"
        }

        fn num_blocks(&self) -> u64 {
            64
        }

        fn block_size(&self) -> usize {
            self.block_size
        }

        fn read_block(&mut self, _block_id: u64, _buf: &mut [u8]) -> BlockResult {
            unreachable!("cache construction must not issue IO")
        }

        fn write_block(&mut self, _block_id: u64, _buf: &[u8]) -> BlockResult {
            unreachable!("cache construction must not issue IO")
        }

        fn flush(&mut self) -> BlockResult {
            unreachable!("cache construction must not issue IO")
        }
    }

    fn test_device(block_size: usize) -> TestBlockDevice {
        TestBlockDevice { block_size }
    }

    #[test]
    fn cache_allocation_failure_is_not_downgraded_to_uncached_io() {
        let device_key = usize::MAX - 1;
        cache::fail_registry_reserve_for_key_for_test(device_key);

        let result = cached_block_device_from_parts(
            device_key,
            Box::new(test_device(512)),
            test_device(512),
        );

        assert!(matches!(result, Err(BlockError::NoMemory)));
        assert!(!cache::registry_contains_key_for_test(device_key));
    }

    #[test]
    fn malformed_cache_geometry_keeps_the_uncached_fallback() {
        let result = cached_block_device_from_parts(
            usize::MAX - 2,
            Box::new(test_device(1000)),
            test_device(1000),
        );

        assert!(matches!(result, Ok(None)));
    }
}
