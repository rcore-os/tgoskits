//! Cold extent-node I/O under real mounted and inode ownership boundaries.

use core::ops::Range;

use super::*;

std::thread_local! {
    static MAPPING_READS: RefCell<Option<Vec<Range<u64>>>> = const { RefCell::new(None) };
    static PROBE: RefCell<Option<MappingProbe>> = const { RefCell::new(None) };
}

struct MappingProbe {
    filesystem: Arc<Ext4Filesystem>,
    inode_lock: Arc<AccessGate>,
    sectors: Vec<Range<u64>>,
    reads: usize,
    action: MappingAction,
}

enum MappingAction {
    None,
    WriteOther(Arc<Inode>),
    Fail,
}

struct MappingGuard;

impl Drop for MappingGuard {
    fn drop(&mut self) {
        let probe = PROBE.with_borrow_mut(Option::take);
        drop(probe);
    }
}

#[test]
fn cold_extent_io_releases_mount_state_and_another_inode_can_write() {
    let (filesystem, input, expected) = fragmented_file();
    let other = create_inode(&filesystem, b"other", b"before");
    let probe = watch(
        &filesystem,
        &input,
        MappingAction::WriteOther(other.clone()),
    );
    let mut output = alloc::vec![0xa5; expected.len() + 3];

    assert_eq!(input.read_at(&mut output, 0), Ok(expected.len()));

    assert_eq!(&output[..expected.len()], expected);
    assert_eq!(&output[expected.len()..], &[0xa5; 3]);
    PROBE.with_borrow(|slot| {
        let probe = slot.as_ref().unwrap();
        assert!(probe.reads > 0, "no physical extent-node read observed");
        assert!(
            matches!(probe.action, MappingAction::None),
            "other inode made no progress"
        );
    });
    drop(probe);
    let mut bytes = [0; 6];
    assert_eq!(other.read_at(&mut bytes, 0), Ok(6));
    assert_eq!(&bytes, b"during");
}

#[test]
fn failed_extent_io_leaves_output_untouched_and_releases_read_admission() {
    let (filesystem, input, expected) = fragmented_file();
    let probe = watch(&filesystem, &input, MappingAction::Fail);
    let mut output = alloc::vec![0xa5; expected.len()];

    assert_eq!(input.read_at(&mut output, 0), Err(VfsError::Io));

    assert!(output.iter().all(|byte| *byte == 0xa5));
    PROBE.with_borrow(|slot| {
        let probe = slot.as_ref().unwrap();
        assert!(probe.reads > 0);
        assert!(probe.inode_lock.try_write().is_some());
    });
    filesystem.admission.close_and_drain().unwrap();
    filesystem.admission.reopen();
    drop(probe);
    assert_eq!(input.read_at(&mut output, 0), Ok(expected.len()));
    assert_eq!(output, expected);
}

fn fragmented_file() -> (Arc<Ext4Filesystem>, Arc<Inode>, Vec<u8>) {
    let (storage, flushes) = formatted_test_storage();
    let (filesystem, _) = mount_test_storage(storage.clone(), flushes.clone(), false);
    let filesystem = Arc::new(filesystem);
    filesystem.lock().ext4.use_shared_device_cache().unwrap();
    let input = create_inode(&filesystem, b"input", &[]);
    let number = InodeNumber::new(input.inode().try_into().unwrap()).unwrap();
    let mut expected = alloc::vec![0; 17 * 4096];
    for index in 0..9 {
        let start = index * 2 * 4096;
        let block = &mut expected[start..start + 4096];
        block.fill(index as u8 + 1);
        assert_eq!(input.write_at(block, start as u64), Ok(block.len()));
    }
    drop(input);
    filesystem.shutdown_quiescent().unwrap();
    drop(filesystem);
    let (filesystem, _) = mount_test_storage(storage, flushes, false);
    let filesystem = Arc::new(filesystem);
    let lifetime = {
        let mut state = filesystem.lock();
        state.ext4.use_shared_device_cache().unwrap();
        state.ext4.inode(number).unwrap();
        state.retain_inode(&filesystem, number)
    };
    (filesystem, Inode::new(lifetime, None), expected)
}

fn watch(filesystem: &Arc<Ext4Filesystem>, input: &Inode, action: MappingAction) -> MappingGuard {
    let number = InodeNumber::new(input.inode().try_into().unwrap()).unwrap();
    // Warm inode metadata excludes table I/O. FIEMAP reads mappings, never file
    // payloads; record those actual device addresses instead of guessing disk
    // layout or weakening the assertion to ordinary data reads. A clean remount
    // and shared endpoint leave these extent nodes cold on the next query too.
    assert!(filesystem.inode_metadata.try_get(number).unwrap().is_some());
    MAPPING_READS.with_borrow_mut(|slot| {
        assert!(slot.is_none());
        *slot = Some(Vec::new());
    });
    let mapping = filesystem
        .lock()
        .ext4
        .inode_extents(number, 0, 17 * 4096, rsext4::FileExtentTarget::Data, 32)
        .unwrap();
    let sectors = MAPPING_READS.with_borrow_mut(Option::take).unwrap();
    assert_eq!(mapping.extents.len(), 9);
    assert!(
        !sectors.is_empty(),
        "fixture has no cold external extent-node reads"
    );
    PROBE.with_borrow_mut(|slot| {
        assert!(slot.is_none());
        *slot = Some(MappingProbe {
            filesystem: filesystem.clone(),
            inode_lock: filesystem.inode_access(number),
            sectors,
            reads: 0,
            action,
        });
    });
    MappingGuard
}

pub(super) fn observe_device_read(sector: u64, bytes: usize) -> BlockResult {
    let end = sector + (bytes / TEST_SECTOR_BYTES) as u64;
    MAPPING_READS.with_borrow_mut(|slot| {
        if let Some(ranges) = slot {
            ranges.push(sector..end);
        }
    });
    let Some(mut probe) = PROBE.with_borrow_mut(Option::take) else {
        return Ok(());
    };
    let mut result = Ok(());
    if probe
        .sectors
        .iter()
        .any(|range| sector < range.end && range.start < end)
    {
        assert!(
            probe.filesystem.inner.try_lock().is_some(),
            "extent-node I/O retained the mount lock"
        );
        assert!(
            probe.inode_lock.try_write().is_none(),
            "extent-node I/O lost content protection"
        );
        assert!(
            probe.inode_lock.try_read().unwrap().is_some(),
            "extent-node I/O excluded independent readers"
        );
        probe.reads += 1;
        match core::mem::replace(&mut probe.action, MappingAction::None) {
            MappingAction::None => {}
            MappingAction::WriteOther(other) => assert_eq!(other.write_at(b"during", 0), Ok(6)),
            MappingAction::Fail => result = Err(BlockError::Io),
        }
    }
    PROBE.with_borrow_mut(|slot| *slot = Some(probe));
    result
}
