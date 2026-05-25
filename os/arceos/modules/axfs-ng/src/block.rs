use alloc::boxed::Box;

#[cfg(any(feature = "ext4", feature = "fat"))]
use ax_errno::AxError;
use ax_errno::AxResult;
use rd_block_volume::{BlockReader, Error as VolumeError};

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

pub trait FsBlockDevice: Send {
    fn name(&self) -> &str;
    fn num_blocks(&self) -> u64;
    fn block_size(&self) -> usize;
    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult;
    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult;
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

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        (**self).write_block(block_id, buf)
    }

    fn flush(&mut self) -> AxResult {
        (**self).flush()
    }
}

#[cfg(any(feature = "ext4", feature = "fat"))]
pub struct RegionBlockDevice<T> {
    inner: T,
    region: BlockRegion,
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

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        self.check_io_bounds(block_id, buf.len())?;
        let physical = self
            .region
            .start_lba
            .checked_add(block_id)
            .ok_or(AxError::BadState)?;
        self.inner.write_block(physical, buf)
    }

    fn flush(&mut self) -> AxResult {
        self.inner.flush()
    }
}

pub struct VolumeReader<'a, T: FsBlockDevice + ?Sized> {
    inner: &'a mut T,
}

impl<'a, T: FsBlockDevice + ?Sized> VolumeReader<'a, T> {
    pub const fn new(inner: &'a mut T) -> Self {
        Self { inner }
    }
}

impl<T: FsBlockDevice + ?Sized> BlockReader for VolumeReader<'_, T> {
    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    fn num_blocks(&self) -> u64 {
        self.inner.num_blocks()
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> rd_block_volume::Result<()> {
        self.inner
            .read_block(block, buf)
            .map_err(|_| VolumeError::Reader)
    }
}
