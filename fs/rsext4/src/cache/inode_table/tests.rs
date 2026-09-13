use super::*;
use crate::{DeviceCapabilities, DeviceGeometry, SectorId};

#[test]
fn reader_observes_updates_without_owning_the_mutation_or_io_lock() {
    let (mut cache, device, number) = populated_cache();
    let reader = cache.reader();
    assert_eq!(reader.try_get(number).unwrap().i_size_lo, 5);
    let mut io = device.into_inner();
    io.reader = Some(reader.clone());
    let mut device = Jbd2Dev::initial_jbd2dev(0, io, false);

    cache
        .modify(&mut device, number, AbsoluteBN::new(0), 0, |inode, _| {
            assert_eq!(reader.try_get(number).unwrap().i_size_lo, 5);
            inode.i_size_lo = 7;
            Ok(())
        })
        .unwrap();
    assert_eq!(reader.try_get(number).unwrap().i_size_lo, 7);
    cache.flush(&mut device, number).unwrap();
    assert_eq!(reader.try_get(number).unwrap().i_size_lo, 7);
}

#[test]
fn failed_mutation_keeps_the_original_reader_snapshot() {
    let (mut cache, mut device, number) = populated_cache();
    let reader = cache.reader();
    let cause = Ext4Error::no_space();
    assert_eq!(
        cache.modify(&mut device, number, AbsoluteBN::new(0), 0, |inode, _| {
            inode.i_size_lo = 99;
            Err(cause)
        }),
        Err(cause)
    );
    assert_eq!(reader.try_get(number).unwrap().i_size_lo, 5);
}

#[test]
fn rollback_restores_contents_without_changing_reader_identity() {
    let (mut cache, mut device, number) = populated_cache();
    let reader = cache.reader();
    let outer = cache.pause_readers();
    let snapshot = cache.clone();
    let nested = cache.pause_readers();
    cache
        .modify(&mut device, number, AbsoluteBN::new(0), 0, |inode, _| {
            inode.i_size_lo = 99;
            Ok(())
        })
        .unwrap();
    assert!(reader.try_get(number).is_none());
    drop(nested);
    assert!(reader.try_get(number).is_none());
    cache.restore_snapshot(snapshot);
    assert!(reader.try_get(number).is_none());
    drop(outer);
    assert_eq!(reader.try_get(number).unwrap().i_size_lo, 5);
}

#[test]
fn reader_returns_miss_after_eviction_clear_or_owner_retirement() {
    let (mut cache, mut device, number) = populated_cache();
    let reader = cache.reader();
    let held = cache.cache.entries.lock();
    assert!(reader.try_get(number).is_none());
    drop(held);
    let snapshot = cache.clone();
    cache.evict(&mut device, number).unwrap();
    assert!(reader.try_get(number).is_none());
    cache.restore_snapshot(snapshot);
    assert_eq!(reader.try_get(number).unwrap().i_size_lo, 5);
    cache.clear();
    assert!(reader.try_get(number).is_none());
    let (replacement, _, replacement_number) = populated_cache();
    cache = replacement;
    assert!(reader.try_get(number).is_none());
    let replacement_reader = cache.reader();
    assert!(replacement_reader.try_get(replacement_number).is_some());
    drop(cache);
    assert!(replacement_reader.try_get(replacement_number).is_none());
}

#[test]
fn test_inode_location_calc() {
    let cache = InodeCache::default(DEFAULT_INODE_SIZE);
    let inodes_per_group = 128;
    let inode_table_start = AbsoluteBN::new(100);

    let (block, offset, group) = cache
        .calc_inode_location(
            InodeNumber::new(1).unwrap(),
            inodes_per_group,
            inode_table_start,
            BLOCK_SIZE,
        )
        .unwrap();
    assert_eq!(block, inode_table_start);
    assert_eq!(offset, 0);
    assert_eq!(group, BGIndex::new(0));

    let inodes_per_block = (BLOCK_SIZE / DEFAULT_INODE_SIZE as usize) as u32;
    let (block, offset, group) = cache
        .calc_inode_location(
            InodeNumber::new(inodes_per_block + 1).unwrap(),
            inodes_per_group,
            inode_table_start,
            BLOCK_SIZE,
        )
        .unwrap();
    assert_eq!(block, AbsoluteBN::new(101));
    assert_eq!(offset, 0);
    assert_eq!(group, BGIndex::new(0));
}

#[test]
fn demand_load_reuses_the_canonical_cache_without_dirty_eviction() {
    let (mut cache, _, number) = populated_cache();
    cache.max_entries = 1;
    cache.mark_dirty(number);
    let other = InodeNumber::new(2).unwrap();
    let pending = cache.prepare_load(other);
    let record = CachedInode::new(
        Ext4Inode::default(),
        alloc::vec![0; 256],
        other,
        AbsoluteBN::new(0),
        256,
    );

    assert!(cache.publish_load(&pending, record).unwrap().is_some());
    assert!(cache.get(number).unwrap().dirty);
    assert!(cache.get(other).is_none());
    assert_eq!(cache.stats().total_entries, 1);
}

#[test]
fn demand_load_can_replace_a_clean_record() {
    let (mut cache, _, number) = populated_cache();
    cache.max_entries = 1;
    let other = InodeNumber::new(2).unwrap();
    let pending = cache.prepare_load(other);
    let record = CachedInode::new(
        Ext4Inode::default(),
        alloc::vec![0; 256],
        other,
        AbsoluteBN::new(0),
        256,
    );

    cache.publish_load(&pending, record).unwrap().unwrap();
    assert!(cache.get(number).is_none());
    assert!(cache.reader().try_get(other).is_some());
    assert_eq!(cache.stats().total_entries, 1);
}

#[test]
fn unrelated_inode_mutation_does_not_cancel_a_pending_load() {
    let (mut cache, mut device, number) = populated_cache();
    let other = InodeNumber::new(2).unwrap();
    let pending = cache.prepare_load(other);
    cache
        .modify(&mut device, number, AbsoluteBN::new(0), 0, |inode, _| {
            inode.i_size_lo = 17;
            Ok(())
        })
        .unwrap();
    assert!(cache.validate_load(&pending).unwrap());
}

#[test]
fn current_inode_wins_over_a_cancelled_demand_load() {
    let (mut cache, mut device, number) = populated_cache();
    let stale = cache.get(number).unwrap();
    let pending = cache.prepare_load(number);
    cache
        .modify(&mut device, number, AbsoluteBN::new(0), 0, |inode, _| {
            inode.i_size_lo = 17;
            Ok(())
        })
        .unwrap();
    assert!(!cache.validate_load(&pending).unwrap());
    assert_eq!(
        cache
            .publish_load(&pending, stale)
            .unwrap()
            .unwrap()
            .i_size_lo,
        17
    );
}

#[test]
fn rollback_and_eviction_do_not_resurrect_an_old_demand_load() {
    let (mut cache, mut device, number) = populated_cache();
    let record = cache.get(number).unwrap();
    let snapshot = cache.clone();
    let pending = cache.prepare_load(number);
    cache.restore_snapshot(snapshot);
    assert!(!cache.validate_load(&pending).unwrap());
    cache.evict(&mut device, number).unwrap();
    assert!(cache.publish_load(&pending, record).unwrap().is_none());
    let fresh = cache.prepare_load(number);
    assert!(cache.validate_load(&fresh).unwrap());
    assert!(!cache.validate_load(&pending).unwrap());
}

#[test]
fn a_foreign_cache_cannot_accept_a_load_even_when_its_inode_is_cached() {
    let (mut origin, _, number) = populated_cache();
    let pending = origin.prepare_load(number);
    let record = origin.get(number).unwrap();
    let (mut foreign, ..) = populated_cache();
    assert_eq!(
        foreign.publish_load(&pending, record).unwrap_err().kind(),
        Ext4ErrorKind::InvalidInput
    );
    assert_eq!(foreign.get(number).unwrap().inode.i_size_lo, 5);
}

#[test]
fn abandoned_load_registrations_are_retired_on_the_next_preparation() {
    let (mut cache, _, number) = populated_cache();
    let pending = cache.prepare_load(number);
    let concurrent = cache.prepare_load(number);
    assert_eq!(cache.pending_reads.len(), 1);
    cache.mark_dirty(number);
    assert!(!cache.validate_load(&pending).unwrap());
    assert!(!cache.validate_load(&concurrent).unwrap());
    drop(pending);
    drop(concurrent);
    let other = InodeNumber::new(2).unwrap();
    let _fresh = cache.prepare_load(other);
    assert_eq!(cache.pending_reads.len(), 1);
    assert!(!cache.pending_reads.contains_key(&number));
}

struct MemoryDevice {
    bytes: Vec<u8>,
    reader: Option<InodeCacheReader>,
}

impl crate::Clock for MemoryDevice {
    fn now(&self) -> Ext4Result<crate::Ext4Timestamp> {
        Ok(crate::Ext4Timestamp::UNIX_EPOCH)
    }
}

impl BlockIo for MemoryDevice {
    fn geometry(&self) -> DeviceGeometry {
        DeviceGeometry::new(BLOCK_SIZE_U32, 8)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            flush: true,
            ..Default::default()
        }
    }

    fn read(&mut self, bytes: &mut [u8], sector: SectorId, _: u32) -> Ext4Result<()> {
        self.observe_unlocked_cache();
        let offset = sector.as_usize()? * BLOCK_SIZE;
        bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
        Ok(())
    }

    fn write(&mut self, bytes: &[u8], sector: SectorId, _: u32) -> Ext4Result<()> {
        self.observe_unlocked_cache();
        let offset = sector.as_usize()? * BLOCK_SIZE;
        self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.observe_unlocked_cache();
        Ok(())
    }
}

impl MemoryDevice {
    fn observe_unlocked_cache(&self) {
        if let Some(reader) = &self.reader {
            assert!(
                reader.try_get(InodeNumber::new(1).unwrap()).is_some(),
                "I/O retained cache exclusion"
            );
        }
    }
}

fn populated_cache() -> (InodeCache, Jbd2Dev<MemoryDevice>, InodeNumber) {
    let cache = InodeCache::default(DEFAULT_INODE_SIZE);
    let number = InodeNumber::new(1).unwrap();
    let inode = Ext4Inode {
        i_size_lo: 5,
        ..Default::default()
    };
    cache.cache.entries.lock().insert(
        number,
        CachedInode::new(
            inode,
            alloc::vec![0; DEFAULT_INODE_SIZE as usize],
            number,
            AbsoluteBN::new(0),
            0,
        ),
    );
    let device = Jbd2Dev::initial_jbd2dev(
        0,
        MemoryDevice {
            bytes: alloc::vec![0; 8 * BLOCK_SIZE],
            reader: None,
        },
        false,
    );
    (cache, device, number)
}
