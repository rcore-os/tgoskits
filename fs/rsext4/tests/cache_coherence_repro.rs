//! Deterministic directory cache-coherence regressions.

use std::cell::Cell;

use rsext4::{
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

impl BlockIo for MockBlockDevice {
    fn read(&mut self, buffer: &mut [u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                self.geometry().block_count,
            ));
        }
        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                self.geometry().block_count,
            ));
        }
        self.data[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn geometry(&self) -> rsext4::DeviceGeometry {
        rsext4::DeviceGeometry::new(self.block_size, {
            (self.data.len() / self.block_size as usize) as u64
        })
    }

    fn capabilities(&self) -> rsext4::DeviceCapabilities {
        rsext4::DeviceCapabilities {
            read_only: { false },

            flush: true,

            ..rsext4::DeviceCapabilities::default()
        }
    }

    fn flush(&mut self) -> rsext4::Ext4Result<()> {
        Ok(())
    }
}

impl rsext4::Clock for MockBlockDevice {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn setup() -> (Jbd2Dev<MockBlockDevice>, Ext4FileSystem) {
    let device = MockBlockDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
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
    assert_eq!(after.size(), (3 * BLOCK_SIZE) as u64);
    assert_ne!(after.i_flags & Ext4Inode::EXT4_INDEX_FL, 0);
    assert_eq!(after.i_links_count, before.i_links_count + 1);
}

#[test]
fn insertion_rejects_a_stale_directory_index() {
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

    let error = insert_dir_entry(
        &mut fs,
        &mut dev,
        parent_ino,
        &mut parent_inode,
        target_ino,
        "entry",
        Ext4DirEntry2::EXT4_FT_REG_FILE,
    )
    .expect_err("a forged index flag must not turn a linear block into an HTree root");
    assert_eq!(error.kind(), rsext4::Ext4ErrorKind::Corrupted);

    let (_, updated_parent) = get_inode_with_num(&mut fs, &mut dev, "/indexed")
        .expect("lookup updated directory")
        .expect("updated directory exists");
    assert_ne!(updated_parent.i_flags & Ext4Inode::EXT4_INDEX_FL, 0);

    assert!(
        get_inode_with_num(&mut fs, &mut dev, "/indexed/entry")
            .expect("lookup rejected entry")
            .is_none()
    );
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
