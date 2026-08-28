//! Checked codec and lifecycle state for ext4 multi-mount protection.

use alloc::{boxed::Box, vec};
use core::{cmp, time::Duration};

use crate::{
    blockdev::{BlockIo, Jbd2Dev},
    bmalloc::AbsoluteBN,
    crc32c::{crc32c_append, ext4_crc32c_seed_from_superblock, ext4_superblock_has_metadata_csum},
    disknode::Ext4Timestamp,
    endian::{read_u16_le, read_u32_le, write_u16_le, write_u32_le, write_u64_le},
    error::{Ext4Error, Ext4Result},
    runtime::{Clock, Delay, EntropySource, MmpIdentity},
    superblock::Ext4Superblock,
};

const MMP_BLOCK_BYTES: usize = 1024;
const MMP_CHECKSUM_OFFSET: usize = 1020;
const MMP_NODE_NAME_OFFSET: usize = 16;
const MMP_NODE_NAME_BYTES: usize = 64;
const MMP_DEVICE_NAME_OFFSET: usize = 80;
const MMP_DEVICE_NAME_BYTES: usize = 32;
const MMP_MAGIC: u32 = 0x004d_4d50;
const MMP_SEQUENCE_CLEAN: u32 = 0xff4d_4d50;
const MMP_SEQUENCE_FSCK: u32 = 0xe24d_4d50;
const MMP_SEQUENCE_MAX: u32 = 0xe24d_4d4f;
const MMP_MIN_CHECK_INTERVAL_SECS: u16 = 5;
const MMP_MAX_CHECK_INTERVAL_SECS: u16 = 300;

#[derive(Clone, Debug, PartialEq, Eq)]
struct MmpBlock {
    bytes: [u8; MMP_BLOCK_BYTES],
}

#[derive(Debug)]
pub(crate) struct MmpLease {
    block: AbsoluteBN,
    filesystem_block: alloc::vec::Vec<u8>,
    record: MmpBlock,
    next_sequence: u32,
    update_interval: Duration,
    check_interval: Duration,
}

#[derive(Debug, Default)]
pub(crate) enum MmpState {
    #[default]
    Disabled,
    Active(Box<MmpLease>),
    Failed(Ext4Error),
}

impl MmpState {
    pub(crate) fn claim<B, E, W>(
        device: &mut Jbd2Dev<B>,
        superblock: &Ext4Superblock,
        entropy: &mut E,
        delay: &mut W,
    ) -> Ext4Result<Self>
    where
        B: BlockIo,
        E: EntropySource,
        W: Delay,
    {
        if !superblock.has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_MMP) {
            return Ok(Self::Disabled);
        }

        let block = validate_mmp_location(superblock)?;
        let (mut filesystem_block, mut record) = read_mmp(device, superblock, block)?;
        let check_interval = effective_startup_check_interval(superblock, &record);
        let wait_time = startup_wait_time(&record, check_interval)?;
        if !wait_time.is_zero() {
            let original_sequence = record.sequence();
            delay
                .wait(wait_time)
                .map_err(|error| error.with_operation("mmp:startup_wait"))?;
            (filesystem_block, record) = read_mmp(device, superblock, block)?;
            if record.sequence() != original_sequence {
                return Err(Ext4Error::busy().with_operation("mmp:active"));
            }
        }

        let claimed_sequence = random_sequence(entropy)?;
        record.set_sequence(claimed_sequence);
        write_mmp(
            device,
            superblock,
            block,
            &mut filesystem_block,
            &mut record,
        )?;
        if !wait_time.is_zero() {
            delay
                .wait(wait_time)
                .map_err(|error| error.with_operation("mmp:claim_wait"))?;
        }
        let (observed_block, observed) = read_mmp(device, superblock, block)?;
        if observed.sequence() != claimed_sequence {
            return Err(Ext4Error::busy().with_operation("mmp:claim_lost"));
        }
        filesystem_block = observed_block;
        record = observed;

        Ok(Self::Active(Box::new(MmpLease {
            block,
            filesystem_block,
            record,
            next_sequence: 0,
            update_interval: Duration::from_secs(u64::from(superblock.s_mmp_interval)),
            check_interval,
        })))
    }

    pub(crate) fn refresh<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        superblock: &Ext4Superblock,
        identity: MmpIdentity,
        elapsed: Duration,
    ) -> Ext4Result<Duration> {
        let result = match self {
            Self::Disabled => return Ok(Duration::ZERO),
            Self::Failed(error) => return Err(*error),
            Self::Active(lease) => refresh_lease(device, superblock, identity, elapsed, lease),
        };
        match result {
            Ok(interval) => Ok(interval),
            Err(error) => {
                *self = Self::Failed(error);
                Err(error)
            }
        }
    }

    pub(crate) fn release_clean<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        superblock: &Ext4Superblock,
    ) -> Ext4Result<()> {
        let result = match self {
            Self::Disabled => return Ok(()),
            Self::Failed(error) => return Err(*error),
            Self::Active(lease) => {
                lease.record.set_sequence(MMP_SEQUENCE_CLEAN);
                lease.record.set_time(wall_seconds(device.now()?));
                write_mmp(
                    device,
                    superblock,
                    lease.block,
                    &mut lease.filesystem_block,
                    &mut lease.record,
                )
            }
        };
        match result {
            Ok(()) => {
                *self = Self::Disabled;
                Ok(())
            }
            Err(error) => {
                *self = Self::Failed(error);
                Err(error)
            }
        }
    }

    pub(crate) fn ensure_writable(&self, operation: &'static str) -> Ext4Result<()> {
        match self {
            Self::Failed(_) => Err(Ext4Error::io().with_operation(operation)),
            Self::Disabled | Self::Active(_) => Ok(()),
        }
    }

    pub(crate) fn mark_failed(&mut self, error: Ext4Error) {
        *self = Self::Failed(error);
    }

    pub(crate) const fn is_active(&self) -> bool {
        matches!(self, Self::Active(_))
    }

    pub(crate) const fn refresh_interval(&self) -> Option<Duration> {
        match self {
            Self::Active(lease) => Some(lease.update_interval),
            Self::Disabled | Self::Failed(_) => None,
        }
    }
}

fn refresh_lease<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    superblock: &Ext4Superblock,
    identity: MmpIdentity,
    elapsed: Duration,
    lease: &mut MmpLease,
) -> Ext4Result<Duration> {
    if elapsed > lease.check_interval {
        let (observed_block, observed) = read_mmp(device, superblock, lease.block)?;
        if observed.sequence() != lease.record.sequence()
            || observed.node_name() != lease.record.node_name()
        {
            return Err(Ext4Error::busy().with_operation("mmp:ownership_lost"));
        }
        lease.filesystem_block = observed_block;
        lease.record = observed;
    }

    lease.next_sequence = if lease.next_sequence >= MMP_SEQUENCE_MAX {
        1
    } else {
        lease.next_sequence + 1
    };
    let check_secs = elapsed.as_secs().saturating_mul(2).clamp(
        u64::from(MMP_MIN_CHECK_INTERVAL_SECS),
        u64::from(MMP_MAX_CHECK_INTERVAL_SECS),
    );
    lease.check_interval = Duration::from_secs(check_secs);
    lease.record.set_sequence(lease.next_sequence);
    lease.record.set_time(wall_seconds(device.now()?));
    lease.record.set_check_interval(check_secs as u16);
    lease.record.set_identity(identity);
    write_mmp(
        device,
        superblock,
        lease.block,
        &mut lease.filesystem_block,
        &mut lease.record,
    )?;
    Ok(lease.update_interval)
}

fn read_mmp<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    superblock: &Ext4Superblock,
    block: AbsoluteBN,
) -> Ext4Result<(alloc::vec::Vec<u8>, MmpBlock)> {
    let mut filesystem_block = vec![0; superblock.checked_block_size()? as usize];
    device
        .read_blocks_uncached(&mut filesystem_block, block, 1)
        .map_err(|error| error.with_operation("mmp:read"))?;
    let record = MmpBlock::decode(&filesystem_block, superblock)?;
    Ok((filesystem_block, record))
}

fn write_mmp<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    superblock: &Ext4Superblock,
    block: AbsoluteBN,
    filesystem_block: &mut [u8],
    record: &mut MmpBlock,
) -> Ext4Result<()> {
    let encoded = record.encode(superblock);
    filesystem_block[..MMP_BLOCK_BYTES].copy_from_slice(&encoded);
    device
        .write_blocks_durable(filesystem_block, block, 1)
        .map_err(|error| error.with_operation("mmp:write"))
}

fn effective_startup_check_interval(superblock: &Ext4Superblock, record: &MmpBlock) -> Duration {
    let seconds = cmp::max(
        cmp::max(superblock.s_mmp_interval, MMP_MIN_CHECK_INTERVAL_SECS),
        record.check_interval(),
    );
    Duration::from_secs(u64::from(seconds))
}

fn startup_wait_time(record: &MmpBlock, check_interval: Duration) -> Ext4Result<Duration> {
    if record.is_clean() {
        return Ok(Duration::ZERO);
    }
    if record.is_fsck() {
        return Err(Ext4Error::busy().with_operation("mmp:fsck"));
    }
    let seconds = cmp::min(
        check_interval.as_secs().saturating_mul(2).saturating_add(1),
        check_interval.as_secs().saturating_add(60),
    );
    Ok(Duration::from_secs(seconds))
}

fn random_sequence<E: EntropySource>(entropy: &mut E) -> Ext4Result<u32> {
    let range = MMP_SEQUENCE_MAX + 1;
    let uniform_zone = u32::MAX - (u32::MAX % range);
    loop {
        let mut bytes = [0; 4];
        entropy
            .fill_bytes(&mut bytes)
            .map_err(|error| error.with_operation("mmp:sequence_entropy"))?;
        let candidate = u32::from_ne_bytes(bytes);
        if candidate < uniform_zone {
            return Ok(candidate % range);
        }
    }
}

fn wall_seconds(timestamp: Ext4Timestamp) -> u64 {
    timestamp.sec as u64
}

impl MmpBlock {
    fn decode(bytes: &[u8], superblock: &Ext4Superblock) -> Ext4Result<Self> {
        let source = bytes
            .get(..MMP_BLOCK_BYTES)
            .ok_or_else(|| Ext4Error::buffer_too_small(bytes.len(), MMP_BLOCK_BYTES))?;
        if read_u32_le(&source[0..4]) != MMP_MAGIC {
            return Err(Ext4Error::corrupted().with_operation("mmp:magic"));
        }
        if ext4_superblock_has_metadata_csum(superblock) {
            let stored = read_u32_le(&source[MMP_CHECKSUM_OFFSET..MMP_BLOCK_BYTES]);
            let expected = crc32c_append(
                ext4_crc32c_seed_from_superblock(superblock),
                &source[..MMP_CHECKSUM_OFFSET],
            );
            if stored != expected {
                return Err(Ext4Error::checksum().with_operation("mmp:checksum"));
            }
        }

        let mut owned = [0; MMP_BLOCK_BYTES];
        owned.copy_from_slice(source);
        Ok(Self { bytes: owned })
    }

    fn sequence(&self) -> u32 {
        read_u32_le(&self.bytes[4..8])
    }

    fn check_interval(&self) -> u16 {
        read_u16_le(&self.bytes[112..114])
    }

    fn node_name(&self) -> &[u8] {
        &self.bytes[MMP_NODE_NAME_OFFSET..MMP_NODE_NAME_OFFSET + MMP_NODE_NAME_BYTES]
    }

    fn is_clean(&self) -> bool {
        self.sequence() == MMP_SEQUENCE_CLEAN
    }

    fn is_fsck(&self) -> bool {
        self.sequence() == MMP_SEQUENCE_FSCK
    }

    #[cfg(test)]
    fn has_reserved_sequence(&self) -> bool {
        self.sequence() > MMP_SEQUENCE_MAX
    }

    fn set_sequence(&mut self, sequence: u32) {
        write_u32_le(sequence, &mut self.bytes[4..8]);
    }

    fn set_time(&mut self, seconds: u64) {
        write_u64_le(seconds, &mut self.bytes[8..16]);
    }

    fn set_check_interval(&mut self, seconds: u16) {
        write_u16_le(seconds, &mut self.bytes[112..114]);
    }

    fn set_identity(&mut self, identity: MmpIdentity) {
        self.bytes[MMP_NODE_NAME_OFFSET..MMP_NODE_NAME_OFFSET + MMP_NODE_NAME_BYTES]
            .copy_from_slice(identity.node_name());
        self.bytes[MMP_DEVICE_NAME_OFFSET..MMP_DEVICE_NAME_OFFSET + MMP_DEVICE_NAME_BYTES]
            .copy_from_slice(identity.device_name());
    }

    fn encode(&mut self, superblock: &Ext4Superblock) -> [u8; MMP_BLOCK_BYTES] {
        if ext4_superblock_has_metadata_csum(superblock) {
            let checksum = crc32c_append(
                ext4_crc32c_seed_from_superblock(superblock),
                &self.bytes[..MMP_CHECKSUM_OFFSET],
            );
            write_u32_le(
                checksum,
                &mut self.bytes[MMP_CHECKSUM_OFFSET..MMP_BLOCK_BYTES],
            );
        }
        self.bytes
    }
}

fn validate_mmp_location(superblock: &Ext4Superblock) -> Ext4Result<AbsoluteBN> {
    let block = superblock.s_mmp_block;
    if block < u64::from(superblock.s_first_data_block) || block >= superblock.blocks_count() {
        return Err(Ext4Error::bad_superblock().with_operation("mmp:block_range"));
    }
    Ok(AbsoluteBN::new(block))
}

#[cfg(test)]
mod tests {
    use alloc::{rc::Rc, vec::Vec};
    use core::cell::{Cell, RefCell};

    use super::*;
    use crate::{
        DeviceCapabilities, DeviceGeometry, Ext4ErrorKind, SectorId, WriteFlags,
        crc32c::crc32c_append,
    };

    struct TestIo {
        bytes: Vec<u8>,
        reads: Rc<Cell<usize>>,
        writes: Rc<RefCell<Vec<WriteFlags>>>,
        fail_writes: Rc<Cell<bool>>,
    }

    impl BlockIo for TestIo {
        fn write(&mut self, buffer: &[u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
            self.write_with_flags(buffer, sector, _count, WriteFlags::empty())
        }

        fn read(&mut self, buffer: &mut [u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
            self.reads.set(self.reads.get() + 1);
            let start = sector.as_usize()? * 512;
            buffer.copy_from_slice(&self.bytes[start..start + buffer.len()]);
            Ok(())
        }

        fn write_with_flags(
            &mut self,
            buffer: &[u8],
            sector: SectorId,
            _count: u32,
            flags: WriteFlags,
        ) -> Ext4Result<()> {
            self.writes.borrow_mut().push(flags);
            if self.fail_writes.get() {
                return Err(Ext4Error::io());
            }
            let start = sector.as_usize()? * 512;
            self.bytes[start..start + buffer.len()].copy_from_slice(buffer);
            Ok(())
        }

        fn geometry(&self) -> DeviceGeometry {
            DeviceGeometry::new(512, (self.bytes.len() / 512) as u64)
        }

        fn capabilities(&self) -> DeviceCapabilities {
            DeviceCapabilities {
                flush: true,
                fua: true,
                ..DeviceCapabilities::default()
            }
        }

        fn flush(&mut self) -> Ext4Result<()> {
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    struct TestClock;

    impl Clock for TestClock {
        fn now(&self) -> Ext4Result<Ext4Timestamp> {
            Ok(Ext4Timestamp::new(1_800_000_000, 0))
        }
    }

    struct TestEntropy(u32);

    impl EntropySource for TestEntropy {
        fn fill_bytes(&mut self, output: &mut [u8]) -> Ext4Result<()> {
            output.copy_from_slice(&self.0.to_ne_bytes());
            self.0 = self.0.wrapping_add(1);
            Ok(())
        }
    }

    struct TestDelay(Rc<RefCell<Vec<Duration>>>);

    impl Delay for TestDelay {
        fn wait(&mut self, duration: Duration) -> Ext4Result<()> {
            self.0.borrow_mut().push(duration);
            Ok(())
        }
    }

    fn metadata_checksum_superblock() -> Ext4Superblock {
        let mut superblock = Ext4Superblock::default();
        superblock.s_feature_ro_compat |= Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM;
        superblock.s_uuid = [
            0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0x55, 0xaa, 0x11, 0x22, 0x33, 0x44,
            0x66, 0x88,
        ];
        superblock
    }

    fn valid_mmp_bytes(superblock: &Ext4Superblock) -> [u8; MMP_BLOCK_BYTES] {
        let mut bytes = [0; MMP_BLOCK_BYTES];
        bytes[0..4].copy_from_slice(&MMP_MAGIC.to_le_bytes());
        bytes[4..8].copy_from_slice(&MMP_SEQUENCE_CLEAN.to_le_bytes());
        let seed = crate::crc32c::ext4_crc32c_seed_from_superblock(superblock);
        let checksum = crc32c_append(seed, &bytes[..MMP_CHECKSUM_OFFSET]);
        bytes[MMP_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    fn mmp_superblock() -> Ext4Superblock {
        let mut superblock = metadata_checksum_superblock();
        superblock.s_log_block_size = 2;
        superblock.s_blocks_count_lo = 32;
        superblock.s_mmp_block = 2;
        superblock.s_mmp_interval = 5;
        superblock.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_MMP;
        superblock
    }

    type MmpTestDevice = (
        Jbd2Dev<TestIo>,
        Rc<Cell<usize>>,
        Rc<RefCell<Vec<WriteFlags>>>,
        Rc<Cell<bool>>,
    );

    fn mmp_device(superblock: &Ext4Superblock, record: &[u8; MMP_BLOCK_BYTES]) -> MmpTestDevice {
        let mut bytes = vec![0; 32 * 4096];
        let start = superblock.s_mmp_block as usize * 4096;
        bytes[start..start + MMP_BLOCK_BYTES].copy_from_slice(record);
        let reads = Rc::new(Cell::new(0));
        let writes = Rc::new(RefCell::new(Vec::new()));
        let fail_writes = Rc::new(Cell::new(false));
        let io = TestIo {
            bytes,
            reads: Rc::clone(&reads),
            writes: Rc::clone(&writes),
            fail_writes: Rc::clone(&fail_writes),
        };
        let mut device = Jbd2Dev::with_clock(0, io, TestClock, false);
        device.set_filesystem_block_size(4096).unwrap();
        (device, reads, writes, fail_writes)
    }

    #[test]
    fn checked_mmp_codec_accepts_linux_layout_and_rejects_corruption() {
        let superblock = metadata_checksum_superblock();
        let bytes = valid_mmp_bytes(&superblock);
        let block = MmpBlock::decode(&bytes, &superblock).expect("decode valid MMP block");

        assert_eq!(block.sequence(), MMP_SEQUENCE_CLEAN);
        assert!(block.is_clean());
        assert!(!block.is_fsck());

        let mut bad_magic = bytes;
        bad_magic[0] ^= 0x01;
        assert_eq!(
            MmpBlock::decode(&bad_magic, &superblock)
                .expect_err("bad MMP magic")
                .kind(),
            Ext4ErrorKind::Corrupted
        );

        let mut bad_checksum = bytes;
        bad_checksum[8] ^= 0x01;
        assert_eq!(
            MmpBlock::decode(&bad_checksum, &superblock)
                .expect_err("bad MMP checksum")
                .kind(),
            Ext4ErrorKind::ChecksumMismatch
        );
    }

    #[test]
    fn mmp_codec_preserves_unmodeled_bytes_and_updates_linux_checksum() {
        let superblock = metadata_checksum_superblock();
        let mut bytes = valid_mmp_bytes(&superblock);
        bytes[16..80].fill(0x5a);
        let seed = crate::crc32c::ext4_crc32c_seed_from_superblock(&superblock);
        let checksum = crc32c_append(seed, &bytes[..MMP_CHECKSUM_OFFSET]);
        bytes[MMP_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());

        let mut block = MmpBlock::decode(&bytes, &superblock).expect("decode named MMP block");
        block.set_sequence(7);
        block.set_time(1234);
        block.set_check_interval(11);
        let encoded = block.encode(&superblock);

        assert_eq!(&encoded[16..80], &[0x5a; 64]);
        assert_eq!(u32::from_le_bytes(encoded[4..8].try_into().unwrap()), 7);
        assert_eq!(u64::from_le_bytes(encoded[8..16].try_into().unwrap()), 1234);
        assert_eq!(
            u16::from_le_bytes(encoded[112..114].try_into().unwrap()),
            11
        );
        MmpBlock::decode(&encoded, &superblock).expect("checksum updated after mutation");
    }

    #[test]
    fn mmp_location_uses_full_64_bit_filesystem_geometry() {
        let mut superblock = Ext4Superblock {
            s_first_data_block: 1,
            s_blocks_count_lo: 4,
            s_blocks_count_hi: 1,
            s_mmp_block: u64::from(u32::MAX) + 2,
            ..Default::default()
        };

        assert_eq!(
            validate_mmp_location(&superblock).unwrap().raw(),
            u64::from(u32::MAX) + 2
        );
        superblock.s_mmp_block = superblock.blocks_count();
        assert_eq!(
            validate_mmp_location(&superblock).unwrap_err().kind(),
            Ext4ErrorKind::BadSuperblock
        );
    }

    #[test]
    fn reserved_sequences_follow_linux_startup_policy() {
        let superblock = metadata_checksum_superblock();
        let mut bytes = valid_mmp_bytes(&superblock);
        bytes[4..8].copy_from_slice(&MMP_SEQUENCE_FSCK.to_le_bytes());
        let checksum = crc32c_append(
            ext4_crc32c_seed_from_superblock(&superblock),
            &bytes[..MMP_CHECKSUM_OFFSET],
        );
        bytes[MMP_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
        let fsck = MmpBlock::decode(&bytes, &superblock).unwrap();
        assert_eq!(
            startup_wait_time(&fsck, Duration::from_secs(5))
                .unwrap_err()
                .kind(),
            Ext4ErrorKind::Busy
        );

        bytes[4..8].copy_from_slice(&(MMP_SEQUENCE_MAX + 2).to_le_bytes());
        let checksum = crc32c_append(
            ext4_crc32c_seed_from_superblock(&superblock),
            &bytes[..MMP_CHECKSUM_OFFSET],
        );
        bytes[MMP_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
        let reserved = MmpBlock::decode(&bytes, &superblock).unwrap();
        assert!(reserved.has_reserved_sequence());
        assert_eq!(
            startup_wait_time(&reserved, Duration::from_secs(5)).unwrap(),
            Duration::from_secs(11)
        );
    }

    #[test]
    fn stale_mmp_owner_is_rechecked_claimed_refreshed_and_released() {
        let superblock = mmp_superblock();
        let mut record = MmpBlock::decode(&valid_mmp_bytes(&superblock), &superblock).unwrap();
        record.set_sequence(7);
        record.set_check_interval(5);
        let encoded = record.encode(&superblock);
        let (mut device, _, writes, _) = mmp_device(&superblock, &encoded);
        let waits = Rc::new(RefCell::new(Vec::new()));

        let mut state = MmpState::claim(
            &mut device,
            &superblock,
            &mut TestEntropy(0x1234_5678),
            &mut TestDelay(Rc::clone(&waits)),
        )
        .expect("unchanged stale sequence can be claimed");
        assert_eq!(
            waits.borrow().as_slice(),
            &[Duration::from_secs(11), Duration::from_secs(11)]
        );

        state
            .refresh(
                &mut device,
                &superblock,
                MmpIdentity::from_names(b"node-a", b"disk-a"),
                Duration::from_secs(5),
            )
            .unwrap();
        state.release_clean(&mut device, &superblock).unwrap();
        assert!(
            writes
                .borrow()
                .iter()
                .all(|flags| flags.contains(WriteFlags::METADATA | WriteFlags::FUA))
        );

        let io = device.into_inner();
        let start = superblock.s_mmp_block as usize * 4096;
        let released =
            MmpBlock::decode(&io.bytes[start..start + MMP_BLOCK_BYTES], &superblock).unwrap();
        assert!(released.is_clean());
    }

    #[test]
    fn fsck_sequence_is_busy_without_waiting_or_writing() {
        let superblock = mmp_superblock();
        let mut record = MmpBlock::decode(&valid_mmp_bytes(&superblock), &superblock).unwrap();
        record.set_sequence(MMP_SEQUENCE_FSCK);
        let encoded = record.encode(&superblock);
        let (mut device, _, writes, _) = mmp_device(&superblock, &encoded);
        let waits = Rc::new(RefCell::new(Vec::new()));

        let error = MmpState::claim(
            &mut device,
            &superblock,
            &mut TestEntropy(1),
            &mut TestDelay(Rc::clone(&waits)),
        )
        .unwrap_err();
        assert_eq!(error.kind(), Ext4ErrorKind::Busy);
        assert!(waits.borrow().is_empty());
        assert!(writes.borrow().is_empty());
    }

    #[test]
    fn clean_mmp_claim_rechecks_the_published_sequence() {
        let superblock = mmp_superblock();
        let encoded = valid_mmp_bytes(&superblock);
        let (mut device, reads, ..) = mmp_device(&superblock, &encoded);

        MmpState::claim(
            &mut device,
            &superblock,
            &mut TestEntropy(0x1234_5678),
            &mut TestDelay(Rc::new(RefCell::new(Vec::new()))),
        )
        .expect("clean MMP block can be claimed");

        assert_eq!(
            reads.get(),
            2,
            "Linux rereads even a clean MMP block after publishing the claim"
        );
    }

    #[test]
    fn refresh_write_failure_latches_mmp_and_blocks_future_mutation() {
        let superblock = mmp_superblock();
        let encoded = valid_mmp_bytes(&superblock);
        let (mut device, _, _, fail_writes) = mmp_device(&superblock, &encoded);
        let mut state = MmpState::claim(
            &mut device,
            &superblock,
            &mut TestEntropy(0x1234_5678),
            &mut TestDelay(Rc::new(RefCell::new(Vec::new()))),
        )
        .unwrap();

        fail_writes.set(true);
        assert_eq!(
            state
                .refresh(
                    &mut device,
                    &superblock,
                    MmpIdentity::default(),
                    Duration::from_secs(5),
                )
                .unwrap_err()
                .kind(),
            Ext4ErrorKind::Io
        );
        assert_eq!(
            state.ensure_writable("test:mutation").unwrap_err().kind(),
            Ext4ErrorKind::Io
        );
    }
}
