//! Pending journal bytes should not depend on an obsolete backing block.

use std::{cell::Cell, rc::Rc};

use rsext4::{
    BlockIo, Clock, DeviceCapabilities, DeviceGeometry, Jbd2Dev, SectorId,
    bmalloc::AbsoluteBN,
    config::BLOCK_SIZE,
    disknode::Ext4Timestamp,
    error::{Ext4Error, Ext4Result},
    jbd2::jbdstruct::JournalSuperBlock,
};

const TARGET: AbsoluteBN = AbsoluteBN::new(4);

#[test]
fn complete_pending_block_does_not_read_backing_storage() {
    let (mut device, io) = journal_device();
    device
        .write_blocks(&[0x5a; BLOCK_SIZE], TARGET, 1, true)
        .unwrap();
    io.fail_reads.set(true);
    let mut bytes = [0; BLOCK_SIZE];

    assert_eq!(device.read_blocks(&mut bytes, TARGET, 1), Ok(()));

    assert_eq!(bytes, [0x5a; BLOCK_SIZE]);
    assert_eq!(io.reads.get(), 0);
    assert_eq!(io.writes.get(), 0, "reading must not commit the journal");
}

#[test]
fn replacement_pending_block_is_visible_without_a_preread() {
    let (mut device, io) = journal_device();
    device
        .write_blocks(&[0x5a; BLOCK_SIZE], TARGET, 1, true)
        .unwrap();
    device
        .write_blocks(&[0xa5; BLOCK_SIZE], TARGET, 1, true)
        .unwrap();
    let mut bytes = [0; BLOCK_SIZE];

    device.read_blocks(&mut bytes, TARGET, 1).unwrap();

    assert_eq!(bytes, [0xa5; BLOCK_SIZE]);
    assert_eq!(io.reads.get(), 0);
    assert_eq!(io.writes.get(), 0);
}

#[test]
fn missing_pending_block_preserves_backing_read_and_error() {
    let (mut device, io) = journal_device();
    let mut bytes = [0; BLOCK_SIZE];
    device.read_blocks(&mut bytes, TARGET, 1).unwrap();
    assert_eq!(bytes, [0x11; BLOCK_SIZE]);
    assert_eq!(io.reads.get(), 1);
    io.fail_reads.set(true);

    assert_eq!(
        device.read_blocks(&mut bytes, TARGET, 1),
        Err(Ext4Error::io())
    );
    assert_eq!(io.reads.get(), 2);
}

#[test]
fn multi_block_read_retains_pending_overlay_and_backing_bytes() {
    let (mut device, io) = journal_device();
    device
        .write_blocks(&[0x5a; BLOCK_SIZE], TARGET, 1, true)
        .unwrap();
    let mut bytes = [0; 2 * BLOCK_SIZE];

    device.read_blocks(&mut bytes, TARGET, 2).unwrap();

    assert_eq!(bytes[..BLOCK_SIZE], [0x5a; BLOCK_SIZE]);
    assert_eq!(bytes[BLOCK_SIZE..], [0x11; BLOCK_SIZE]);
    assert_eq!(io.reads.get(), 1);
    assert_eq!(io.writes.get(), 0);
}

#[test]
fn short_buffer_is_rejected_before_pending_copy_or_io() {
    let (mut device, io) = journal_device();
    device
        .write_blocks(&[0x5a; BLOCK_SIZE], TARGET, 1, true)
        .unwrap();
    let mut bytes = [0xee; BLOCK_SIZE - 1];

    assert_eq!(
        device.read_blocks(&mut bytes, TARGET, 1),
        Err(Ext4Error::buffer_too_small(BLOCK_SIZE - 1, BLOCK_SIZE))
    );

    assert_eq!(bytes, [0xee; BLOCK_SIZE - 1]);
    assert_eq!(io.reads.get(), 0);
}

#[test]
fn disabling_journal_keeps_direct_storage_semantics() {
    let (mut device, io) = journal_device();
    device.set_journal_use(false).unwrap();
    let mut bytes = [0; BLOCK_SIZE];

    device.read_blocks(&mut bytes, TARGET, 1).unwrap();

    assert_eq!(bytes, [0x11; BLOCK_SIZE]);
    assert_eq!(io.reads.get(), 1);
    assert_eq!(io.writes.get(), 0);
}

#[test]
fn oversized_buffer_keeps_existing_backing_read_boundary() {
    let (mut device, io) = journal_device();
    device
        .write_blocks(&[0x5a; BLOCK_SIZE], TARGET, 1, true)
        .unwrap();
    let mut bytes = [0xee; BLOCK_SIZE + 7];

    device.read_blocks(&mut bytes, TARGET, 1).unwrap();

    assert_eq!(bytes[..BLOCK_SIZE], [0x5a; BLOCK_SIZE]);
    assert_eq!(bytes[BLOCK_SIZE..], [0xee; 7]);
    assert_eq!(io.reads.get(), 1);
}

#[test]
fn zero_count_retains_the_existing_device_call() {
    let (mut device, io) = journal_device();

    device.read_blocks(&mut [], TARGET, 0).unwrap();

    assert_eq!(io.reads.get(), 1);
}

fn journal_device() -> (Jbd2Dev<BackingDevice>, Rc<DeviceIo>) {
    let io = Rc::new(DeviceIo::default());
    let mut device = Jbd2Dev::initial_jbd2dev(0, BackingDevice { io: io.clone() }, true);
    device
        .set_journal_superblock(
            JournalSuperBlock {
                s_maxlen: 16,
                ..Default::default()
            },
            AbsoluteBN::new(16),
        )
        .unwrap();
    (device, io)
}

#[derive(Default)]
struct DeviceIo {
    reads: Cell<usize>,
    writes: Cell<usize>,
    fail_reads: Cell<bool>,
}

struct BackingDevice {
    io: Rc<DeviceIo>,
}

impl BlockIo for BackingDevice {
    fn read(&mut self, buffer: &mut [u8], _sector: SectorId, count: u32) -> Ext4Result<()> {
        self.io.reads.set(self.io.reads.get() + 1);
        if self.io.fail_reads.get() {
            return Err(Ext4Error::io());
        }
        buffer[..count as usize * BLOCK_SIZE].fill(0x11);
        Ok(())
    }

    fn write(&mut self, _buffer: &[u8], _sector: SectorId, _count: u32) -> Ext4Result<()> {
        self.io.writes.set(self.io.writes.get() + 1);
        Ok(())
    }

    fn geometry(&self) -> DeviceGeometry {
        DeviceGeometry::new(BLOCK_SIZE as u32, 64)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            flush: true,
            ..Default::default()
        }
    }

    fn flush(&mut self) -> Ext4Result<()> {
        Ok(())
    }
}

impl Clock for BackingDevice {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(Ext4Timestamp::new(0, 0))
    }
}
