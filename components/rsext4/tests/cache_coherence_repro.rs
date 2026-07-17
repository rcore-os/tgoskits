//! Deterministic directory cache-coherence regressions.

use std::{
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use ax_kspin_test_runtime as _;
use rsext4::{
    bmalloc::{AbsoluteBN, InodeNumber},
    dir::{get_inode_with_num, insert_dir_entry},
    disknode::{Ext4Extent, Ext4Inode},
    entries::Ext4DirEntry2,
    error::{Ext4Error, Ext4Result},
    extents_tree::ExtentTree,
    file::{read_inode_data_into, truncate_inode, write_inode_data},
    *,
};

struct MockBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    now: Cell<i64>,
    io_stats: Arc<MockIoStats>,
}

#[derive(Default)]
struct MockIoStats {
    write_calls: AtomicUsize,
    largest_write_bytes: AtomicUsize,
}

impl MockIoStats {
    fn reset(&self) {
        self.write_calls.store(0, Ordering::Relaxed);
        self.largest_write_bytes.store(0, Ordering::Relaxed);
    }

    fn record_write(&self, bytes: usize) {
        self.write_calls.fetch_add(1, Ordering::Relaxed);
        self.largest_write_bytes.fetch_max(bytes, Ordering::Relaxed);
    }

    fn largest_write_bytes(&self) -> usize {
        self.largest_write_bytes.load(Ordering::Relaxed)
    }
}

impl MockBlockDevice {
    fn new(size: usize) -> Self {
        Self::with_stats(size).0
    }

    fn with_stats(size: usize) -> (Self, Arc<MockIoStats>) {
        let io_stats = Arc::new(MockIoStats::default());
        (
            Self {
                data: vec![0; size],
                block_size: BLOCK_SIZE as u32,
                now: Cell::new(1_700_000_000),
                io_stats: io_stats.clone(),
            },
            io_stats,
        )
    }
}

impl BlockDevice for MockBlockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                self.total_blocks(),
            ));
        }
        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        self.io_stats.record_write(buffer.len());
        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                self.total_blocks(),
            ));
        }
        self.data[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn open(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn total_blocks(&self) -> u64 {
        (self.data.len() / self.block_size as usize) as u64
    }

    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn setup() -> (Jbd2Dev<MockBlockDevice>, Ext4FileSystem) {
    let device = MockBlockDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let fs = mount(&mut jbd2_dev).expect("mount failed");
    (jbd2_dev, fs)
}

fn setup_with_io_stats() -> (Jbd2Dev<MockBlockDevice>, Ext4FileSystem, Arc<MockIoStats>) {
    let (device, io_stats) = MockBlockDevice::with_stats(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let fs = mount(&mut jbd2_dev).expect("mount failed");
    (jbd2_dev, fs, io_stats)
}

fn install_unwritten_extent(
    dev: &mut Jbd2Dev<MockBlockDevice>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> (InodeNumber, AbsoluteBN) {
    mkfile(dev, fs, path, None, None).expect("create unwritten extent file");
    let (ino, mut inode) = get_inode_with_num(fs, dev, path)
        .expect("lookup unwritten extent file")
        .expect("unwritten extent file exists");
    let physical = fs.alloc_block(dev).expect("allocate unwritten data block");
    let unwritten_len = Ext4Extent::encode_len(1, true).expect("encode unwritten extent");
    {
        let mut tree = ExtentTree::with_checksum(&mut inode, &fs.superblock, ino);
        let mut extent = Ext4Extent::new(0, physical.raw(), 1);
        extent.ee_len = unwritten_len;
        tree.insert_extent(fs, extent, dev)
            .expect("insert unwritten extent");
    }
    inode.i_size_lo = BLOCK_SIZE as u32;
    inode.i_blocks_lo = (BLOCK_SIZE / 512) as u32;
    fs.modify_inode(dev, ino, |stored| *stored = inode)
        .expect("persist unwritten extent inode");
    (ino, physical)
}

fn long_name(prefix: &str) -> String {
    format!("{prefix}{}", "x".repeat(248))
}

#[test]
fn directory_growth_preserves_parent_link_count() {
    let (mut dev, mut fs) = setup();
    mkdir(&mut dev, &mut fs, "/parent").expect("mkdir /parent");

    for index in 0..15 {
        let path = format!("/parent/{}", long_name(&format!("f{index:02}")));
        mkfile(&mut dev, &mut fs, &path, Some(b"x"), None)
            .unwrap_or_else(|error| panic!("mkfile {path} failed: {error:?}"));
    }

    let (_, before) = get_inode_with_num(&mut fs, &mut dev, "/parent")
        .expect("lookup parent")
        .expect("parent exists");
    assert_eq!(before.size(), BLOCK_SIZE as u64);

    let child = long_name("dir");
    mkdir(&mut dev, &mut fs, &format!("/parent/{child}"))
        .expect("mkdir that expands parent directory");

    let (_, after) = get_inode_with_num(&mut fs, &mut dev, "/parent")
        .expect("lookup expanded parent")
        .expect("expanded parent exists");
    assert_eq!(after.size(), (2 * BLOCK_SIZE) as u64);
    assert_eq!(after.i_links_count, before.i_links_count + 1);
}

#[test]
fn insertion_clears_stale_directory_index_flag() {
    let (mut dev, mut fs) = setup();
    mkdir(&mut dev, &mut fs, "/indexed").expect("mkdir /indexed");
    mkfile(&mut dev, &mut fs, "/target", Some(b"x"), None).expect("mkfile /target");

    let (parent_ino, mut parent_inode) = get_inode_with_num(&mut fs, &mut dev, "/indexed")
        .expect("lookup indexed directory")
        .expect("indexed directory exists");
    let (target_ino, _) = get_inode_with_num(&mut fs, &mut dev, "/target")
        .expect("lookup target")
        .expect("target exists");

    parent_inode.i_flags |= Ext4Inode::EXT4_INDEX_FL;
    fs.modify_inode(&mut dev, parent_ino, |inode| {
        inode.i_flags |= Ext4Inode::EXT4_INDEX_FL;
    })
    .expect("mark directory index stale");

    insert_dir_entry(
        &mut fs,
        &mut dev,
        parent_ino,
        &mut parent_inode,
        target_ino,
        "entry",
        Ext4DirEntry2::EXT4_FT_REG_FILE,
    )
    .expect("insert entry into indexed directory");

    let (_, updated_parent) = get_inode_with_num(&mut fs, &mut dev, "/indexed")
        .expect("lookup updated directory")
        .expect("updated directory exists");
    assert_eq!(updated_parent.i_flags & Ext4Inode::EXT4_INDEX_FL, 0);

    let (entry_ino, _) = get_inode_with_num(&mut fs, &mut dev, "/indexed/entry")
        .expect("lookup inserted entry")
        .expect("inserted entry exists");
    assert_eq!(entry_ino, target_ino);
}

#[test]
fn truncate_rewrite_reread_is_coherent() {
    let (mut dev, mut fs) = setup();
    let path = "/libfoo.so.3";

    // Phase 1: write initial data
    mkfile(&mut dev, &mut fs, path, Some(b"old data - v1.0"), None).expect("create libfoo");

    // Phase 2: truncate to 0 (simulates apk upgrading the .so)
    let (ino, _) = get_inode_with_num(&mut fs, &mut dev, path)
        .expect("lookup")
        .expect("exists");
    truncate_inode(&mut dev, &mut fs, ino, 0).expect("truncate to 0");

    // Phase 3: write new data (simulates apk installing new version)
    let new_content: Vec<u8> = (0..8192u16).flat_map(|i| i.to_le_bytes()).collect();
    write_inode_data(&mut dev, &mut fs, ino, 0, &new_content).expect("write new data");

    // Phase 4: read back and verify — must see the new data, not old
    let mut buf = vec![0u8; new_content.len()];
    let n = read_inode_data_into(&mut dev, &mut fs, ino, 0, &mut buf).expect("read back");
    assert_eq!(n, new_content.len(), "read length");
    assert_eq!(
        buf, new_content,
        "data mismatch — truncate+rewrite not visible to reader"
    );
}

#[test]
fn extending_an_extent_file_keeps_the_new_range_sparse() {
    let (mut dev, mut fs) = setup();
    let path = "/sparse-growth.bin";
    mkfile(&mut dev, &mut fs, path, None, None).expect("create sparse-growth file");
    let (ino, before_inode) = get_inode_with_num(&mut fs, &mut dev, path)
        .expect("lookup sparse-growth file")
        .expect("sparse-growth file exists");
    let free_blocks_before = fs.statfs().free_blocks;

    let new_len = 512 * BLOCK_SIZE as u64;
    truncate_inode(&mut dev, &mut fs, ino, new_len).expect("extend sparse file");

    let after_inode = fs
        .get_inode_by_num(&mut dev, ino)
        .expect("reload sparse inode");
    assert_eq!(after_inode.size(), new_len);
    assert_eq!(
        after_inode.blocks_count(),
        before_inode.blocks_count(),
        "extending i_size must not allocate data blocks"
    );
    assert_eq!(
        fs.statfs().free_blocks,
        free_blocks_before,
        "an unwritten extension must remain a hole"
    );

    let mut tail = [0xa5; BLOCK_SIZE];
    let read = read_inode_data_into(
        &mut dev,
        &mut fs,
        ino,
        new_len - BLOCK_SIZE as u64,
        &mut tail,
    )
    .expect("read sparse tail");
    assert_eq!(read, BLOCK_SIZE);
    assert_eq!(tail, [0; BLOCK_SIZE]);
}

#[test]
fn whole_file_read_preserves_holes_before_written_sparse_data() {
    let (mut dev, mut fs) = setup();
    let path = "/sparse-whole-file-read.bin";
    mkfile(&mut dev, &mut fs, path, None, None).expect("create sparse read file");
    let (ino, _) = get_inode_with_num(&mut fs, &mut dev, path)
        .expect("lookup sparse read file")
        .expect("sparse read file exists");
    let file_len = 3 * BLOCK_SIZE as u64;
    truncate_inode(&mut dev, &mut fs, ino, file_len).expect("extend sparse read file");
    write_inode_data(
        &mut dev,
        &mut fs,
        ino,
        2 * BLOCK_SIZE as u64,
        &[0x5a; BLOCK_SIZE],
    )
    .expect("write sparse tail block");

    let contents = read_file(&mut dev, &mut fs, path).expect("read complete sparse file");

    assert_eq!(contents.len(), file_len as usize);
    assert_eq!(contents[..2 * BLOCK_SIZE], [0; 2 * BLOCK_SIZE]);
    assert_eq!(contents[2 * BLOCK_SIZE..], [0x5a; BLOCK_SIZE]);
}

#[test]
fn writing_a_full_sparse_range_uses_multi_block_data_io() {
    let (mut dev, mut fs, io_stats) = setup_with_io_stats();
    let path = "/sparse-writeback.bin";
    mkfile(&mut dev, &mut fs, path, None, None).expect("create sparse-writeback file");
    let (ino, _) = get_inode_with_num(&mut fs, &mut dev, path)
        .expect("lookup sparse-writeback file")
        .expect("sparse-writeback file exists");
    let data = vec![0x5a; 256 * BLOCK_SIZE];
    truncate_inode(&mut dev, &mut fs, ino, data.len() as u64).expect("extend sparse file");
    io_stats.reset();

    write_inode_data(&mut dev, &mut fs, ino, 0, &data).expect("write full sparse range");

    assert!(
        io_stats.largest_write_bytes() >= data.len(),
        "full-page writeback into one hole must reach the device as a multi-block run; largest \
         write was {} bytes",
        io_stats.largest_write_bytes()
    );
}

#[test]
fn writing_an_unwritten_extent_does_not_allocate_an_overlapping_mapping() {
    let (mut dev, mut fs) = setup();
    let (ino, physical) = install_unwritten_extent(&mut dev, &mut fs, "/unwritten-extent.bin");
    let free_blocks_before = fs.statfs().free_blocks;

    let error = write_inode_data(&mut dev, &mut fs, ino, 0, &[0x5a; BLOCK_SIZE])
        .expect_err("unwritten extent conversion is not implemented yet");

    assert_eq!(error.code, Errno::EOPNOTSUPP);
    assert_eq!(
        fs.statfs().free_blocks,
        free_blocks_before,
        "a rejected unwritten-extent write must not allocate overlapping blocks"
    );
    let mut persisted = fs
        .get_inode_by_num(&mut dev, ino)
        .expect("reload unwritten extent inode");
    let extent = ExtentTree::new(&mut persisted)
        .find_extent(&mut dev, 0)
        .expect("read extent tree")
        .expect("unwritten extent remains mapped");
    assert!(extent.is_unwritten());
    assert_eq!(extent.start_block(), physical.raw());
}

#[test]
fn truncating_an_unwritten_extent_releases_its_physical_blocks() {
    let (mut dev, mut fs) = setup();
    let free_blocks_before = fs.statfs().free_blocks;
    let (ino, _) = install_unwritten_extent(&mut dev, &mut fs, "/truncate-unwritten.bin");
    assert_eq!(fs.statfs().free_blocks + 1, free_blocks_before);

    truncate_inode(&mut dev, &mut fs, ino, 0).expect("truncate unwritten extent");

    assert_eq!(fs.statfs().free_blocks, free_blocks_before);
    let mut persisted = fs
        .get_inode_by_num(&mut dev, ino)
        .expect("reload truncated inode");
    assert_eq!(persisted.size(), 0);
    assert_eq!(persisted.blocks_count(), 0);
    assert!(
        ExtentTree::new(&mut persisted)
            .find_extent(&mut dev, 0)
            .expect("read truncated extent tree")
            .is_none()
    );
}

#[test]
fn shrink_then_sparse_reextend_does_not_expose_truncated_tail() {
    let (mut dev, mut fs) = setup();
    let path = "/truncate-tail.bin";
    mkfile(&mut dev, &mut fs, path, Some(&[0x5a; BLOCK_SIZE]), None)
        .expect("create truncate-tail file");
    let (ino, _) = get_inode_with_num(&mut fs, &mut dev, path)
        .expect("lookup truncate-tail file")
        .expect("truncate-tail file exists");

    truncate_inode(&mut dev, &mut fs, ino, 1).expect("shrink inside data block");
    truncate_inode(&mut dev, &mut fs, ino, BLOCK_SIZE as u64)
        .expect("sparse re-extend inside retained block");

    let mut contents = [0xa5; BLOCK_SIZE];
    let read = read_inode_data_into(&mut dev, &mut fs, ino, 0, &mut contents)
        .expect("read re-extended file");
    assert_eq!(read, BLOCK_SIZE);
    assert_eq!(contents[0], 0x5a);
    assert_eq!(contents[1..], [0; BLOCK_SIZE - 1]);
}
