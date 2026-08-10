mod fs;
mod inode;
mod util;

use alloc::boxed::Box;

pub use fs::*;
pub use inode::*;
use rsext4::{
    BlockIo, DeviceCapabilities, DeviceGeometry, Event, Observer,
    bmalloc::AbsoluteBN,
    config::BLOCK_SIZE,
    disknode::Ext4Timestamp,
    error::{Ext4Error, Ext4Result},
};

use crate::block::{BlockRegion, FsBlockDevice, RegionBlockDevice};

pub(crate) struct Ext4Disk(RegionBlockDevice<Box<dyn FsBlockDevice>>);

#[derive(Default)]
pub(crate) struct Ext4Observer;

impl Observer for Ext4Observer {
    fn event(&mut self, event: Event) {
        log::debug!("rsext4 event: {event:?}");
    }
}

impl Ext4Disk {
    pub fn new(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> Self {
        Self(RegionBlockDevice::new(dev, region))
    }
}

impl BlockIo for Ext4Disk {
    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let dev_block = self.0.block_size();
        if !BLOCK_SIZE.is_multiple_of(dev_block) {
            return Err(Ext4Error::invalid_input());
        }
        let factor = (BLOCK_SIZE / dev_block) as u64;
        let required_size = BLOCK_SIZE * count as usize;
        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        let start_block = block_id.raw() * factor;
        self.0
            .write_block(start_block, &buffer[..required_size])
            .map_err(|_| Ext4Error::io())
    }

    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let dev_block = self.0.block_size();
        if !BLOCK_SIZE.is_multiple_of(dev_block) {
            return Err(Ext4Error::invalid_input());
        }
        let factor = (BLOCK_SIZE / dev_block) as u64;
        let required_size = BLOCK_SIZE * count as usize;
        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        let start_block = block_id.raw() * factor;
        self.0
            .read_block(start_block, &mut buffer[..required_size])
            .map_err(|_| Ext4Error::io())
    }

    fn geometry(&self) -> DeviceGeometry {
        let device_bytes = self
            .0
            .num_blocks()
            .saturating_mul(self.0.block_size() as u64);
        DeviceGeometry::new(BLOCK_SIZE as u32, device_bytes / BLOCK_SIZE as u64)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            flush: true,
            barrier: true,
            ..DeviceCapabilities::default()
        }
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.0.flush().map_err(|_| Ext4Error::io())
    }

    fn barrier(&mut self) -> Ext4Result<()> {
        self.flush()
    }
}

impl rsext4::Clock for Ext4Disk {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let dur = crate::os::wall_time();
        Ok(Ext4Timestamp::new(dur.as_secs() as i64, dur.subsec_nanos()))
    }
}
