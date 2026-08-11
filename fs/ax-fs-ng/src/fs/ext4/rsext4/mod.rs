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
        if self.0.is_read_only() {
            return Err(Ext4Error::read_only());
        }
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
        let supports_flush = self.0.supports_flush();
        DeviceCapabilities {
            read_only: self.0.is_read_only(),
            flush: supports_flush,
            barrier: supports_flush,
            ..DeviceCapabilities::default()
        }
    }

    fn flush(&mut self) -> Ext4Result<()> {
        if !self.0.supports_flush() {
            return Err(Ext4Error::unsupported_capability("block_io:flush"));
        }
        self.0.flush().map_err(|_| Ext4Error::io())
    }

    fn barrier(&mut self) -> Ext4Result<()> {
        if !self.0.supports_flush() {
            return Err(Ext4Error::unsupported_capability("block_io:barrier"));
        }
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

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use ax_errno::AxResult;
    use rsext4::Ext4ErrorKind;

    use super::*;

    struct CapabilityDevice {
        read_only: bool,
        supports_flush: bool,
        writes: Arc<AtomicUsize>,
        flushes: Arc<AtomicUsize>,
    }

    impl FsBlockDevice for CapabilityDevice {
        fn name(&self) -> &str {
            "capability-test"
        }

        fn num_blocks(&self) -> u64 {
            16
        }

        fn block_size(&self) -> usize {
            512
        }

        fn is_read_only(&self) -> bool {
            self.read_only
        }

        fn supports_flush(&self) -> bool {
            self.supports_flush
        }

        fn read_block(&mut self, _block_id: u64, buf: &mut [u8]) -> AxResult {
            buf.fill(0);
            Ok(())
        }

        fn write_block(&mut self, _block_id: u64, _buf: &[u8]) -> AxResult {
            self.writes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn flush(&mut self) -> AxResult {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn test_disk(
        read_only: bool,
        supports_flush: bool,
    ) -> (Ext4Disk, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let writes = Arc::new(AtomicUsize::new(0));
        let flushes = Arc::new(AtomicUsize::new(0));
        let device = CapabilityDevice {
            read_only,
            supports_flush,
            writes: Arc::clone(&writes),
            flushes: Arc::clone(&flushes),
        };
        (
            Ext4Disk::new(Box::new(device), BlockRegion::from_num_blocks(16)),
            writes,
            flushes,
        )
    }

    #[test]
    fn ext4_disk_propagates_read_only_and_rejects_writes() {
        let (mut disk, writes, _) = test_disk(true, true);

        assert!(disk.capabilities().read_only);
        let error = disk
            .write(&[0; 512], SectorId::new(0), 1)
            .expect_err("read-only adapter must reject writes before device I/O");
        assert_eq!(error.kind(), Ext4ErrorKind::ReadOnly);
        assert_eq!(writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ext4_disk_does_not_invent_flush_or_barrier_support() {
        let (mut disk, _, flushes) = test_disk(false, false);

        let capabilities = disk.capabilities();
        assert!(!capabilities.flush);
        assert!(!capabilities.barrier);
        assert_eq!(
            disk.flush().expect_err("unsupported flush").kind(),
            Ext4ErrorKind::UnsupportedCapability
        );
        assert_eq!(
            disk.barrier().expect_err("unsupported barrier").kind(),
            Ext4ErrorKind::UnsupportedCapability
        );
        assert_eq!(flushes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ext4_disk_uses_advertised_flush_as_its_barrier() {
        let (mut disk, _, flushes) = test_disk(false, true);

        let capabilities = disk.capabilities();
        assert!(capabilities.flush);
        assert!(capabilities.barrier);
        disk.flush().expect("flush supported device");
        disk.barrier().expect("barrier supported device");
        assert_eq!(flushes.load(Ordering::Relaxed), 2);
    }
}
