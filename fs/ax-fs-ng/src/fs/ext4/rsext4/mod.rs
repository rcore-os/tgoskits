mod fs;
mod inode;
mod util;

use alloc::boxed::Box;

pub use fs::*;
pub use inode::*;
use rsext4::{
    BlockIo, Delay, DeviceCapabilities, DeviceGeometry, EntropySource, Event, Ext4Timestamp,
    MountedServices, Observer, SectorId, WriteFlags,
    error::{Ext4Error, Ext4Result},
};

use crate::block::{BlockRegion, FsBlockDevice, RegionBlockDevice};

pub(crate) struct Ext4Disk {
    device: RegionBlockDevice<Box<dyn FsBlockDevice>>,
    geometry: DeviceGeometry,
}

pub(crate) type MountedExt4 =
    rsext4::Ext4<Ext4Disk, MountedServices<Ext4Entropy, Ext4Observer, Ext4Delay>>;

#[derive(Default)]
pub(crate) struct Ext4Observer;

#[derive(Default)]
pub(crate) struct Ext4Clock;

#[derive(Default)]
pub(crate) struct Ext4Entropy;

#[derive(Default)]
pub(crate) struct Ext4Delay;

impl Observer for Ext4Observer {
    fn event(&mut self, event: Event) {
        log::debug!("rsext4 event: {event:?}");
    }
}

impl EntropySource for Ext4Entropy {
    fn fill_bytes(&mut self, output: &mut [u8]) -> Ext4Result<()> {
        if !crate::os::has_entropy_provider() {
            return Err(Ext4Error::unsupported_capability("runtime:entropy"));
        }
        crate::os::fill_entropy(output).map_err(|_| Ext4Error::io())
    }
}

impl Delay for Ext4Delay {
    fn wait(&mut self, duration: core::time::Duration) -> Ext4Result<()> {
        let runtime = crate::os::runtime_ops()
            .map_err(|_| Ext4Error::unsupported_capability("runtime:delay"))?;
        if !runtime.can_block() {
            return Err(Ext4Error::unsupported_capability("runtime:blocking_delay"));
        }
        let notification = runtime.notification();
        if notification.wait_timeout(duration) {
            Ok(())
        } else {
            Err(Ext4Error::timeout().with_operation("mmp:startup_interrupted"))
        }
    }
}

impl Ext4Disk {
    pub fn new(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> Ext4Result<Self> {
        let device = RegionBlockDevice::new(dev, region);
        let logical_block_size =
            u32::try_from(device.block_size()).map_err(|_| Ext4Error::overflow())?;
        let physical_block_size =
            u32::try_from(device.physical_block_size()).map_err(|_| Ext4Error::overflow())?;
        let geometry = DeviceGeometry::new(logical_block_size, device.num_blocks())
            .with_physical_block_size(physical_block_size);
        Ok(Self { device, geometry })
    }
}

impl BlockIo for Ext4Disk {
    fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        if self.device.is_read_only() {
            return Err(Ext4Error::read_only());
        }
        let dev_block = self.device.block_size();
        let required_size = dev_block
            .checked_mul(count as usize)
            .ok_or_else(Ext4Error::overflow)?;
        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        self.device
            .write_block(sector.raw(), &buffer[..required_size])
            .map_err(|_| Ext4Error::io())
    }

    fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        let dev_block = self.device.block_size();
        let required_size = dev_block
            .checked_mul(count as usize)
            .ok_or_else(Ext4Error::overflow)?;
        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        self.device
            .read_block(sector.raw(), &mut buffer[..required_size])
            .map_err(|_| Ext4Error::io())
    }

    fn write_with_flags(
        &mut self,
        buffer: &[u8],
        sector: SectorId,
        count: u32,
        flags: WriteFlags,
    ) -> Ext4Result<()> {
        if !flags.contains(WriteFlags::FUA) {
            return self.write(buffer, sector, count);
        }
        if self.device.is_read_only() {
            return Err(Ext4Error::read_only());
        }
        if !self.device.supports_fua() {
            return Err(Ext4Error::unsupported_capability("block_io:fua"));
        }
        let dev_block = self.device.block_size();
        let required_size = dev_block
            .checked_mul(count as usize)
            .ok_or_else(Ext4Error::overflow)?;
        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        self.device
            .write_block_fua(sector.raw(), &buffer[..required_size])
            .map_err(|_| Ext4Error::io())
    }

    fn geometry(&self) -> DeviceGeometry {
        self.geometry
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let supports_flush = self.device.supports_flush();
        DeviceCapabilities {
            read_only: self.device.is_read_only(),
            flush: supports_flush,
            barrier: supports_flush,
            fua: self.device.supports_fua(),
            ..DeviceCapabilities::default()
        }
    }

    fn flush(&mut self) -> Ext4Result<()> {
        if !self.device.supports_flush() {
            return Err(Ext4Error::unsupported_capability("block_io:flush"));
        }
        self.device.flush().map_err(|_| Ext4Error::io())
    }

    fn barrier(&mut self) -> Ext4Result<()> {
        if !self.device.supports_flush() {
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

    use rsext4::Ext4ErrorKind;

    use super::*;
    use crate::BlockResult;

    struct CapabilityDevice {
        read_only: bool,
        supports_flush: bool,
        supports_fua: bool,
        logical_block_size: usize,
        physical_block_size: usize,
        writes: Arc<AtomicUsize>,
        fua_writes: Arc<AtomicUsize>,
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
            self.logical_block_size
        }

        fn physical_block_size(&self) -> usize {
            self.physical_block_size
        }

        fn is_read_only(&self) -> bool {
            self.read_only
        }

        fn supports_flush(&self) -> bool {
            self.supports_flush
        }

        fn supports_fua(&self) -> bool {
            self.supports_fua
        }

        fn read_block(&mut self, _block_id: u64, buf: &mut [u8]) -> BlockResult {
            buf.fill(0);
            Ok(())
        }

        fn write_block(&mut self, _block_id: u64, _buf: &[u8]) -> BlockResult {
            self.writes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn write_block_fua(&mut self, _block_id: u64, _buf: &[u8]) -> BlockResult {
            self.fua_writes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn flush(&mut self) -> BlockResult {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn test_disk(
        read_only: bool,
        supports_flush: bool,
        supports_fua: bool,
    ) -> (
        Ext4Disk,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        let writes = Arc::new(AtomicUsize::new(0));
        let fua_writes = Arc::new(AtomicUsize::new(0));
        let flushes = Arc::new(AtomicUsize::new(0));
        let device = CapabilityDevice {
            read_only,
            supports_flush,
            supports_fua,
            logical_block_size: 512,
            physical_block_size: 4096,
            writes: Arc::clone(&writes),
            fua_writes: Arc::clone(&fua_writes),
            flushes: Arc::clone(&flushes),
        };
        (
            Ext4Disk::new(Box::new(device), BlockRegion::from_num_blocks(16))
                .expect("valid test-device geometry"),
            writes,
            fua_writes,
            flushes,
        )
    }

    #[test]
    fn ext4_disk_propagates_read_only_and_rejects_writes() {
        let (mut disk, writes, ..) = test_disk(true, true, true);

        assert!(disk.capabilities().read_only);
        let error = disk
            .write(&[0; 512], SectorId::new(0), 1)
            .expect_err("read-only adapter must reject writes before device I/O");
        assert_eq!(error.kind(), Ext4ErrorKind::ReadOnly);
        assert_eq!(writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ext4_disk_does_not_invent_flush_or_barrier_support() {
        let (mut disk, _, _, flushes) = test_disk(false, false, false);

        let capabilities = disk.capabilities();
        assert!(!capabilities.flush);
        assert!(!capabilities.barrier);
        assert!(!capabilities.fua);
        assert_eq!(
            disk.flush().expect_err("unsupported flush").kind(),
            Ext4ErrorKind::UnsupportedCapability
        );
        assert_eq!(
            disk.barrier().expect_err("unsupported barrier").kind(),
            Ext4ErrorKind::UnsupportedCapability
        );
        assert_eq!(
            disk.write_with_flags(&[0x5a; 512], SectorId::new(0), 1, WriteFlags::FUA)
                .expect_err("unsupported FUA")
                .kind(),
            Ext4ErrorKind::UnsupportedCapability
        );
        assert_eq!(flushes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ext4_disk_uses_advertised_flush_as_its_barrier() {
        let (mut disk, _, _, flushes) = test_disk(false, true, false);

        let capabilities = disk.capabilities();
        assert!(capabilities.flush);
        assert!(capabilities.barrier);
        disk.flush().expect("flush supported device");
        disk.barrier().expect("barrier supported device");
        assert_eq!(flushes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn ext4_disk_forwards_native_fua_without_a_flush() {
        let (mut disk, writes, fua_writes, flushes) = test_disk(false, true, true);

        assert!(disk.capabilities().fua);
        disk.write_with_flags(
            &[0x5a; 512],
            SectorId::new(0),
            1,
            rsext4::WriteFlags::FUA | rsext4::WriteFlags::METADATA,
        )
        .expect("native FUA write");

        assert_eq!(writes.load(Ordering::Relaxed), 0);
        assert_eq!(fua_writes.load(Ordering::Relaxed), 1);
        assert_eq!(flushes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ext4_disk_preserves_physical_block_geometry() {
        let (disk, ..) = test_disk(false, true, true);

        assert_eq!(disk.geometry().logical_block_size, 512);
        assert_eq!(disk.geometry().physical_block_size, 4096);
    }

    #[test]
    fn ext4_disk_rejects_geometry_that_cannot_cross_the_core_boundary() {
        let device = CapabilityDevice {
            read_only: false,
            supports_flush: true,
            supports_fua: false,
            logical_block_size: u32::MAX as usize + 1,
            physical_block_size: u32::MAX as usize + 1,
            writes: Arc::new(AtomicUsize::new(0)),
            fua_writes: Arc::new(AtomicUsize::new(0)),
            flushes: Arc::new(AtomicUsize::new(0)),
        };

        let error = match Ext4Disk::new(Box::new(device), BlockRegion::from_num_blocks(16)) {
            Ok(_) => panic!("oversized adapter geometry must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), Ext4ErrorKind::Overflow);
    }
}
