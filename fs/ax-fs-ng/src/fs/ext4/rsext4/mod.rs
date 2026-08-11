mod fs;
mod inode;
mod util;

use alloc::boxed::Box;

pub use fs::*;
pub use inode::*;
use rsext4::{
    BlockIo, DeviceCapabilities, DeviceGeometry, Event, Ext4Timestamp, MountedServices, Observer,
    SectorId,
    error::{Ext4Error, Ext4Result},
};

use crate::block::{BlockRegion, FsBlockDevice, RegionBlockDevice};

pub(crate) struct Ext4Disk(RegionBlockDevice<Box<dyn FsBlockDevice>>);

pub(crate) type MountedExt4 = rsext4::Ext4<Ext4Disk, MountedServices<(), (), (), Ext4Observer>>;

#[derive(Default)]
pub(crate) struct Ext4Observer;

#[derive(Default)]
pub(crate) struct Ext4Clock;

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
    fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        let dev_block = self.0.block_size();
        let required_size = dev_block
            .checked_mul(count as usize)
            .ok_or_else(Ext4Error::overflow)?;
        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        self.0
            .write_block(sector.raw(), &buffer[..required_size])
            .map_err(|_| Ext4Error::io())
    }

    fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        let dev_block = self.0.block_size();
        let required_size = dev_block
            .checked_mul(count as usize)
            .ok_or_else(Ext4Error::overflow)?;
        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        self.0
            .read_block(sector.raw(), &mut buffer[..required_size])
            .map_err(|_| Ext4Error::io())
    }

    fn geometry(&self) -> DeviceGeometry {
        DeviceGeometry::new(self.0.block_size() as u32, self.0.num_blocks())
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

impl rsext4::Clock for Ext4Clock {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let dur = crate::os::wall_time();
        let seconds = i64::try_from(dur.as_secs()).map_err(|_| Ext4Error::overflow())?;
        Ok(Ext4Timestamp::new(seconds, dur.subsec_nanos()))
    }
}
