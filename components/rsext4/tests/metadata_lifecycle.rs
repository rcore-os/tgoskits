//! Metadata lifecycle tests for timestamps, project IDs, and deletion time.
//!
//! These tests focus on the higher-level metadata contract: when timestamps
//! advance, when `i_dtime` changes, and how inheritance behaves on new inodes.

use std::cell::Cell;

use rsext4::{
    bmalloc::AbsoluteBN,
    disknode::Ext4Inode,
    error::{Ext4Error, Ext4Result},
    superblock::Ext4Superblock,
    *,
};

const INODE_SIZE: u16 = DEFAULT_INODE_SIZE as u16;

/// In-memory block device with a monotonically increasing clock.
struct TimedBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    now: Cell<i64>,
}

impl TimedBlockDevice {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            block_size: BLOCK_SIZE as u32,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for TimedBlockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
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

    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
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

    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

/// Creates a freshly formatted and mounted filesystem for each metadata scenario.
fn setup_fs() -> (Jbd2Dev<TimedBlockDevice>, Ext4FileSystem) {
    let device = TimedBlockDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let fs = mount(&mut jbd2_dev).expect("mount failed");
    (jbd2_dev, fs)
}

/// Resolves a path and returns the current inode snapshot for assertions.
fn lookup_inode(
    dev: &mut Jbd2Dev<TimedBlockDevice>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> Ext4Inode {
    find_file(fs, dev, path).expect("inode not found")
}

#[test]
fn test_create_delete_and_reallocate_inode_updates_dtime() {
    let (mut dev, mut fs) = setup_fs();

    // Test idea: track one inode across create, delete, and allocator reuse, and
    // ensure `i_dtime` follows the lifecycle while creation timestamps stay initialized.
    mkfile(&mut dev, &mut fs, "/meta/file", Some(b"hello"), None).expect("mkfile failed");
    mkdir(&mut dev, &mut fs, "/meta/dir").expect("mkdir failed");
    create_symbol_link(&mut dev, &mut fs, "/meta/file", "/meta/link").expect("symlink failed");

    let file_inode = lookup_inode(&mut dev, &mut fs, "/meta/file");
    assert_eq!(file_inode.i_dtime, 0);
    assert_eq!(
        file_inode.atime_ts(INODE_SIZE).sec,
        file_inode.mtime_ts(INODE_SIZE).sec
    );
    assert_eq!(
        file_inode.atime_ts(INODE_SIZE).sec,
        file_inode.ctime_ts(INODE_SIZE).sec
    );
    assert_eq!(
        file_inode.atime_ts(INODE_SIZE).sec,
        file_inode.crtime_ts(INODE_SIZE).unwrap().sec
    );

    let dir_inode = lookup_inode(&mut dev, &mut fs, "/meta/dir");
    assert_eq!(dir_inode.i_dtime, 0);
    assert!(dir_inode.crtime_ts(INODE_SIZE).is_some());

    let link_inode = lookup_inode(&mut dev, &mut fs, "/meta/link");
    assert_eq!(link_inode.i_dtime, 0);
    assert!(link_inode.crtime_ts(INODE_SIZE).is_some());

    let file = open(&mut dev, &mut fs, "/meta/file", false).expect("open failed");
    let deleted_ino = file.inode_num;
    delete_file(&mut fs, &mut dev, "/meta/file").expect("delete failed");

    let deleted_inode = fs
        .get_inode_by_num(&mut dev, deleted_ino)
        .expect("deleted inode load failed");
    assert!(deleted_inode.i_dtime > 0);
    assert_ne!(deleted_inode.i_dtime, u32::MAX);

    let mut reused = None;
    for _ in 0..64 {
        let ino = fs.alloc_inode(&mut dev).expect("alloc_inode failed");
        if ino == deleted_ino {
            reused = Some(ino);
            break;
        }
    }
    assert_eq!(reused, Some(deleted_ino));

    let reused_inode = fs
        .get_inode_by_num(&mut dev, deleted_ino)
        .expect("reused inode load failed");
    assert_eq!(reused_inode.i_dtime, 0);
}

#[test]
fn test_read_write_truncate_and_noatime_update_expected_timestamps() {
    let (mut dev, mut fs) = setup_fs();

    // Test idea: exercise read, flag changes, write, and truncate in sequence and
    // verify the exact timestamp fields that should or should not move.
    mkfile(&mut dev, &mut fs, "/rw/file", Some(b"hello"), None).expect("mkfile failed");

    let before = lookup_inode(&mut dev, &mut fs, "/rw/file");
    let before_atime = before.atime_ts(INODE_SIZE);

    let mut file = open(&mut dev, &mut fs, "/rw/file", false).expect("open failed");
    let data = read_at(&mut dev, &mut fs, &mut file, 5).expect("read_at failed");
    assert_eq!(data, b"hello");

    let after_read = lookup_inode(&mut dev, &mut fs, "/rw/file");
    assert!(after_read.atime_ts(INODE_SIZE).sec > before_atime.sec);
    assert_eq!(after_read.i_dtime, 0);

    set_flags(
        &mut dev,
        &mut fs,
        "/rw/file",
        Ext4Inode::EXT4_NOATIME_FL | Ext4Inode::EXT4_INDEX_FL,
    )
    .expect("set_flags failed");
    let after_flags = lookup_inode(&mut dev, &mut fs, "/rw/file");
    assert_ne!(after_flags.i_flags & Ext4Inode::EXT4_NOATIME_FL, 0);
    assert_eq!(after_flags.i_flags & Ext4Inode::EXT4_INDEX_FL, 0);

    let atime_before_noatime_read = after_flags.atime_ts(INODE_SIZE);
    let mut noatime_file = open(&mut dev, &mut fs, "/rw/file", false).expect("open failed");
    read_at(&mut dev, &mut fs, &mut noatime_file, 5).expect("read_at failed");
    let after_noatime_read = lookup_inode(&mut dev, &mut fs, "/rw/file");
    assert_eq!(
        after_noatime_read.atime_ts(INODE_SIZE),
        atime_before_noatime_read
    );

    chmod(&mut dev, &mut fs, "/rw/file", 0o6755).expect("chmod failed");
    let before_write = lookup_inode(&mut dev, &mut fs, "/rw/file");
    write_file(&mut dev, &mut fs, "/rw/file", 0, b"HELLO").expect("write failed");
    let after_write = lookup_inode(&mut dev, &mut fs, "/rw/file");
    assert!(after_write.mtime_ts(INODE_SIZE).sec > before_write.mtime_ts(INODE_SIZE).sec);
    assert!(after_write.ctime_ts(INODE_SIZE).sec > before_write.ctime_ts(INODE_SIZE).sec);
    assert_eq!(
        after_write.i_mode & (Ext4Inode::S_ISUID | Ext4Inode::S_ISGID),
        0
    );
    assert_eq!(after_write.i_dtime, 0);

    chmod(&mut dev, &mut fs, "/rw/file", 0o6755).expect("chmod failed");
    let before_truncate = lookup_inode(&mut dev, &mut fs, "/rw/file");
    truncate(&mut dev, &mut fs, "/rw/file", 2).expect("truncate failed");
    let after_truncate = lookup_inode(&mut dev, &mut fs, "/rw/file");
    assert!(after_truncate.mtime_ts(INODE_SIZE).sec > before_truncate.mtime_ts(INODE_SIZE).sec);
    assert!(after_truncate.ctime_ts(INODE_SIZE).sec > before_truncate.ctime_ts(INODE_SIZE).sec);
    assert_eq!(
        after_truncate.i_mode & (Ext4Inode::S_ISUID | Ext4Inode::S_ISGID),
        0
    );
    assert_eq!(after_truncate.i_dtime, 0);
}

#[test]
fn test_metadata_mutators_and_project_inheritance() {
    let (mut dev, mut fs) = setup_fs();

    // Test idea: validate chmod/chown/utimens side effects first, then enable the
    // project feature and confirm inherited project metadata on a new child inode.
    mkfile(&mut dev, &mut fs, "/ops/file", Some(b"abcdef"), None).expect("mkfile failed");

    let before = lookup_inode(&mut dev, &mut fs, "/ops/file");
    chmod(&mut dev, &mut fs, "/ops/file", 0o6755).expect("chmod failed");
    let after_chmod = lookup_inode(&mut dev, &mut fs, "/ops/file");
    assert_eq!(after_chmod.i_mode & Ext4Inode::S_IFMT, Ext4Inode::S_IFREG);
    assert_eq!(after_chmod.permissions(), 0o6755);
    assert!(after_chmod.ctime_ts(INODE_SIZE).sec > before.ctime_ts(INODE_SIZE).sec);

    chown(
        &mut dev,
        &mut fs,
        "/ops/file",
        Some(0x1234_5678),
        Some(0x9abc_def0),
    )
    .expect("chown failed");
    let after_chown = lookup_inode(&mut dev, &mut fs, "/ops/file");
    assert_eq!(after_chown.uid(), 0x1234_5678);
    assert_eq!(after_chown.gid(), 0x9abc_def0);
    assert_eq!(
        after_chown.i_mode & (Ext4Inode::S_ISUID | Ext4Inode::S_ISGID),
        0
    );

    let preserved_mtime = after_chown.mtime_ts(INODE_SIZE);
    let desired_atime = Ext4Timestamp::new(12_345, 678_900_000);
    utimens(
        &mut dev,
        &mut fs,
        "/ops/file",
        Ext4TimeSpec::Set(desired_atime),
        Ext4TimeSpec::Omit,
    )
    .expect("utimens failed");
    let after_utimens = lookup_inode(&mut dev, &mut fs, "/ops/file");
    assert_eq!(after_utimens.atime_ts(INODE_SIZE), desired_atime);
    assert_eq!(after_utimens.mtime_ts(INODE_SIZE), preserved_mtime);
    assert!(after_utimens.ctime_ts(INODE_SIZE).sec > after_chown.ctime_ts(INODE_SIZE).sec);
    assert_eq!(after_utimens.i_dtime, 0);

    assert_eq!(
        set_project(&mut dev, &mut fs, "/ops/file", 7),
        Err(Ext4Error::unsupported())
    );

    fs.superblock.s_feature_ro_compat |= Ext4Superblock::EXT4_FEATURE_RO_COMPAT_PROJECT;
    mkdir(&mut dev, &mut fs, "/projects").expect("mkdir failed");
    set_project(&mut dev, &mut fs, "/projects", 42).expect("set_project failed");
    set_flags(
        &mut dev,
        &mut fs,
        "/projects",
        Ext4Inode::EXT4_PROJINHERIT_FL,
    )
    .expect("set_flags failed");
    mkfile(&mut dev, &mut fs, "/projects/child", None, None).expect("mkfile failed");
    let child = lookup_inode(&mut dev, &mut fs, "/projects/child");
    assert_eq!(child.i_projid, 42);
}

#[test]
fn test_parent_directory_timestamps_follow_entry_changes() {
    let (mut dev, mut fs) = setup_fs();

    // Test idea: perform all directory-entry-changing operations and verify that
    // the parent directories observe the expected mtime/ctime advancement.
    mkdir(&mut dev, &mut fs, "/parent").expect("mkdir parent failed");
    mkdir(&mut dev, &mut fs, "/other").expect("mkdir other failed");

    let parent_before_create = lookup_inode(&mut dev, &mut fs, "/parent");
    mkfile(&mut dev, &mut fs, "/parent/file", None, None).expect("mkfile failed");
    let parent_after_create = lookup_inode(&mut dev, &mut fs, "/parent");
    assert!(
        parent_after_create.mtime_ts(INODE_SIZE).sec
            > parent_before_create.mtime_ts(INODE_SIZE).sec
    );
    assert!(
        parent_after_create.ctime_ts(INODE_SIZE).sec
            > parent_before_create.ctime_ts(INODE_SIZE).sec
    );

    let parent_before_link = parent_after_create;
    link(&mut fs, &mut dev, "/parent/file.link", "/parent/file").expect("link failed");
    let parent_after_link = lookup_inode(&mut dev, &mut fs, "/parent");
    assert!(
        parent_after_link.mtime_ts(INODE_SIZE).sec > parent_before_link.mtime_ts(INODE_SIZE).sec
    );

    let parent_before_unlink = parent_after_link;
    unlink(&mut fs, &mut dev, "/parent/file.link").expect("unlink failed");
    let parent_after_unlink = lookup_inode(&mut dev, &mut fs, "/parent");
    assert!(
        parent_after_unlink.mtime_ts(INODE_SIZE).sec
            > parent_before_unlink.mtime_ts(INODE_SIZE).sec
    );

    let parent_before_rename = parent_after_unlink;
    rename(&mut dev, &mut fs, "/parent/file", "/parent/file2").expect("rename failed");
    let parent_after_rename = lookup_inode(&mut dev, &mut fs, "/parent");
    assert!(
        parent_after_rename.mtime_ts(INODE_SIZE).sec
            > parent_before_rename.mtime_ts(INODE_SIZE).sec
    );

    let old_parent_before_move = parent_after_rename;
    let new_parent_before_move = lookup_inode(&mut dev, &mut fs, "/other");
    mv(&mut fs, &mut dev, "/parent/file2", "/other/file2").expect("mv failed");
    let old_parent_after_move = lookup_inode(&mut dev, &mut fs, "/parent");
    let new_parent_after_move = lookup_inode(&mut dev, &mut fs, "/other");
    assert!(
        old_parent_after_move.mtime_ts(INODE_SIZE).sec
            > old_parent_before_move.mtime_ts(INODE_SIZE).sec
    );
    assert!(
        new_parent_after_move.mtime_ts(INODE_SIZE).sec
            > new_parent_before_move.mtime_ts(INODE_SIZE).sec
    );
}
