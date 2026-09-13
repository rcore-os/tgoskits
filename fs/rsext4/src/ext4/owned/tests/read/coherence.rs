//! Authoritative images, invalidation and boundary behavior of owned file reads.

use super::*;
use crate::bmalloc::AbsoluteBN;

#[test]
fn dirty_file_images_override_home_bytes_without_device_reads() {
    let (mut mount, number) = cold_file(b"hello");
    let physical = first_block(&mut mount, number);
    let cached = mount
        .filesystem
        .datablock_cache
        .create_new(&mut mount.device, physical)
        .unwrap();
    assert!(cached.dirty);
    let prepared = prepare(&mut mount, number, 5);
    let before = READS.get();
    let failure = FailRead::new();
    let completed = prepared.execute(&mut Visible(&mut mount));
    drop(failure);
    assert_eq!(
        READS.get(),
        before,
        "dirty image fell through to old home bytes"
    );
    assert_completed(&mut mount, &completed, &[0; 5]);
}

#[cfg(feature = "USE_MULTILEVEL_CACHE")]
#[test]
fn journal_only_file_images_override_home_bytes_without_device_reads() {
    let (mut mount, number) = cold_file(b"hello");
    let physical = first_block(&mut mount, number);
    mount.enable_background_writeback().unwrap();
    mount
        .filesystem
        .datablock_cache
        .create_new(&mut mount.device, physical)
        .unwrap();
    mount
        .filesystem
        .datablock_cache
        .flush_metadata(&mut mount.device, physical)
        .unwrap();
    // Exercise the journal visibility boundary explicitly. Ordinary file-data
    // writeback is direct and does not itself produce a journal-owned image.
    mount.filesystem.datablock_cache.clear();
    assert!(mount.device.visible_block_image(physical).is_some());
    let mut home = alloc::vec![0; mount.filesystem.block_size()];
    mount
        .device
        .fork_read_endpoint()
        .unwrap()
        .read(&mut home, SectorId::new(physical.raw() * 8), 8)
        .unwrap();
    assert_eq!(
        &home[..5],
        b"hello",
        "fixture prematurely wrote journal bytes home"
    );
    let prepared = prepare(&mut mount, number, 5);
    let before = READS.get();
    let failure = FailRead::new();
    let completed = prepared.execute(&mut Visible(&mut mount));
    drop(failure);
    assert_eq!(READS.get(), before);
    assert_completed(&mut mount, &completed, &[0; 5]);
}

#[test]
fn superseded_bytes_and_errors_both_request_a_fresh_read() {
    for fail in [false, true] {
        let (mut mount, number, expected) = fragmented_file();
        let prepared = prepare(&mut mount, number, expected.len());
        let failure = fail.then(FailRead::new);
        let completed = prepared.execute(&mut Visible(&mut mount));
        drop(failure);
        mount
            .filesystem
            .modify_inode(&mut mount.device, number, |inode| inode.set_uid(73))
            .unwrap();
        assert!(
            mount
                .finish_inode_read(&completed, expected.len())
                .unwrap()
                .is_none()
        );
        let completed =
            prepare(&mut mount, number, expected.len()).execute(&mut Visible(&mut mount));
        assert_completed(&mut mount, &completed, &expected);
        assert_eq!(mount.inode(number).unwrap().uid, 73);
    }
}

#[test]
fn unrelated_metadata_does_not_invalidate_file_bytes() {
    let (mut mount, number) = cold_file(b"hello");
    let completed = prepare(&mut mount, number, 5).execute(&mut Visible(&mut mount));
    let other = mount.root_inode();
    mount
        .filesystem
        .modify_inode(&mut mount.device, other, |inode| inode.set_uid(73))
        .unwrap();
    assert_completed(&mut mount, &completed, b"hello");
}

#[test]
fn rolled_back_metadata_still_invalidates_a_completed_read() {
    let (mut mount, number) = cold_file(b"hello");
    let before = mount.inode(number).unwrap();
    let completed = prepare(&mut mount, number, 5).execute(&mut Visible(&mut mount));
    let result: Ext4Result<()> =
        mount
            .filesystem
            .with_metadata_transaction(&mut mount.device, 4, |filesystem, device| {
                filesystem.modify_inode(device, number, |inode| inode.set_uid(73))?;
                Err(Ext4Error::no_space())
            });
    assert_eq!(result, Err(Ext4Error::no_space()));
    assert!(mount.finish_inode_read(&completed, 5).unwrap().is_none());
    assert_eq!(mount.inode(number).unwrap().uid, before.uid);
    let completed = prepare(&mut mount, number, 5).execute(&mut Visible(&mut mount));
    assert_completed(&mut mount, &completed, b"hello");
}

#[test]
fn unwritten_extents_and_eof_return_zeros_and_leave_output_tails_untouched() {
    let (mut mount, number) = cold_file(b"hello");
    mount
        .preallocate_inode(number, 4096, 4096, PreallocationOptions::EXTEND_SIZE)
        .unwrap();
    let mut expected = alloc::vec![0; 8192];
    expected[..5].copy_from_slice(b"hello");
    make_cold(&mut mount);
    let completed =
        prepare(&mut mount, number, expected.len() + 17).execute(&mut Visible(&mut mount));
    assert_completed(&mut mount, &completed, &expected);
    let InodeReadPreparation::Read(prepared) = mount.prepare_inode_read(number, 8189, 9).unwrap()
    else {
        panic!("short EOF read must retain independent ownership");
    };
    let completed = prepared.execute(&mut Visible(&mut mount));
    assert_completed(&mut mount, &completed, &[0; 3]);
    let InodeReadPreparation::Read(prepared) = mount.prepare_inode_read(number, 8192, 9).unwrap()
    else {
        panic!("EOF read must retain independent ownership");
    };
    let before = READS.get();
    let completed = prepared.execute(&mut Visible(&mut mount));
    assert_completed(&mut mount, &completed, &[]);
    assert_eq!(READS.get(), before);
}

#[test]
fn empty_overflowing_and_oversized_requests_preserve_explicit_phases() {
    let (mut mount, number) = cold_file(b"hello");
    assert!(matches!(
        mount.prepare_inode_read(number, u64::MAX, 0).unwrap(),
        InodeReadPreparation::Empty
    ));
    assert!(matches!(
        mount.prepare_inode_read(number, u64::MAX, 5).unwrap(),
        InodeReadPreparation::Serialized
    ));
    assert!(matches!(
        mount
            .prepare_inode_read(number, 0, crate::file::MAX_READ_BYTES + 1)
            .unwrap(),
        InodeReadPreparation::Serialized
    ));
}

#[test]
fn invalid_mapping_and_data_image_sizes_preserve_typed_errors() {
    for (mut mount, number, expected) in [fragmented_file(), {
        let (mount, number) = cold_file(b"hello");
        (mount, number, b"hello".to_vec())
    }] {
        let completed = prepare(&mut mount, number, expected.len()).execute(&mut ShortImage);
        assert_eq!(
            mount
                .finish_inode_read(&completed, expected.len())
                .unwrap_err()
                .kind(),
            Ext4ErrorKind::Corrupted
        );
    }
}

#[test]
fn an_incomplete_visibility_batch_is_rejected_before_data_reads() {
    let (mut mount, number) = cold_file(b"hello");
    let prepared = prepare(&mut mount, number, 5);
    let before = READS.get();
    let completed = prepared.execute(&mut MissingBatch);
    assert_eq!(
        mount.finish_inode_read(&completed, 5).unwrap_err().kind(),
        Ext4ErrorKind::Corrupted
    );
    assert_eq!(READS.get(), before);
}

fn first_block(mount: &mut TestMount, number: InodeNumber) -> AbsoluteBN {
    let mapping = mount
        .inode_extents(number, 0, 5, FileExtentTarget::Data, 1)
        .unwrap();
    AbsoluteBN::new(mapping.extents[0].physical_start / mount.filesystem.block_size() as u64)
}

struct ShortImage;

impl InodeReadCache for ShortImage {
    fn visible(&mut self, _: &InodeBlockRequest) -> Ext4Result<Option<Arc<Vec<u8>>>> {
        Ok(Some(Arc::new(alloc::vec![0; 1])))
    }
}

struct MissingBatch;

impl InodeReadCache for MissingBatch {
    fn visible(&mut self, _: &InodeBlockRequest) -> Ext4Result<Option<Arc<Vec<u8>>>> {
        panic!("root extent mapping needs no metadata block read");
    }

    fn visible_data(&mut self, _: &InodeDataRequest) -> Ext4Result<Vec<Option<Arc<Vec<u8>>>>> {
        Ok(Vec::new())
    }
}
