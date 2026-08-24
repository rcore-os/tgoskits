use alloc::{boxed::Box, sync::Arc};

#[cfg(any(feature = "ext4", feature = "fat"))]
use crate::BlockError;
use crate::BlockResult;

pub mod runtime;

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

pub(crate) trait FsBlockDevice: Send {
    fn name(&self) -> &str;
    fn num_blocks(&self) -> u64;
    fn block_size(&self) -> usize;
    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult;
    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> BlockResult;
    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn flush(&mut self) -> BlockResult;
}

impl<T: FsBlockDevice + ?Sized> FsBlockDevice for Box<T> {
    fn name(&self) -> &str {
        (**self).name()
    }

    fn num_blocks(&self) -> u64 {
        (**self).num_blocks()
    }

    fn block_size(&self) -> usize {
        (**self).block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult {
        (**self).read_block(block_id, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> BlockResult {
        (**self).write_block(block_id, buf)
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
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn num_blocks(&self) -> u64 {
        self.region.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.inner.block_size()
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

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn flush(&mut self) -> BlockResult {
        self.inner.flush()
    }
}

impl FsBlockDevice for NativeHandleBlockDevice {
    fn name(&self) -> &str {
        self.handle.name()
    }

    fn num_blocks(&self) -> u64 {
        self.handle.device_info().num_blocks
    }

    fn block_size(&self) -> usize {
        self.handle.device_info().logical_block_size
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult {
        self.handle.read_blocks(block_id, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> BlockResult {
        self.handle.write_blocks(block_id, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn flush(&mut self) -> BlockResult {
        self.handle.flush_blocks()
    }
}

pub(crate) fn boxed_native_handle_block_device(
    handle: Arc<BlockDeviceHandle>,
) -> Box<dyn FsBlockDevice> {
    #[cfg(any(feature = "ext4", feature = "fat"))]
    if let Some(cached) = cached_block_device(handle.clone()) {
        return cached;
    }
    // Also the fallback when the cache cannot wrap a malformed geometry,
    // and the only path when no filesystem consumer is enabled.
    Box::new(NativeHandleBlockDevice::new(handle))
}

/// Wraps the device in its shared cache tree, keyed by the handle's
/// address. The wrapper keeps its own `Arc` clone alive, so the key stays
/// valid as long as any cached consumer of the device exists.
#[cfg(any(feature = "ext4", feature = "fat"))]
fn cached_block_device(handle: Arc<BlockDeviceHandle>) -> Option<Box<dyn FsBlockDevice>> {
    let device_key = Arc::as_ptr(&handle) as usize;
    let endpoint = Box::new(NativeHandleBlockDevice::new(handle.clone()));
    match BufferedBlockDevice::with_device_key(
        device_key,
        endpoint,
        NativeHandleBlockDevice::new(handle),
    ) {
        Ok(buffered) => Some(Box::new(buffered)),
        // Only a malformed device geometry (non power-of-two block size)
        // reaches this arm; keep the device usable uncached rather than
        // failing boot, but leave a loud trace.
        Err(error) => {
            error!("block cache unavailable, using uncached device: {error:?}");
            None
        }
    }
}
