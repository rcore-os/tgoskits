use alloc::{boxed::Box, sync::Arc, vec};
use core::mem;

use ax_errno::{AxError as FsBlockError, AxResult as FsBlockResult};

use crate::{
    block::{BlockRegion, FsBlockDevice, RegionBlockDevice},
    os::sync::SleepMutex,
};

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
    state: Arc<SleepMutex<SeekableDiskState>>,
}

#[derive(Clone)]
pub struct SeekableDiskFlusher {
    state: Arc<SleepMutex<SeekableDiskState>>,
}

struct SeekableDiskState {
    dev: RegionBlockDevice<Box<dyn FsBlockDevice>>,

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
    pub fn new(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> Self {
        assert!(dev.block_size().is_power_of_two());
        let block_size = dev.block_size();
        let block_size_log2 = block_size.trailing_zeros() as u8;
        let read_buffer = vec![0u8; block_size].into_boxed_slice();
        let write_buffer = vec![0u8; block_size].into_boxed_slice();
        Self {
            state: Arc::new(SleepMutex::new(SeekableDiskState {
                dev: RegionBlockDevice::new(dev, region),
                block_id: 0,
                offset: 0,
                block_size_log2,
                read_buffer,
                write_buffer,
                write_buffer_dirty: false,
            })),
        }
    }

    pub fn flusher(&self) -> SeekableDiskFlusher {
        SeekableDiskFlusher {
            state: self.state.clone(),
        }
    }

    /// Get the size of the disk.
    pub fn size(&self) -> u64 {
        self.state.lock().size()
    }

    /// Get the position of the cursor.
    pub fn position(&self) -> u64 {
        self.state.lock().position()
    }

    /// Set the position of the cursor.
    pub fn set_position(&mut self, pos: u64) -> FsBlockResult<()> {
        self.state.lock().set_position(pos)
    }

    /// Write all pending changes to the disk and issue a device flush.
    pub fn flush(&mut self) -> FsBlockResult<()> {
        self.state.lock().sync()
    }

    /// Read from the disk, returns the number of bytes read.
    pub fn read(&mut self, buf: &mut [u8]) -> FsBlockResult<usize> {
        self.state.lock().read(buf)
    }

    /// Write to the disk, returns the number of bytes written.
    pub fn write(&mut self, buf: &[u8]) -> FsBlockResult<usize> {
        self.state.lock().write(buf)
    }
}

impl Drop for SeekableDisk {
    fn drop(&mut self) {
        if let Err(error) = self.state.lock().sync() {
            error!("failed to flush FAT disk while dropping cursor: {error:?}");
        }
    }
}

impl SeekableDiskFlusher {
    pub fn flush(&self) -> FsBlockResult<()> {
        self.state.lock().sync()
    }
}

impl SeekableDiskState {
    fn size(&self) -> u64 {
        self.dev.num_blocks() << self.block_size_log2
    }

    fn block_size(&self) -> usize {
        1 << self.block_size_log2
    }

    fn position(&self) -> u64 {
        (self.block_id << self.block_size_log2) + self.offset as u64
    }

    fn set_position(&mut self, pos: u64) -> FsBlockResult<()> {
        let block_id = pos >> self.block_size_log2;
        let offset = pos as usize & (self.block_size() - 1);
        if self.write_buffer_dirty && block_id != self.block_id {
            self.flush_buffer()?;
        }
        self.block_id = block_id;
        self.offset = offset;
        Ok(())
    }

    fn flush_buffer(&mut self) -> FsBlockResult<()> {
        if self.write_buffer_dirty {
            self.dev.write_block(self.block_id, &self.write_buffer)?;
            self.write_buffer_dirty = false;
        }
        Ok(())
    }

    fn sync(&mut self) -> FsBlockResult<()> {
        self.flush_buffer()?;
        self.dev.flush()
    }

    fn read_partial(&mut self, buf: &mut &mut [u8]) -> FsBlockResult<usize> {
        if self.write_buffer_dirty {
            self.read_buffer.copy_from_slice(&self.write_buffer);
        } else {
            self.dev.read_block(self.block_id, &mut self.read_buffer)?;
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
    fn read(&mut self, mut buf: &mut [u8]) -> FsBlockResult<usize> {
        let mut read = 0;
        if self.offset != 0 {
            read += self.read_partial(&mut buf)?;
        }
        if buf.len() >= self.block_size() {
            self.flush_buffer()?;
            let blocks = buf.len() >> self.block_size_log2;
            let length = blocks << self.block_size_log2;
            self.dev
                .read_block(self.block_id, take_mut(&mut buf, length))?;
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
            self.dev.read_block(self.block_id, &mut self.write_buffer)?;
            self.write_buffer_dirty = true;
        }

        let data = &mut self.write_buffer[self.offset..];
        let length = buf.len().min(data.len());
        data[..length].copy_from_slice(take(buf, length));

        self.offset += length;
        if self.offset == self.block_size() {
            self.flush_buffer()?;
            self.block_id += 1;
            self.offset = 0;
        }

        Ok(length)
    }

    /// Write to the disk, returns the number of bytes written.
    fn write(&mut self, mut buf: &[u8]) -> FsBlockResult<usize> {
        let mut written = 0;
        if self.offset != 0 {
            written += self.write_partial(&mut buf)?;
        }
        if buf.len() >= self.block_size() {
            if self.write_buffer_dirty {
                self.flush_buffer()?;
            }
            let blocks = buf.len() >> self.block_size_log2;
            let length = blocks << self.block_size_log2;
            self.dev
                .write_block(self.block_id, take(&mut buf, length))?;
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
    use alloc::sync::Arc;
    use std::sync::Mutex;

    use super::*;

    struct MemoryBlockDevice {
        storage: Arc<Mutex<Vec<u8>>>,
        fail_flush: bool,
    }

    impl FsBlockDevice for MemoryBlockDevice {
        fn name(&self) -> &str {
            "memory"
        }

        fn num_blocks(&self) -> u64 {
            1
        }

        fn block_size(&self) -> usize {
            512
        }

        fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> FsBlockResult<()> {
            assert_eq!(block_id, 0);
            buf.copy_from_slice(&self.storage.lock().unwrap());
            Ok(())
        }

        fn write_block(&mut self, block_id: u64, buf: &[u8]) -> FsBlockResult<()> {
            assert_eq!(block_id, 0);
            self.storage.lock().unwrap().copy_from_slice(buf);
            Ok(())
        }

        fn flush(&mut self) -> FsBlockResult<()> {
            if self.fail_flush {
                Err(FsBlockError::Io)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn dropping_disk_persists_pending_partial_write() {
        let storage = Arc::new(Mutex::new(vec![0; 512]));
        let device = MemoryBlockDevice {
            storage: storage.clone(),
            fail_flush: false,
        };
        let mut disk = SeekableDisk::new(Box::new(device), BlockRegion::from_num_blocks(1));

        disk.write(b"fat").expect("buffer partial FAT write");
        drop(disk);

        assert_eq!(&storage.lock().unwrap()[..3], b"fat");
    }

    #[test]
    fn flusher_persists_pending_write_and_propagates_device_error() {
        let storage = Arc::new(Mutex::new(vec![0; 512]));
        let device = MemoryBlockDevice {
            storage: storage.clone(),
            fail_flush: true,
        };
        let mut disk = SeekableDisk::new(Box::new(device), BlockRegion::from_num_blocks(1));
        let flusher = disk.flusher();
        disk.write(b"fat").expect("buffer partial FAT write");

        assert_eq!(flusher.flush(), Err(FsBlockError::Io));
        assert_eq!(&storage.lock().unwrap()[..3], b"fat");
    }
}
