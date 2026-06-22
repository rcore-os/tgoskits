use alloc::{boxed::Box, sync::Arc};

#[cfg(any(feature = "ext4", feature = "fat"))]
use ax_errno::AxError;
use ax_errno::AxResult;

pub mod runtime;

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
    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult;
    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult;
    #[cfg(feature = "ext4")]
    fn flush(&mut self) -> AxResult;
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

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        (**self).read_block(block_id, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        (**self).write_block(block_id, buf)
    }

    #[cfg(feature = "ext4")]
    fn flush(&mut self) -> AxResult {
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

    fn check_io_bounds(&self, block_id: u64, buf_len: usize) -> AxResult {
        let block_size = self.inner.block_size();
        if block_size == 0 || !buf_len.is_multiple_of(block_size) {
            return Err(AxError::InvalidInput);
        }

        let blocks = u64::try_from(buf_len / block_size).map_err(|_| AxError::BadState)?;
        let end_block = block_id.checked_add(blocks).ok_or(AxError::BadState)?;
        if end_block > self.num_blocks() {
            return Err(AxError::InvalidInput);
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

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        self.check_io_bounds(block_id, buf.len())?;
        let physical = self
            .region
            .start_lba
            .checked_add(block_id)
            .ok_or(AxError::BadState)?;
        self.inner.read_block(physical, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        self.check_io_bounds(block_id, buf.len())?;
        let physical = self
            .region
            .start_lba
            .checked_add(block_id)
            .ok_or(AxError::BadState)?;
        self.inner.write_block(physical, buf)
    }

    #[cfg(feature = "ext4")]
    fn flush(&mut self) -> AxResult {
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

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        self.handle.read_blocks(block_id, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        self.handle.write_blocks(block_id, buf)
    }

    #[cfg(feature = "ext4")]
    fn flush(&mut self) -> AxResult {
        self.handle.flush_blocks()
    }
}

pub(crate) fn boxed_native_handle_block_device(
    handle: Arc<BlockDeviceHandle>,
) -> Box<dyn FsBlockDevice> {
    Box::new(NativeHandleBlockDevice::new(handle))
}
