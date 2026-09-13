//! Mounted-core fixtures shared by transactional boundary regressions.

use alloc::sync::Arc;
use core::ops::Range;
use std::sync::Mutex;

use super::*;
use crate::{DeviceCapabilities, DeviceGeometry, ForkBlockIo, NoopObserver, SectorId};

mod directory_read;
mod inode_load;
#[cfg(feature = "USE_MULTILEVEL_CACHE")]
mod inode_writeback;
mod metadata;
mod read;
mod write;

type TestMount = Ext4<MemoryDevice, MountedServices<(), NoopObserver>>;

fn mounted_filesystem() -> TestMount {
    let device = MemoryDevice {
        bytes: Arc::new(Mutex::new(alloc::vec![0; 64 * 1024 * 1024])),
    };
    let device = format(device, TestClock, MkfsOptions::default()).unwrap();
    Ext4::mount(
        device,
        MountServices::new(TestClock, (), NoopObserver),
        MountOptions::read_write(),
    )
    .unwrap()
}

fn create_file(mount: &mut TestMount) -> InodeNumber {
    mount
        .create_regular_file(
            MutationContext::new(0, 0, 0, 0),
            mount.root_inode(),
            FileName::new(b"input").unwrap(),
            FilePermissions::new(0o600).unwrap(),
        )
        .unwrap()
        .number
}

#[derive(Clone)]
struct MemoryDevice {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl MemoryDevice {
    fn transfer_range(sector: SectorId, count: u32, bytes: usize) -> Ext4Result<Range<usize>> {
        let start = sector
            .as_usize()?
            .checked_mul(512)
            .ok_or_else(Ext4Error::overflow)?;
        let length = (count as usize)
            .checked_mul(512)
            .ok_or_else(Ext4Error::overflow)?;
        if length != bytes {
            return Err(Ext4Error::invalid_input());
        }
        let end = start.checked_add(length).ok_or_else(Ext4Error::overflow)?;
        Ok(start..end)
    }
}

impl BlockIo for MemoryDevice {
    fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        read::observe_read()?;
        write::observe_read()?;
        directory_read::observe_read()?;
        inode_load::observe_read()?;
        #[cfg(feature = "USE_MULTILEVEL_CACHE")]
        inode_writeback::observe_read()?;
        let range = Self::transfer_range(sector, count, buffer.len())?;
        let bytes = self.bytes.lock().unwrap();
        buffer.copy_from_slice(bytes.get(range).ok_or_else(Ext4Error::invalid_input)?);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        write::observe_write(buffer.len())?;
        let range = Self::transfer_range(sector, count, buffer.len())?;
        self.bytes
            .lock()
            .unwrap()
            .get_mut(range)
            .ok_or_else(Ext4Error::invalid_input)?
            .copy_from_slice(buffer);
        Ok(())
    }

    fn geometry(&self) -> DeviceGeometry {
        DeviceGeometry::new(512, (64 * 1024 * 1024) / 512)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            flush: true,
            barrier: true,
            ..Default::default()
        }
    }

    fn flush(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn barrier(&mut self) -> Ext4Result<()> {
        Ok(())
    }
}

impl ForkBlockIo for MemoryDevice {
    fn fork_io(&self) -> Ext4Result<Self> {
        directory_read::check_fork()?;
        Ok(self.clone())
    }
}

struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(Ext4Timestamp::new(1_700_000_000, 0))
    }
}
