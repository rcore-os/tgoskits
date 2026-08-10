//! Synchronous block I/O capabilities required by the portable ext4 core.

use bitflags::bitflags;

use crate::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
};

/// Typed logical sector identifier exposed by a block device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct SectorId(u64);

impl SectorId {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Immutable geometry of the injected block device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceGeometry {
    pub logical_block_size: u32,
    pub physical_block_size: u32,
    pub block_count: u64,
}

impl DeviceGeometry {
    pub const fn new(logical_block_size: u32, block_count: u64) -> Self {
        Self {
            logical_block_size,
            physical_block_size: logical_block_size,
            block_count,
        }
    }
}

/// Optional durability and maintenance operations provided by a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceCapabilities {
    pub read_only: bool,
    pub flush: bool,
    pub barrier: bool,
    pub fua: bool,
    pub discard: bool,
}

bitflags! {
    /// Durability requirements attached to one block write.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct WriteFlags: u8 {
        const FUA = 1 << 0;
        const METADATA = 1 << 1;
    }
}

/// Low-level synchronous block I/O interface used by the filesystem core.
///
/// Device lifecycle, asynchronous completion, IRQ handling, and OS locking are
/// deliberately outside this boundary. The current block-number signature is
/// retained while the core migrates to dynamic filesystem-block geometry.
pub trait BlockIo {
    /// Writes `count` logical device blocks from `buffer`.
    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()>;

    /// Reads `count` logical device blocks into `buffer`.
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()>;

    /// Writes with an explicit durability requirement.
    fn write_with_flags(
        &mut self,
        buffer: &[u8],
        block_id: AbsoluteBN,
        count: u32,
        flags: WriteFlags,
    ) -> Ext4Result<()> {
        if flags.is_empty() || flags == WriteFlags::METADATA {
            self.write(buffer, block_id, count)
        } else {
            Err(Ext4Error::unsupported())
        }
    }

    /// Returns immutable device geometry.
    fn geometry(&self) -> DeviceGeometry;

    /// Returns optional operations supported by this device.
    fn capabilities(&self) -> DeviceCapabilities;

    /// Flushes device state to stable storage.
    fn flush(&mut self) -> Ext4Result<()>;

    /// Establishes an ordering barrier when supported by the device.
    fn barrier(&mut self) -> Ext4Result<()> {
        Err(Ext4Error::unsupported())
    }

    /// Discards a logical device-block range when supported by the device.
    fn discard(&mut self, _start: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        Err(Ext4Error::unsupported())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::Ext4ErrorKind;

    struct MemoryIo {
        data: Vec<u8>,
    }

    impl BlockIo for MemoryIo {
        fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
            let start = block_id.as_usize()? * 512;
            self.data[start..start + buffer.len()].copy_from_slice(buffer);
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
            let start = block_id.as_usize()? * 512;
            buffer.copy_from_slice(&self.data[start..start + buffer.len()]);
            Ok(())
        }

        fn geometry(&self) -> DeviceGeometry {
            DeviceGeometry::new(512, (self.data.len() / 512) as u64)
        }

        fn capabilities(&self) -> DeviceCapabilities {
            DeviceCapabilities {
                flush: true,
                ..DeviceCapabilities::default()
            }
        }

        fn flush(&mut self) -> Ext4Result<()> {
            Ok(())
        }
    }

    #[test]
    fn optional_durability_operations_never_fake_success() {
        let mut io = MemoryIo {
            data: alloc::vec![0; 4096],
        };
        let data = [0x5a; 512];

        assert_eq!(
            io.write_with_flags(&data, AbsoluteBN::new(0), 1, WriteFlags::FUA)
                .expect_err("FUA requires an explicit backend implementation")
                .kind(),
            Ext4ErrorKind::Unsupported
        );
        assert_eq!(
            io.barrier()
                .expect_err("barrier requires an explicit backend implementation")
                .kind(),
            Ext4ErrorKind::Unsupported
        );
        assert_eq!(
            io.discard(AbsoluteBN::new(0), 1)
                .expect_err("discard requires an explicit backend implementation")
                .kind(),
            Ext4ErrorKind::Unsupported
        );
        io.flush().expect("declared memory flush must succeed");
    }
}
