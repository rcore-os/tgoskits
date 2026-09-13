//! Real cold inode/extent reads, batched visibility and delayed publication.

use core::cell::Cell;

use super::*;
use crate::extents_tree::{ExtentNode, ExtentTree};

mod coherence;

std::thread_local! {
    static READS: Cell<usize> = const { Cell::new(0) };
    static FAIL_READ: Cell<bool> = const { Cell::new(false) };
    static DATA_QUERIES: Cell<usize> = const { Cell::new(0) };
}

#[test]
fn cold_external_extents_are_read_only_during_independent_execution() {
    let (mut mount, number, expected) = fragmented_file();
    let before = READS.get();
    let prepared = prepare(&mut mount, number, expected.len());
    assert_eq!(READS.get(), before, "preparation read extent metadata");

    let completed = prepared.execute(&mut Visible(&mut mount));

    assert!(READS.get() > before, "cold mappings/data were never read");
    assert_completed(&mut mount, &completed, &expected);
}

#[test]
fn cold_inode_table_has_an_explicit_lock_external_phase() {
    let (mut mount, number, expected) = fragmented_file();
    mount
        .filesystem
        .inodetable_cache
        .evict(&mut mount.device, number)
        .unwrap();
    let before = READS.get();
    let InodeReadPreparation::Inode(prepared) =
        mount.prepare_inode_read(number, 0, expected.len()).unwrap()
    else {
        panic!("cold inode did not produce a table-read owner");
    };
    assert_eq!(READS.get(), before);
    let completed = prepared.execute();
    assert_eq!(READS.get(), before + 1);
    let InodeReadPreparation::Read(prepared) = mount
        .finish_read_inode_load(completed, 0, expected.len())
        .unwrap()
        .unwrap()
    else {
        panic!("inode publication did not produce a file-read owner");
    };
    assert_eq!(READS.get(), before + 1, "inode publication read disk");
    let completed = prepared.execute(&mut Visible(&mut mount));
    assert_completed(&mut mount, &completed, &expected);
}

#[test]
fn contiguous_data_uses_one_visibility_batch_and_one_device_read() {
    let expected = alloc::vec![0x65; 128 * 1024];
    let (mut mount, number) = cold_file(&expected);
    let mapping = mount
        .inode_extents(number, 0, expected.len() as u64, FileExtentTarget::Data, 32)
        .unwrap();
    assert_eq!(
        mapping.extents.len(),
        1,
        "fixture is not physically contiguous"
    );
    let prepared = prepare(&mut mount, number, expected.len());
    let reads = READS.get();
    let queries = DATA_QUERIES.get();

    let completed = prepared.execute(&mut Visible(&mut mount));

    assert_eq!(
        DATA_QUERIES.get() - queries,
        1,
        "visibility split into per-block locking"
    );
    assert_eq!(
        READS.get() - reads,
        1,
        "contiguous read split into individual blocks"
    );
    assert_completed(&mut mount, &completed, &expected);
}

#[test]
fn failed_mapping_io_stays_private_until_publication_and_is_retryable() {
    let (mut mount, number, expected) = fragmented_file();
    let prepared = prepare(&mut mount, number, expected.len());
    let failure = FailRead::new();
    let completed = prepared.execute(&mut Visible(&mut mount));
    drop(failure);
    assert_eq!(
        mount
            .finish_inode_read(&completed, expected.len())
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::Io
    );
    let completed = prepare(&mut mount, number, expected.len()).execute(&mut Visible(&mut mount));
    assert_completed(&mut mount, &completed, &expected);
}

#[test]
fn short_capacity_does_not_consume_a_read_or_publish_access_time() {
    let (mut mount, number) = cold_file(b"hello");
    let completed = prepare(&mut mount, number, 5).execute(&mut Visible(&mut mount));
    let before = mount.inode(number).unwrap();
    assert_eq!(
        mount.finish_inode_read(&completed, 4).unwrap_err(),
        Ext4Error::buffer_too_small(4, 5)
    );
    let after = mount.inode(number).unwrap();
    assert_eq!(before, after);
    assert_completed(&mut mount, &completed, b"hello");
}

#[test]
fn foreign_mount_rejects_file_visibility_and_publication_without_consuming_bytes() {
    let (mut mount, number) = cold_file(b"hello");
    let mut foreign = mounted_filesystem();
    let completed = prepare(&mut mount, number, 5).execute(&mut Visible(&mut foreign));
    assert_eq!(
        mount.finish_inode_read(&completed, 5).unwrap_err().kind(),
        Ext4ErrorKind::InvalidInput
    );
    let completed = prepare(&mut mount, number, 5).execute(&mut Visible(&mut mount));
    assert_eq!(
        foreign.finish_inode_read(&completed, 5).unwrap_err().kind(),
        Ext4ErrorKind::InvalidInput
    );
    assert_completed(&mut mount, &completed, b"hello");
}

fn cold_file(bytes: &[u8]) -> (TestMount, InodeNumber) {
    let mut mount = mounted_filesystem();
    let number = create_file(&mut mount);
    mount.use_shared_device_cache().unwrap();
    mount.write_inode(number, 0, bytes).unwrap();
    make_cold(&mut mount);
    (mount, number)
}

fn fragmented_file() -> (TestMount, InodeNumber, Vec<u8>) {
    let mut mount = mounted_filesystem();
    let number = create_file(&mut mount);
    mount.use_shared_device_cache().unwrap();
    let block_size = mount.filesystem.block_size();
    let mut expected = alloc::vec![0; 17 * block_size];
    for index in 0..9 {
        let start = index * 2 * block_size;
        let block = &mut expected[start..start + block_size];
        block.fill(index as u8 + 1);
        mount.write_inode(number, start as u64, block).unwrap();
    }
    let mut inode = mount
        .filesystem
        .get_inode_by_num(&mut mount.device, number)
        .unwrap();
    assert!(
        matches!(
            ExtentTree::with_filesystem(&mut inode, &mount.filesystem, number)
                .load_root_from_inode()
                .unwrap(),
            ExtentNode::Index { .. }
        ),
        "fixture must have actual external extent nodes"
    );
    make_cold(&mut mount);
    (mount, number, expected)
}

fn make_cold(mount: &mut TestMount) {
    mount.sync().unwrap();
    mount.device.flush().unwrap();
    mount.filesystem.datablock_cache.clear();
}

fn prepare(
    mount: &mut TestMount,
    number: InodeNumber,
    length: usize,
) -> PreparedInodeRead<MemoryDevice> {
    match mount.prepare_inode_read(number, 0, length).unwrap() {
        InodeReadPreparation::Read(prepared) => prepared,
        phase => panic!("expected a cached-inode read owner, got {phase:?}"),
    }
}

fn assert_completed(mount: &mut TestMount, completed: &CompletedInodeRead, expected: &[u8]) {
    let mut output = alloc::vec![0xa5; expected.len() + 3];
    let validated = mount
        .finish_inode_read(completed, output.len())
        .unwrap()
        .unwrap();
    assert_eq!(validated.copy_to(&mut output), Ok(expected.len()));
    assert_eq!(&output[..expected.len()], expected);
    assert_eq!(&output[expected.len()..], &[0xa5; 3]);
}

struct Visible<'a>(&'a mut TestMount);

impl InodeReadCache for Visible<'_> {
    fn visible(&mut self, request: &InodeBlockRequest) -> Ext4Result<Option<Arc<Vec<u8>>>> {
        self.0.inode_read_block_image(request)
    }

    fn visible_data(
        &mut self,
        request: &InodeDataRequest,
    ) -> Ext4Result<Vec<Option<Arc<Vec<u8>>>>> {
        DATA_QUERIES.set(DATA_QUERIES.get() + 1);
        self.0.inode_read_data_images(request)
    }
}

struct FailRead;

impl FailRead {
    fn new() -> Self {
        assert!(!FAIL_READ.replace(true));
        Self
    }
}

impl Drop for FailRead {
    fn drop(&mut self) {
        FAIL_READ.set(false);
    }
}

pub(super) fn observe_read() -> Ext4Result<()> {
    READS.set(READS.get() + 1);
    if FAIL_READ.get() {
        Err(Ext4Error::io())
    } else {
        Ok(())
    }
}
