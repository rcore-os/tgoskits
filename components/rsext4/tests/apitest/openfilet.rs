//! `open()` semantic tests for the current ext4 API surface.
//!
//! The current `open()` API is path-oriented and only carries a boolean
//! "create if missing" switch, so these tests verify that observable contract
//! directly instead of Linux-style flag parsing.

use std::cell::Cell;

use rsext4::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
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
            block_size: 1024u32 << rsext4::LOG_BLOCK_SIZE,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for MockBlockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: DevBN, _count: u32) -> Ext4Result<()> {
        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], block_id: DevBN, _count: u32) -> Ext4Result<()> {
        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
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

    fn dev_block_size(&self) -> u32 {
        self.block_size
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn new_fs() -> (Jbd2Dev<MockBlockDevice>, Ext4FileSystem) {
    let device = MockBlockDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let fs = mount(&mut jbd2_dev).expect("mount failed");
    (jbd2_dev, fs)
}

fn assert_errno<T>(res: Ext4Result<T>, code: Errno) {
    match res {
        Ok(_) => panic!("expected errno {code:?}, got Ok"),
        Err(e) => assert_eq!(e.code, code),
    }
}

#[test]
fn test_open_missing_without_create_fails() {
    let (mut dev, mut fs) = new_fs();

    assert_errno(
        open(&mut dev, &mut fs, "/no/such/file", false),
        Errno::ENOENT,
    );

    umount(fs, &mut dev).unwrap();
}

#[test]
fn test_open_create_auto_creates_missing_parents() {
    let (mut dev, mut fs) = new_fs();

    let created = open(&mut dev, &mut fs, "/missing_parent/new.txt", true).unwrap();
    assert!(created.inode.is_file());
    assert!(
        find_file(&mut fs, &mut dev, "/missing_parent")
            .unwrap()
            .is_dir()
    );

    umount(fs, &mut dev).unwrap();
}

#[test]
fn test_open_create_existing_keeps_file_contents() {
    let (mut dev, mut fs) = new_fs();

    let created = open(&mut dev, &mut fs, "/new.txt", true).unwrap();
    assert!(created.inode.is_file());

    mkfile(&mut dev, &mut fs, "/keep.txt", Some(b"abcdef"), None).unwrap();
    let reopened = open(&mut dev, &mut fs, "/keep.txt", true).unwrap();
    assert!(reopened.inode.is_file());

    let bytes = read_file(&mut dev, &mut fs, "/keep.txt").unwrap();
    assert_eq!(bytes, b"abcdef");

    umount(fs, &mut dev).unwrap();
}

#[test]
fn test_open_root_directory_and_regular_file() {
    let (mut dev, mut fs) = new_fs();

    mkdir(&mut dev, &mut fs, "/d").unwrap();
    mkfile(&mut dev, &mut fs, "/f", Some(b"x"), None).unwrap();

    let root = open(&mut dev, &mut fs, "/", false).unwrap();
    assert!(root.inode.is_dir());

    let dir = open(&mut dev, &mut fs, "/d", false).unwrap();
    assert!(dir.inode.is_dir());

    let file = open(&mut dev, &mut fs, "/f", false).unwrap();
    assert!(file.inode.is_file());

    umount(fs, &mut dev).unwrap();
}

#[test]
fn test_open_symlink_returns_link_inode() {
    let (mut dev, mut fs) = new_fs();

    mkfile(&mut dev, &mut fs, "/target", Some(b"payload"), None).unwrap();
    create_symbol_link(&mut dev, &mut fs, "/target", "/link").unwrap();

    let file = open(&mut dev, &mut fs, "/link", false).unwrap();
    assert!(file.inode.is_symlink());

    umount(fs, &mut dev).unwrap();
}
