//! Behavior contracts of the shared block cache. Device IO is recorded by
//! a mock so cache hits, deferred writes, merged writeback runs, and
//! writeback-before-barrier ordering are asserted deterministically.

use std::{
    num::NonZeroUsize,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::{
    address_space::{BlockAddressSpace, FolioGeometry},
    folio::CacheFolio,
    folio_cache::FolioCache,
    *,
};
use crate::{
    BlockError, BlockResult,
    block::{BlockRegion, FsBlockDevice, RegionBlockDevice},
};

const KEY_A: usize = 0x1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IoOp {
    Read { lba: u64, blocks: usize },
    Write { lba: u64, blocks: usize },
    FuaWrite { lba: u64, blocks: usize },
    Flush,
}

impl IoOp {
    fn is_write_of(&self, lba: u64, blocks: usize) -> bool {
        matches!(
            *self,
            IoOp::Write { lba: l, blocks: n } | IoOp::FuaWrite { lba: l, blocks: n }
                if l == lba && n == blocks
        )
    }

    fn is_read(&self) -> bool {
        matches!(self, IoOp::Read { .. })
    }

    fn is_flush(&self) -> bool {
        matches!(self, IoOp::Flush)
    }
}

struct RecordingState {
    storage: Vec<u8>,
    log: Vec<IoOp>,
    fail_writes: bool,
    partially_commit_blocks: Option<usize>,
}

#[derive(Clone)]
struct RecordingDevice {
    state: Arc<Mutex<RecordingState>>,
    block_size: usize,
}

/// Models the runtime endpoint whose final handle drop performs shutdown.
struct ShutdownTrackedDevice {
    inner: RecordingDevice,
    shutdowns: Arc<AtomicUsize>,
}

impl Drop for ShutdownTrackedDevice {
    fn drop(&mut self) {
        self.shutdowns.fetch_add(1, Ordering::AcqRel);
    }
}

impl FsBlockDevice for ShutdownTrackedDevice {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn num_blocks(&self) -> u64 {
        self.inner.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult<()> {
        self.inner.read_block(block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> BlockResult<()> {
        self.inner.write_block(block_id, buf)
    }

    fn flush(&mut self) -> BlockResult<()> {
        self.inner.flush()
    }
}

struct GeometryOnlyDevice {
    block_size: usize,
    io_calls: Arc<AtomicUsize>,
}

impl FsBlockDevice for GeometryOnlyDevice {
    fn name(&self) -> &str {
        "geometry-only"
    }

    fn num_blocks(&self) -> u64 {
        1
    }

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn read_block(&mut self, _block_id: u64, _buf: &mut [u8]) -> BlockResult<()> {
        self.io_calls.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn write_block(&mut self, _block_id: u64, _buf: &[u8]) -> BlockResult<()> {
        self.io_calls.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn flush(&mut self) -> BlockResult<()> {
        self.io_calls.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

impl RecordingDevice {
    fn new(blocks: usize, block_size: usize) -> (Self, Arc<Mutex<RecordingState>>) {
        let state = Arc::new(Mutex::new(RecordingState {
            storage: vec![0u8; blocks * block_size],
            log: Vec::new(),
            fail_writes: false,
            partially_commit_blocks: None,
        }));
        (
            Self {
                state: state.clone(),
                block_size,
            },
            state,
        )
    }
}

impl FsBlockDevice for RecordingDevice {
    fn name(&self) -> &str {
        "recording"
    }

    fn num_blocks(&self) -> u64 {
        (self.state.lock().unwrap().storage.len() / self.block_size) as u64
    }

    fn block_size(&self) -> usize {
        self.block_size
    }

    #[cfg(feature = "ext4")]
    fn supports_fua(&self) -> bool {
        true
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult<()> {
        let mut state = self.state.lock().unwrap();
        state.log.push(IoOp::Read {
            lba: block_id,
            blocks: buf.len() / self.block_size,
        });
        let start = block_id as usize * self.block_size;
        let end = start
            .checked_add(buf.len())
            .ok_or(BlockError::InvalidRequest)?;
        let storage = &mut state.storage;
        if end > storage.len() {
            return Err(BlockError::InvalidRequest);
        }
        buf.copy_from_slice(&storage[start..end]);
        Ok(())
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> BlockResult<()> {
        let mut state = self.state.lock().unwrap();
        if state.fail_writes {
            return Err(BlockError::Io);
        }
        if let Some(blocks) = state.partially_commit_blocks.take() {
            let blocks = blocks.min(buf.len() / self.block_size);
            let bytes = blocks * self.block_size;
            state.log.push(IoOp::Write {
                lba: block_id,
                blocks,
            });
            let start = block_id as usize * self.block_size;
            state.storage[start..start + bytes].copy_from_slice(&buf[..bytes]);
            return Err(BlockError::Io);
        }
        state.log.push(IoOp::Write {
            lba: block_id,
            blocks: buf.len() / self.block_size,
        });
        let start = block_id as usize * self.block_size;
        state.storage[start..start + buf.len()].copy_from_slice(buf);
        Ok(())
    }

    #[cfg(feature = "ext4")]
    fn write_block_fua(&mut self, block_id: u64, buf: &[u8]) -> BlockResult<()> {
        let mut state = self.state.lock().unwrap();
        if state.fail_writes {
            return Err(BlockError::Io);
        }
        state.log.push(IoOp::FuaWrite {
            lba: block_id,
            blocks: buf.len() / self.block_size,
        });
        let start = block_id as usize * self.block_size;
        state.storage[start..start + buf.len()].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> BlockResult<()> {
        self.state.lock().unwrap().log.push(IoOp::Flush);
        Ok(())
    }
}

fn count_ops(state: &Arc<Mutex<RecordingState>>, pred: impl Fn(&IoOp) -> bool) -> usize {
    state
        .lock()
        .unwrap()
        .log
        .iter()
        .filter(|op| pred(op))
        .count()
}

fn write_pattern(state: &Arc<Mutex<RecordingState>>, block: usize, block_size: usize, byte: u8) {
    let start = block * block_size;
    state.lock().unwrap().storage[start..start + block_size].fill(byte);
}

fn buffered(key: usize, device: RecordingDevice) -> BufferedBlockDevice<RecordingDevice> {
    // The registry keeps an equivalent device as the global-sync endpoint.
    let endpoint = device.clone();
    BufferedBlockDevice::with_device_key(key, Box::new(endpoint), device)
        .expect("recording device geometry is valid")
}

#[test]
fn read_is_served_from_cache_on_second_access() {
    let (device, state) = RecordingDevice::new(64, 512);
    let mut cached = buffered(KEY_A, device);
    write_pattern(&state, 1, 512, 0xAB);

    let mut buf = [0u8; 512];
    cached.read_block(1, &mut buf).unwrap();
    assert_eq!(count_ops(&state, IoOp::is_read), 1);
    assert!(buf.iter().all(|&b| b == 0xAB));

    let mut buf = [0u8; 512];
    cached.read_block(1, &mut buf).unwrap();
    assert_eq!(count_ops(&state, IoOp::is_read), 1, "second read must hit");
    assert!(buf.iter().all(|&b| b == 0xAB));
}

#[test]
fn partial_folio_reads_track_slot_state() {
    let (device, state) = RecordingDevice::new(64, 512);
    let mut cached = buffered(KEY_A + 1, device);
    // One 4 KiB folio covers 8 blocks; reading blocks 1 and 3 of frame 0
    // must read exactly those blocks and mark only those slots uptodate.
    let mut buf = [0u8; 512];
    cached.read_block(1, &mut buf).unwrap();
    cached.read_block(3, &mut buf).unwrap();
    assert_eq!(count_ops(&state, IoOp::is_read), 2);

    cached.read_block(1, &mut buf).unwrap();
    cached.read_block(3, &mut buf).unwrap();
    assert_eq!(count_ops(&state, IoOp::is_read), 2);

    // The neighboring block 2 was never read and stays uncached.
    cached.read_block(2, &mut buf).unwrap();
    assert_eq!(count_ops(&state, IoOp::is_read), 3);
}

#[test]
fn write_is_deferred_until_flush() {
    let (device, state) = RecordingDevice::new(64, 512);
    let mut cached = buffered(KEY_A + 2, device);

    let data = [0x5Au8; 512];
    cached.write_block(3, &data).unwrap();
    assert_eq!(count_ops(&state, |op| op.is_write_of(3, 1)), 0);
    assert!(
        !state.lock().unwrap().storage[3 * 512..4 * 512]
            .iter()
            .any(|&b| b == 0x5A)
    );

    cached.flush().unwrap();
    assert_eq!(count_ops(&state, |op| op.is_write_of(3, 1)), 1);
    assert!(
        state.lock().unwrap().storage[3 * 512..4 * 512]
            .iter()
            .all(|&b| b == 0x5A)
    );
    // flush() = writeback followed by the device barrier.
    let log = state.lock().unwrap().log.clone();
    assert_eq!(*log.last().unwrap(), IoOp::Flush);
}

#[cfg(feature = "ext4")]
#[test]
fn fua_bypasses_deferred_write_and_refreshes_cached_bytes() {
    let (device, state) = RecordingDevice::new(64, 512);
    let mut cached = buffered(KEY_A + 24, device);
    let old = [0x11u8; 512];
    let durable = [0x5au8; 512];

    cached.write_block(3, &old).unwrap();
    cached.write_block_fua(3, &durable).unwrap();

    let log = state.lock().unwrap().log.clone();
    assert_eq!(
        log,
        vec![
            IoOp::Write { lba: 3, blocks: 1 },
            IoOp::FuaWrite { lba: 3, blocks: 1 }
        ]
    );
    let reads_before = count_ops(&state, IoOp::is_read);
    let mut observed = [0u8; 512];
    cached.read_block(3, &mut observed).unwrap();
    assert_eq!(observed, durable);
    assert_eq!(count_ops(&state, IoOp::is_read), reads_before);
}

#[cfg(feature = "ext4")]
#[test]
fn failed_fua_invalidates_overlapping_cache_slots() {
    let (device, state) = RecordingDevice::new(64, 512);
    let mut cached = buffered(KEY_A + 25, device);
    let mut observed = [0u8; 512];
    cached.read_block(4, &mut observed).unwrap();
    state.lock().unwrap().fail_writes = true;

    assert_eq!(
        cached.write_block_fua(4, &[0x5au8; 512]),
        Err(BlockError::Io)
    );
    state.lock().unwrap().fail_writes = false;
    let reads_before = count_ops(&state, IoOp::is_read);
    cached.read_block(4, &mut observed).unwrap();
    assert_eq!(count_ops(&state, IoOp::is_read), reads_before + 1);
}

#[test]
fn flush_merges_adjacent_dirty_runs() {
    let (device, state) = RecordingDevice::new(64, 512);
    let mut cached = buffered(KEY_A + 3, device);

    let data = [0x11u8; 512];
    cached.write_block(8, &data).unwrap();
    cached.write_block(9, &data).unwrap();
    cached.write_block(10, &data).unwrap();
    cached.write_block(13, &data).unwrap();

    cached.flush().unwrap();
    // Blocks 8..=10 merge into one write; block 13 stays separate.
    assert_eq!(count_ops(&state, |op| op.is_write_of(8, 3)), 1);
    assert_eq!(count_ops(&state, |op| op.is_write_of(13, 1)), 1);
    assert_eq!(count_ops(&state, |op| matches!(op, IoOp::Write { .. })), 2);
}

#[test]
fn dirty_writeback_precedes_barrier_and_later_writes() {
    let (device, state) = RecordingDevice::new(64, 512);
    let mut cached = buffered(KEY_A + 5, device);

    // Model of the JBD2 commit contract: a "descriptor" write, a flush
    // barrier, then a "commit record" write, then another flush. The
    // deferred data must reach the device before the first barrier, and
    // the commit record must reach it only after that barrier.
    let descriptor = [0x01u8; 512];
    let commit = [0x02u8; 512];
    let data = [0x03u8; 512];
    cached.write_block(4, &data).unwrap();
    cached.write_block(8, &descriptor).unwrap();
    cached.flush().unwrap();
    cached.write_block(12, &commit).unwrap();
    cached.flush().unwrap();

    let log = state.lock().unwrap().log.clone();
    let position = |op: IoOp| log.iter().position(|entry| *entry == op).unwrap();
    let data_write = position(IoOp::Write { lba: 4, blocks: 1 });
    let descriptor_write = position(IoOp::Write { lba: 8, blocks: 1 });
    let first_barrier = log.iter().position(IoOp::is_flush).unwrap();
    let commit_write = position(IoOp::Write { lba: 12, blocks: 1 });
    let barriers = count_ops(&state, IoOp::is_flush);
    assert_eq!(barriers, 2);
    assert!(data_write < first_barrier && descriptor_write < first_barrier);
    assert!(commit_write > first_barrier);
}

#[test]
fn lru_eviction_writes_back_dirty_victim() {
    // Eviction is exercised on a direct two-folio tree: dirtying frame 0
    // and touching two other frames must write frame 0 back before it is
    // dropped.
    let (mut inner, state) = RecordingDevice::new(64, 512);
    let mut tree = BlockAddressSpace::with_capacity(FolioGeometry::new(512).unwrap(), 2);

    let data = [0x77u8; 512];
    tree.write_buffered(&mut inner, 0, 1, &data).unwrap();
    assert!(tree.has_dirty());

    let mut buf = [0u8; 512];
    tree.read_buffered(&mut inner, 8, 1, &mut buf).unwrap();
    tree.read_buffered(&mut inner, 16, 1, &mut buf).unwrap();
    assert!(
        state.lock().unwrap().storage[..512]
            .iter()
            .all(|&b| b == 0x77)
    );
    assert!(!tree.has_dirty(), "eviction leaves no dirty accounting");

    // The evicted frame reads back from the device.
    tree.read_buffered(&mut inner, 0, 1, &mut buf).unwrap();
    assert!(buf.iter().all(|&b| b == 0x77));
}

#[test]
fn lru_hit_preserves_the_recently_used_folio() {
    let (mut inner, state) = RecordingDevice::new(64, 512);
    let mut tree = BlockAddressSpace::with_capacity(FolioGeometry::new(512).unwrap(), 2);
    let mut buf = [0u8; 512];

    tree.read_buffered(&mut inner, 0, 1, &mut buf).unwrap();
    tree.read_buffered(&mut inner, 8, 1, &mut buf).unwrap();
    tree.read_buffered(&mut inner, 0, 1, &mut buf).unwrap();
    let reads_before_eviction = count_ops(&state, IoOp::is_read);

    tree.read_buffered(&mut inner, 16, 1, &mut buf).unwrap();
    tree.read_buffered(&mut inner, 0, 1, &mut buf).unwrap();
    assert_eq!(
        count_ops(&state, IoOp::is_read),
        reads_before_eviction + 1,
        "the touched frame must remain cached when the older frame is evicted"
    );

    tree.read_buffered(&mut inner, 8, 1, &mut buf).unwrap();
    assert_eq!(count_ops(&state, IoOp::is_read), reads_before_eviction + 2);
}

#[test]
fn lru_reserve_failure_preserves_existing_entry() {
    let mut cache = FolioCache::new(NonZeroUsize::new(2).unwrap());
    cache.try_reserve_entry().unwrap();
    cache.insert_reserved(7, CacheFolio::try_new(1, 1).unwrap());

    assert_eq!(
        cache.try_reserve_for_test(usize::MAX),
        Err(BlockError::NoMemory)
    );
    assert!(cache.contains(&7));
    assert_eq!(cache.least_recent(), Some(7));
}

#[test]
fn direct_write_overlays_cached_folio() {
    let (device, state) = RecordingDevice::new(64, 512);
    let mut cached = buffered(KEY_A + 7, device);

    let mut buf = [0u8; 512];
    cached.read_block(0, &mut buf).unwrap();
    let reads_before = count_ops(&state, IoOp::is_read);

    // 16 blocks span two folios: device-direct write.
    let data = [0x42u8; 16 * 512];
    cached.write_block(0, &data).unwrap();
    assert!(
        state.lock().unwrap().storage[..16 * 512]
            .iter()
            .all(|&b| b == 0x42)
    );

    // The cached folio of block 0 was overlaid: the read is served from
    // the folio with the new bytes and no extra device read happens.
    cached.read_block(0, &mut buf).unwrap();
    assert_eq!(count_ops(&state, IoOp::is_read), reads_before);
    assert!(buf.iter().all(|&b| b == 0x42));
}

#[test]
fn failed_direct_write_invalidates_partially_updated_folios() {
    let (device, state) = RecordingDevice::new(64, 512);
    let mut cached = buffered(KEY_A + 26, device);

    let mut block = [0u8; 512];
    cached.read_block(0, &mut block).unwrap();
    assert_eq!(block, [0u8; 512]);
    cached.read_block(8, &mut block).unwrap();
    assert_eq!(block, [0u8; 512]);
    let reads_before_failure = count_ops(&state, IoOp::is_read);

    // The device commits the first folio before reporting failure for this
    // two-folio direct write. Its error gives the cache no exact completed
    // prefix, so every overlapping folio must be treated as unknown.
    state.lock().unwrap().partially_commit_blocks = Some(8);
    let direct = [0x42u8; 16 * 512];
    assert_eq!(cached.write_block(0, &direct), Err(BlockError::Io));
    assert_eq!(
        &state.lock().unwrap().storage[..8 * 512],
        &direct[..8 * 512]
    );

    block.fill(0);
    cached.read_block(0, &mut block).unwrap();
    assert_eq!(block, [0x42u8; 512]);
    cached.read_block(8, &mut block).unwrap();
    assert_eq!(block, [0u8; 512]);
    assert_eq!(
        count_ops(&state, IoOp::is_read),
        reads_before_failure + 2,
        "an indeterminate failure must invalidate every overlapping folio"
    );
}

#[test]
fn direct_read_observes_deferred_dirty_data() {
    let (device, state) = RecordingDevice::new(64, 512);
    let mut cached = buffered(KEY_A + 8, device);

    let data = [0x99u8; 512];
    cached.write_block(0, &data).unwrap();
    assert_eq!(count_ops(&state, |op| matches!(op, IoOp::Write { .. })), 0);

    // A multi-folio read must not bypass the dirty folio with stale device
    // bytes: writeback happens first, then the direct read.
    let mut buf = [0u8; 16 * 512];
    cached.read_block(0, &mut buf).unwrap();
    assert!(buf[..512].iter().all(|&b| b == 0x99));
    let log = state.lock().unwrap().log.clone();
    let writeback = log.iter().position(|op| op.is_write_of(0, 1)).unwrap();
    let direct_read = log
        .iter()
        .position(|op| matches!(op, IoOp::Read { lba: 0, blocks: 16 }))
        .unwrap();
    assert!(writeback < direct_read);
}

#[test]
fn shared_registry_serves_two_instances_from_one_tree() {
    let (device_a, state_a) = RecordingDevice::new(64, 512);
    let (device_b, state_b) = RecordingDevice::new(64, 512);
    let key = KEY_A + 9;
    write_pattern(&state_a, 5, 512, 0xEF);

    // Both instances stay alive: they must resolve to one folio tree, so
    // the second read is served without touching device B at all.
    let mut first = buffered(key, device_a);
    let mut buf = [0u8; 512];
    first.read_block(5, &mut buf).unwrap();
    assert!(buf.iter().all(|&b| b == 0xEF));

    let mut second = buffered(key, device_b);
    let mut buf = [0u8; 512];
    second.read_block(5, &mut buf).unwrap();
    assert!(buf.iter().all(|&b| b == 0xEF));
    assert_eq!(count_ops(&state_b, IoOp::is_read), 0);
}

#[test]
fn shared_wrappers_observe_dirty_and_direct_updates_without_stale_reads() {
    let (device, state) = RecordingDevice::new(64, 512);
    let key = KEY_A + 24;
    let mut first = buffered(key, device.clone());
    let mut second = buffered(key, device);

    let dirty = [0xA5u8; 512];
    first.write_block(3, &dirty).unwrap();
    let mut block = [0u8; 512];
    second.read_block(3, &mut block).unwrap();
    assert_eq!(block, dirty);
    assert_eq!(count_ops(&state, IoOp::is_read), 0);
    assert_eq!(count_ops(&state, |op| matches!(op, IoOp::Write { .. })), 0);

    let direct = [0x5Au8; 16 * 512];
    second.write_block(0, &direct).unwrap();
    block.fill(0);
    first.read_block(3, &mut block).unwrap();
    assert!(block.iter().all(|&byte| byte == 0x5A));
    assert_eq!(count_ops(&state, IoOp::is_read), 0);
}

#[test]
fn shared_partition_wrappers_keep_physical_lbas_distinct() {
    let (device, state) = RecordingDevice::new(64, 512);
    let key = KEY_A + 25;
    let mut first = RegionBlockDevice::new(buffered(key, device.clone()), BlockRegion::new(8, 8));
    let mut second = RegionBlockDevice::new(buffered(key, device), BlockRegion::new(24, 8));

    let first_data = [0x18u8; 512];
    let second_data = [0x24u8; 512];
    first.write_block(0, &first_data).unwrap();
    second.write_block(0, &second_data).unwrap();

    let mut block = [0u8; 512];
    first.read_block(0, &mut block).unwrap();
    assert_eq!(block, first_data);
    second.read_block(0, &mut block).unwrap();
    assert_eq!(block, second_data);

    second.flush().unwrap();
    {
        let state = state.lock().unwrap();
        assert_eq!(&state.storage[8 * 512..9 * 512], &first_data);
        assert_eq!(&state.storage[24 * 512..25 * 512], &second_data);
    }
    assert_eq!(count_ops(&state, |op| op.is_write_of(0, 1)), 0);
}

#[test]
fn drop_flushes_last_instance() {
    let (device, state) = RecordingDevice::new(64, 512);
    let mut cached = buffered(KEY_A + 10, device);

    let data = [0x33u8; 512];
    cached.write_block(6, &data).unwrap();
    drop(cached);

    assert!(
        state.lock().unwrap().storage[6 * 512..7 * 512]
            .iter()
            .all(|&b| b == 0x33)
    );
    // The count is a lower bound: a concurrently running global-sync test
    // may flush this live registry entry before the drop happens.
    assert!(count_ops(&state, IoOp::is_flush) >= 1);
}

#[test]
fn dropping_last_consumer_releases_registry_endpoint() {
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let key = KEY_A + 11;

    for expected_shutdowns in 1..=2 {
        let (device, _state) = RecordingDevice::new(64, 512);
        let endpoint = ShutdownTrackedDevice {
            inner: device.clone(),
            shutdowns: shutdowns.clone(),
        };
        let cached = BufferedBlockDevice::with_device_key(key, Box::new(endpoint), device)
            .expect("recording device geometry is valid");

        drop(cached);
        assert_eq!(
            shutdowns.load(Ordering::Acquire),
            expected_shutdowns,
            "each last cache consumer must release the endpoint for shutdown"
        );
    }
}

#[test]
fn folio_allocation_failure_returns_no_memory_without_io() {
    let block_size = 1usize << (usize::BITS - 1);
    let geometry = FolioGeometry::new(block_size).unwrap();
    let mut tree = BlockAddressSpace::with_capacity(geometry, 1);
    let io_calls = Arc::new(AtomicUsize::new(0));
    let mut device = GeometryOnlyDevice {
        block_size,
        io_calls: io_calls.clone(),
    };
    let mut output = [];

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        tree.read_buffered(&mut device, 0, 1, &mut output)
    }));

    assert!(matches!(outcome, Ok(Err(BlockError::NoMemory))));
    assert_eq!(
        CacheFolio::try_new(1, usize::MAX).unwrap_err(),
        BlockError::NoMemory,
        "per-block head allocation must use the same fallible error path"
    );
    assert!(!tree.has_dirty());
    assert_eq!(io_calls.load(Ordering::Acquire), 0);
}

#[test]
fn failed_writeback_retains_dirty_data_for_retry() {
    // A direct tree keeps the failing device out of the process-global
    // registry, where a live failing endpoint would break registry-wide
    // sync in concurrently running tests.
    let (mut inner, state) = RecordingDevice::new(64, 512);
    let mut tree = BlockAddressSpace::with_capacity(FolioGeometry::new(512).unwrap(), 4);

    let data = [0xD7u8; 512];
    tree.write_buffered(&mut inner, 7, 1, &data).unwrap();
    state.lock().unwrap().fail_writes = true;
    assert_eq!(tree.writeback_dirty(&mut inner, None), Err(BlockError::Io));
    assert!(tree.has_dirty());

    let mut cached = [0u8; 512];
    tree.read_buffered(&mut inner, 7, 1, &mut cached).unwrap();
    assert_eq!(
        cached, data,
        "a failed writeback must not discard dirty bytes"
    );

    state.lock().unwrap().fail_writes = false;
    tree.writeback_dirty(&mut inner, None).unwrap();
    assert!(!tree.has_dirty());
    assert_eq!(&state.lock().unwrap().storage[7 * 512..8 * 512], &data);
}

#[test]
fn non_power_of_two_block_size_is_rejected() {
    assert!(FolioGeometry::new(1000).is_err());
    assert!(FolioGeometry::new(0).is_err());
    let geometry = FolioGeometry::new(4096).unwrap();
    assert_eq!(geometry.folio_size(), 4096);
    assert_eq!(geometry.slots(), 1);
    let geometry = FolioGeometry::new(512).unwrap();
    assert_eq!(geometry.folio_size(), 4096);
    assert_eq!(geometry.slots(), 8);
    assert!(geometry.spans_one_folio(0, 8));
    assert!(!geometry.spans_one_folio(6, 3));
}

#[test]
fn invalid_request_geometry_is_rejected() {
    let (device, _state) = RecordingDevice::new(64, 512);
    let mut cached = buffered(KEY_A + 12, device);
    let mut misaligned = [0u8; 100];
    assert_eq!(
        cached.read_block(0, &mut misaligned),
        Err(BlockError::InvalidRequest)
    );
}

#[test]
fn sync_all_block_caches_writes_back_every_registered_device() {
    let (device_a, state_a) = RecordingDevice::new(64, 512);
    let (device_b, state_b) = RecordingDevice::new(64, 512);
    write_pattern(&state_a, 0, 512, 0xEE);

    // Both wrappers stay alive so their registry entries are live during
    // the global sync; the endpoints see the same recorded devices.
    let mut wrapper_a = buffered(KEY_A + 20, device_a);
    let mut wrapper_b = buffered(KEY_A + 21, device_b);
    let data = [0x66u8; 512];
    wrapper_a.write_block(1, &data).unwrap();
    wrapper_b.write_block(2, &data).unwrap();

    let _ = super::sync_all_block_caches();
    // Effect assertions: both devices persisted their dirty block and saw
    // a flush barrier, regardless of unrelated registry entries that
    // concurrently running tests keep alive.
    assert!(
        state_a.lock().unwrap().storage[512..2 * 512]
            .iter()
            .all(|&b| b == 0x66)
    );
    assert!(
        state_b.lock().unwrap().storage[2 * 512..3 * 512]
            .iter()
            .all(|&b| b == 0x66)
    );
    assert!(count_ops(&state_a, IoOp::is_flush) >= 1);
    assert!(count_ops(&state_b, IoOp::is_flush) >= 1);
}

#[test]
#[cfg(feature = "vfs")]
fn reclaim_clean_folios_drops_only_clean_frames() {
    // A direct tree pins the exact dirty/clean layout without interference
    // from the process-global registry.
    let (mut inner, state) = RecordingDevice::new(64, 512);
    let mut tree = BlockAddressSpace::with_capacity(FolioGeometry::new(512).unwrap(), 8);

    let data = [0x55u8; 512];
    tree.write_buffered(&mut inner, 0, 1, &data).unwrap();
    let mut buf = [0u8; 512];
    tree.read_buffered(&mut inner, 8, 1, &mut buf).unwrap();
    tree.read_buffered(&mut inner, 16, 1, &mut buf).unwrap();
    let reads_before = count_ops(&state, IoOp::is_read);

    let reclaimed = tree.reclaim_clean_folios(2);
    assert_eq!(reclaimed, 2, "both clean folios are reclaimable");
    assert!(tree.has_dirty(), "the dirty folio is preserved");
    // With only the dirty folio left, reclaim makes no progress.
    assert_eq!(tree.reclaim_clean_folios(4), 0);

    // The reclaimed folios read from the device again; the dirty one still
    // serves from cache (no write happened yet).
    tree.read_buffered(&mut inner, 8, 1, &mut buf).unwrap();
    tree.read_buffered(&mut inner, 16, 1, &mut buf).unwrap();
    assert_eq!(count_ops(&state, IoOp::is_read), reads_before + 2);
    assert_eq!(count_ops(&state, |op| matches!(op, IoOp::Write { .. })), 0);
}

#[test]
#[cfg(feature = "vfs")]
fn allocator_reclaim_skips_a_cache_with_its_state_lock_held() {
    let (device, _state) = RecordingDevice::new(64, 512);
    let cached = buffered(KEY_A + 22, device);

    assert_eq!(
        cached.reclaim_from_allocator_while_state_locked_for_test(),
        0,
        "allocator reclaim must skip a cache already locked by the allocating path"
    );
}

#[test]
#[cfg(feature = "vfs")]
fn cache_drop_cleanup_skips_a_contended_registry_lock() {
    let (device, _state) = RecordingDevice::new(64, 512);
    let cached = buffered(KEY_A + 23, device);

    cached.unregister_while_registry_locked_for_test();
}
