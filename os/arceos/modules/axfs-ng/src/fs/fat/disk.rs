use alloc::{boxed::Box, sync::Arc, vec};
use core::mem;

use ax_errno::{AxError as FsBlockError, AxResult as FsBlockResult};

use crate::block::{BlockDevice, BlockRegion, RegionBlockDevice};

fn take<'a>(buf: &mut &'a [u8], cnt: usize) -> &'a [u8] {
    let (first, rem) = buf.split_at(cnt);
    *buf = rem;
    first
}

fn take_mut<'a>(buf: &mut &'a mut [u8], cnt: usize) -> &'a mut [u8] {
    // use mem::take to circumvent lifetime issues
    let (first, rem) = mem::take(buf).split_at_mut(cnt);
    *buf = rem;
    first
}

/// A disk device with a cursor.
pub struct SeekableDisk {
    dev: RegionBlockDevice,

    block_id: u64,
    offset: usize,
    block_size_log2: u8,

    read_buffer: Box<[u8]>,
    write_buffer: Box<[u8]>,
    /// Whether we have unsaved changes in the write buffer.
    ///
    /// It's guaranteed that when `offset == 0`, write_buffer_dirty is false.
    write_buffer_dirty: bool,
}

impl SeekableDisk {
    pub fn new(dev: Arc<dyn BlockDevice>, region: BlockRegion) -> FsBlockResult<Self> {
        let block_size = dev.metadata().block_size();
        let block_size_log2 = block_size.trailing_zeros() as u8;
        let read_buffer = vec![0u8; block_size].into_boxed_slice();
        let write_buffer = vec![0u8; block_size].into_boxed_slice();
        Ok(Self {
            dev: RegionBlockDevice::new(dev, region)?,
            block_id: 0,
            offset: 0,
            block_size_log2,
            read_buffer,
            write_buffer,
            write_buffer_dirty: false,
        })
    }

    /// Get the size of the disk.
    pub fn size(&self) -> u64 {
        self.dev.metadata().num_blocks() << self.block_size_log2
    }

    /// Get the block size.
    pub fn block_size(&self) -> usize {
        1 << self.block_size_log2
    }

    /// Get the position of the cursor.
    pub fn position(&self) -> u64 {
        (self.block_id << self.block_size_log2) + self.offset as u64
    }

    /// Set the position of the cursor.
    pub fn set_position(&mut self, pos: u64) -> FsBlockResult<()> {
        let block_id = pos >> self.block_size_log2;
        let offset = pos as usize & (self.block_size() - 1);
        if self.write_buffer_dirty && block_id != self.block_id {
            self.writeback_buffer()?;
        }
        self.block_id = block_id;
        self.offset = offset;
        Ok(())
    }

    /// Writes pending changes and asks the device to make them durable.
    pub fn flush(&mut self) -> FsBlockResult<()> {
        self.writeback_buffer()?;
        self.dev.flush()
    }

    fn writeback_buffer(&mut self) -> FsBlockResult<()> {
        if self.write_buffer_dirty {
            self.dev.write_blocks(self.block_id, &self.write_buffer)?;
            self.write_buffer_dirty = false;
        }
        Ok(())
    }

    fn read_partial(&mut self, buf: &mut &mut [u8]) -> FsBlockResult<usize> {
        if self.write_buffer_dirty {
            self.read_buffer.copy_from_slice(&self.write_buffer);
        } else {
            self.dev.read_blocks(self.block_id, &mut self.read_buffer)?;
        }

        let data = &self.read_buffer[self.offset..];
        let length = buf.len().min(data.len());
        take_mut(buf, length).copy_from_slice(&data[..length]);

        self.offset += length;
        if self.offset == self.block_size() {
            self.block_id += 1;
            self.offset = 0;
        }

        Ok(length)
    }

    /// Read from the disk, returns the number of bytes read.
    pub fn read(&mut self, mut buf: &mut [u8]) -> FsBlockResult<usize> {
        let mut read = 0;
        if self.offset != 0 {
            read += self.read_partial(&mut buf)?;
        }
        if buf.len() >= self.block_size() {
            self.writeback_buffer()?;
            let blocks = buf.len() >> self.block_size_log2;
            let length = blocks << self.block_size_log2;
            self.dev
                .read_blocks(self.block_id, take_mut(&mut buf, length))?;
            read += length;

            self.block_id = self
                .block_id
                .checked_add(blocks as u64)
                .ok_or(FsBlockError::BadState)?;
        }
        if !buf.is_empty() {
            read += self.read_partial(&mut buf)?;
        }

        Ok(read)
    }

    fn write_partial(&mut self, buf: &mut &[u8]) -> FsBlockResult<usize> {
        if !self.write_buffer_dirty {
            self.dev
                .read_blocks(self.block_id, &mut self.write_buffer)?;
            self.write_buffer_dirty = true;
        }

        let data = &mut self.write_buffer[self.offset..];
        let length = buf.len().min(data.len());
        data[..length].copy_from_slice(take(buf, length));

        self.offset += length;
        if self.offset == self.block_size() {
            self.writeback_buffer()?;
            self.block_id += 1;
            self.offset = 0;
        }

        Ok(length)
    }

    /// Write to the disk, returns the number of bytes written.
    pub fn write(&mut self, mut buf: &[u8]) -> FsBlockResult<usize> {
        let mut written = 0;
        if self.offset != 0 {
            written += self.write_partial(&mut buf)?;
        }
        if buf.len() >= self.block_size() {
            if self.write_buffer_dirty {
                self.writeback_buffer()?;
            }
            let blocks = buf.len() >> self.block_size_log2;
            let length = blocks << self.block_size_log2;
            self.dev
                .write_blocks(self.block_id, take(&mut buf, length))?;
            written += length;

            self.block_id = self
                .block_id
                .checked_add(blocks as u64)
                .ok_or(FsBlockError::BadState)?;
        }
        if !buf.is_empty() {
            written += self.write_partial(&mut buf)?;
        }

        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use ax_kspin::SpinNoPreempt;

    use super::*;
    use crate::BlockDeviceMetadata;

    struct TrackingDevice {
        bytes: SpinNoPreempt<alloc::vec::Vec<u8>>,
        writes: AtomicUsize,
        flushes: AtomicUsize,
    }

    impl TrackingDevice {
        fn new() -> Self {
            Self {
                bytes: SpinNoPreempt::new(vec![0; 4 * 512]),
                writes: AtomicUsize::new(0),
                flushes: AtomicUsize::new(0),
            }
        }
    }

    impl BlockDevice for TrackingDevice {
        fn name(&self) -> &str {
            "tracking"
        }

        fn metadata(&self) -> BlockDeviceMetadata {
            BlockDeviceMetadata::new(4, 512).unwrap()
        }

        fn read_blocks(&self, start_block: u64, buffer: &mut [u8]) -> FsBlockResult<()> {
            self.metadata()
                .validate_transfer(start_block, buffer.len())?;
            let start = start_block as usize * 512;
            buffer.copy_from_slice(&self.bytes.lock()[start..start + buffer.len()]);
            Ok(())
        }

        fn write_blocks(&self, start_block: u64, buffer: &[u8]) -> FsBlockResult<()> {
            self.metadata()
                .validate_transfer(start_block, buffer.len())?;
            self.writes.fetch_add(1, Ordering::Relaxed);
            let start = start_block as usize * 512;
            self.bytes.lock()[start..start + buffer.len()].copy_from_slice(buffer);
            Ok(())
        }

        fn flush(&self) -> FsBlockResult<()> {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn explicit_flush_writes_partial_block_then_flushes_device() {
        let device = Arc::new(TrackingDevice::new());
        let mut disk = SeekableDisk::new(device.clone(), BlockRegion::from_num_blocks(4)).unwrap();

        assert_eq!(disk.write(&[0x5a]).unwrap(), 1);
        assert_eq!(device.writes.load(Ordering::Relaxed), 0);
        assert_eq!(device.flushes.load(Ordering::Relaxed), 0);

        disk.flush().unwrap();

        assert_eq!(device.writes.load(Ordering::Relaxed), 1);
        assert_eq!(device.flushes.load(Ordering::Relaxed), 1);
        assert_eq!(device.bytes.lock()[0], 0x5a);
    }

    #[test]
    fn cursor_change_writes_buffer_without_forcing_device_flush() {
        let device = Arc::new(TrackingDevice::new());
        let mut disk = SeekableDisk::new(device.clone(), BlockRegion::from_num_blocks(4)).unwrap();
        disk.write(&[0xa5]).unwrap();

        disk.set_position(512).unwrap();

        assert_eq!(device.writes.load(Ordering::Relaxed), 1);
        assert_eq!(device.flushes.load(Ordering::Relaxed), 0);
        assert_eq!(device.bytes.lock()[0], 0xa5);
    }
}
