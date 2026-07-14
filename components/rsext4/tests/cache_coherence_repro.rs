//! Deterministic directory cache-coherence regressions.

use std::cell::Cell;

use rsext4::{
    bmalloc::AbsoluteBN,
    dir::{get_inode_with_num, insert_dir_entry},
    disknode::Ext4Inode,
    entries::Ext4DirEntry2,
    error::{Ext4Error, Ext4Result},
    file::{read_inode_data_into, truncate_inode, write_inode_data},
    *,
};

struct MockBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    now: Cell<i64>,
}

impl MockBlockDevice {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            block_size: BLOCK_SIZE as u32,
            now: Cell::new(1_700_000_000),
        }
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
