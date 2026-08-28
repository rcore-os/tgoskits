//! Functional tests for file-level operations.
//!
//! The suite focuses on common file workflows and records a few implementation
//! details that intentionally differ from a fully POSIX-like filesystem.

use std::{cell::Cell, rc::Rc};

use rsext4::{
    error::{Ext4Error, Ext4Result},
    *,
};

/// In-memory block device used by file operation tests.
struct MockBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    fail_on_write: bool,
    fail_on_read: bool,
    fail_after_write_sector: Rc<Cell<Option<u64>>>,
    now: Cell<i64>,
}

impl MockBlockDevice {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            block_size: rsext4::BLOCK_SIZE as u32,
            fail_on_write: false,
            fail_on_read: false,
            fail_after_write_sector: Rc::new(Cell::new(None)),
            now: Cell::new(1_700_000_000),
        }
    }

    fn with_write_failure_handle(size: usize) -> (Self, Rc<Cell<Option<u64>>>) {
        let device = Self::new(size);
        let handle = Rc::clone(&device.fail_after_write_sector);
        (device, handle)
    }
}

impl BlockIo for MockBlockDevice {
    fn read(&mut self, buffer: &mut [u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
        if self.fail_on_read {
            return Err(Ext4Error::io());
        }

        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
        if self.fail_on_write {
            return Err(Ext4Error::io());
        }

        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        self.data[start..end].copy_from_slice(buffer);
        if self.fail_after_write_sector.get() == Some(sector.raw()) {
            self.fail_after_write_sector.set(None);
            Err(Ext4Error::io())
        } else {
            Ok(())
        }
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

#[cfg(test)]
mod file_functional_tests {
    use rsext4::bmalloc::AbsoluteBN;

    use super::*;

    fn write_block_fixture(
        device: &mut Jbd2Dev<MockBlockDevice>,
        block: AbsoluteBN,
        is_metadata: bool,
        operation: impl FnOnce(&mut [u8]),
    ) {
        let mut image = vec![0; BLOCK_SIZE];
        operation(&mut image);
        device
            .write_blocks(&image, block, 1, is_metadata)
            .expect("fixture block write");
    }

    /// Covers the create-read-write loop and documents that a shorter overwrite
    /// updates the prefix without implicitly truncating the file.
    #[test]
    fn test_file_create_and_rw() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/testdir").expect("mkdir failed");

        // Arrange one file with known contents and validate the initial read path.
        let test_data = b"This is test data for file operations.";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/testdir/testfile",
            Some(test_data),
            None,
        )
        .expect("mkfile failed");

        let read_data =
            read_file(&mut jbd2_dev, &mut fs, "/testdir/testfile").expect("read_file failed");
        assert_eq!(read_data, test_data.to_vec());

        // Overwrite the prefix and check that the modified region is visible.
        let new_data = b"Modified data";
        write_file(&mut jbd2_dev, &mut fs, "/testdir/testfile", 0, new_data)
            .expect("write_file failed");

        let modified_data =
            read_file(&mut jbd2_dev, &mut fs, "/testdir/testfile").expect("read_file failed");

        // The current implementation does not auto-truncate when the replacement
        // payload is shorter, so the suffix from the old file remains.
        assert_eq!(
            &modified_data[..new_data.len()],
            new_data,
            "The new prefix should be written correctly",
        );

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    #[test]
    fn failed_file_create_does_not_publish_a_directory_entry() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        let free_blocks_before = fs.superblock.free_blocks_count();
        let free_inodes_before = fs.superblock.s_free_inodes_count;
        let root_number = fs.root_inode;
        let mut root_inode = fs
            .get_inode_by_num(&mut jbd2_dev, root_number)
            .expect("root inode");
        let root_block =
            loopfile::resolve_inode_block(&fs, &mut jbd2_dev, root_number, &mut root_inode, 0)
                .expect("root block lookup failed")
                .expect("root block missing");

        fs.sync_filesystem(&mut jbd2_dev)
            .expect("fixture sync failed");
        jbd2_dev.flush().expect("fixture checkpoint failed");
        jbd2_dev
            .set_journal_use(false)
            .expect("disable journal for direct fault injection");
        fail_after_write_sector.set(Some(root_block.raw()));

        let error = mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/create-fault",
            Some(b"unpublished create"),
            None,
        )
        .expect_err("directory write failure must abort file creation");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        drop(fs);
        let device = jbd2_dev.into_inner();
        let mut remount_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_dev).expect("remount after failed create");
        assert!(
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/create-fault")
                .expect("remounted lookup failed")
                .is_none(),
            "a failed create must not leave a reachable inode"
        );
        assert_eq!(remounted.superblock.free_blocks_count(), free_blocks_before);
        assert_eq!(remounted.superblock.s_free_inodes_count, free_inodes_before);
    }

    #[test]
    fn failed_directory_create_restores_parent_and_allocation_state() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        let free_blocks_before = fs.superblock.free_blocks_count();
        let free_inodes_before = fs.superblock.s_free_inodes_count;
        let used_dirs_before: u32 = fs
            .group_descs
            .iter()
            .map(|descriptor| descriptor.used_dirs_count())
            .sum();
        let root_number = fs.root_inode;
        let mut root_inode = fs
            .get_inode_by_num(&mut jbd2_dev, root_number)
            .expect("root inode");
        let root_links_before = root_inode.i_links_count;
        let root_block =
            loopfile::resolve_inode_block(&fs, &mut jbd2_dev, root_number, &mut root_inode, 0)
                .expect("root block lookup failed")
                .expect("root block missing");

        fs.sync_filesystem(&mut jbd2_dev)
            .expect("fixture sync failed");
        jbd2_dev.flush().expect("fixture checkpoint failed");
        jbd2_dev
            .set_journal_use(false)
            .expect("disable journal for direct fault injection");
        fail_after_write_sector.set(Some(root_block.raw()));

        let error = mkdir(&mut jbd2_dev, &mut fs, "/mkdir-fault")
            .expect_err("directory write failure must abort mkdir");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        drop(fs);
        let device = jbd2_dev.into_inner();
        let mut remount_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_dev).expect("remount after failed mkdir");
        assert!(
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/mkdir-fault")
                .expect("remounted lookup failed")
                .is_none(),
            "a failed mkdir must not leave a reachable directory"
        );
        assert_eq!(remounted.superblock.free_blocks_count(), free_blocks_before);
        assert_eq!(remounted.superblock.s_free_inodes_count, free_inodes_before);
        let root_after = remounted
            .get_inode_by_num(&mut remount_dev, root_number)
            .expect("remounted root inode");
        assert_eq!(root_after.i_links_count, root_links_before);
        let used_dirs_after: u32 = remounted
            .group_descs
            .iter()
            .map(|descriptor| descriptor.used_dirs_count())
            .sum();
        assert_eq!(used_dirs_after, used_dirs_before);
    }

    /// Covers both shrinking and growing a file and requires Linux EOF zeroing.
    #[test]
    fn test_file_truncate() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/truncatetest").expect("mkdir failed");

        let original_data = b"This is a long string that will be truncated";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/truncatetest/truncate_file",
            Some(original_data),
            None,
        )
        .expect("mkfile failed");

        // Shrink the file and verify the visible prefix.
        truncate(&mut jbd2_dev, &mut fs, "/truncatetest/truncate_file", 10)
            .expect("truncate failed");

        let truncated_data = read_file(&mut jbd2_dev, &mut fs, "/truncatetest/truncate_file")
            .expect("read_file failed");
        assert_eq!(truncated_data, Vec::from(&original_data[..10]));

        // Grow the file again. Bytes hidden by the previous shrink must not
        // become visible again.
        truncate(&mut jbd2_dev, &mut fs, "/truncatetest/truncate_file", 20)
            .expect("truncate expand failed");

        let expanded_data = read_file(&mut jbd2_dev, &mut fs, "/truncatetest/truncate_file")
            .expect("read_file failed");

        let mut expected = Vec::from(&original_data[..10]);
        expected.resize(20, 0);
        assert_eq!(expanded_data, expected);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies that rename removes the old path and preserves file contents at
    /// the new path within the same directory.
    #[test]
    fn test_file_rename() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/renametest").expect("mkdir failed");

        let test_data = b"Data for rename test";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/renametest/oldname",
            Some(test_data),
            None,
        )
        .expect("mkfile failed");

        let _ = rename(
            &mut jbd2_dev,
            &mut fs,
            "/renametest/oldname",
            "/renametest/newname",
            RenameOptions::REPLACE,
        )
        .expect("rename failed");

        // The old path must disappear after rename.
        let old_err = read_file(&mut jbd2_dev, &mut fs, "/renametest/oldname")
            .expect_err("old path should not exist");
        assert_eq!(old_err.kind(), Ext4ErrorKind::NotFound);

        // The new path should expose the exact original content.
        let new_data =
            read_file(&mut jbd2_dev, &mut fs, "/renametest/newname").expect("read_file failed");
        assert_eq!(new_data, test_data.to_vec());

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    #[test]
    fn renaming_an_entry_to_itself_is_a_noop() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        let contents = b"same dentry must survive";
        mkfile(&mut jbd2_dev, &mut fs, "/same-name", Some(contents), None)
            .expect("file creation failed");

        let _ = rename(
            &mut jbd2_dev,
            &mut fs,
            "/same-name",
            "/same-name",
            RenameOptions::REPLACE,
        )
        .expect("same-path rename must succeed");

        let data = read_file(&mut jbd2_dev, &mut fs, "/same-name")
            .expect("same-path rename removed the entry");
        assert_eq!(data, contents);
    }

    /// Verifies that a full-block overwrite inside an existing large extent
    /// updates only the requested range and keeps surrounding data intact.
    #[test]
    fn test_large_file_existing_extent_full_block_overwrite() {
        let device = MockBlockDevice::new(128 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        let original_len = 20 * 1024 * 1024;
        let overwrite_offset = 8 * 1024 * 1024;
        let overwrite_len = 4 * 1024 * 1024;
        let original = vec![0x31; original_len];
        let replacement = vec![0x7a; overwrite_len];

        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/large-overwrite.bin",
            Some(&original),
            None,
        )
        .expect("mkfile failed");
        write_file(
            &mut jbd2_dev,
            &mut fs,
            "/large-overwrite.bin",
            overwrite_offset as u64,
            &replacement,
        )
        .expect("write_file failed");

        let data =
            read_file(&mut jbd2_dev, &mut fs, "/large-overwrite.bin").expect("read_file failed");
        assert_eq!(data.len(), original_len);
        assert!(data[..overwrite_offset].iter().all(|&byte| byte == 0x31));
        assert!(
            data[overwrite_offset..overwrite_offset + overwrite_len]
                .iter()
                .all(|&byte| byte == 0x7a)
        );
        assert!(
            data[overwrite_offset + overwrite_len..]
                .iter()
                .all(|&byte| byte == 0x31)
        );

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies POSIX rename-over-existing-file (sed -i / Redis AOF pattern).
    #[test]
    fn test_file_rename_replace_existing() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/tmp").expect("mkdir failed");

        let original = b"OLD CONTENT\n";
        let updated = b"NEW CONTENT\n";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/tmp/original.txt",
            Some(original),
            None,
        )
        .expect("mkfile original failed");
        mkfile(&mut jbd2_dev, &mut fs, "/tmp/temp.txt", Some(updated), None)
            .expect("mkfile temp failed");

        let outcome = rename(
            &mut jbd2_dev,
            &mut fs,
            "/tmp/temp.txt",
            "/tmp/original.txt",
            RenameOptions::REPLACE,
        )
        .expect("rename replace failed");
        let replaced = outcome.replaced.expect("replacement outcome missing");
        assert!(replaced.requires_reap());
        reap_unlinked_inode(&mut fs, &mut jbd2_dev, replaced.inode)
            .expect("replacement target reap failed");

        let data =
            read_file(&mut jbd2_dev, &mut fs, "/tmp/original.txt").expect("read_file failed");
        assert_eq!(data, updated.to_vec());

        let temp_err = read_file(&mut jbd2_dev, &mut fs, "/tmp/temp.txt").expect_err("temp gone");
        assert_eq!(temp_err.kind(), Ext4ErrorKind::NotFound);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Redis 8 AOF: `rename(temp in parent, file in appendonlydir/)` on first start.
    #[test]
    fn test_file_rename_cross_directory() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/aofdir").expect("mkdir failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/temp-rewriteaof.aof",
            Some(b"RDB-BASE-DATA\n"),
            None,
        )
        .expect("mkfile temp failed");

        let _ = rename(
            &mut jbd2_dev,
            &mut fs,
            "/temp-rewriteaof.aof",
            "/aofdir/appendonly.aof.1.base.rdb",
            RenameOptions::REPLACE,
        )
        .expect("cross-dir rename failed");

        let data = read_file(&mut jbd2_dev, &mut fs, "/aofdir/appendonly.aof.1.base.rdb")
            .expect("read dest failed");
        assert_eq!(data, b"RDB-BASE-DATA\n");

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    #[test]
    fn failed_cross_directory_rename_restores_both_names_after_remount() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkdir(&mut jbd2_dev, &mut fs, "/old-parent").expect("old parent mkdir failed");
        mkdir(&mut jbd2_dev, &mut fs, "/new-parent").expect("new parent mkdir failed");
        let payload = b"rename must publish atomically";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/old-parent/source",
            Some(payload),
            None,
        )
        .expect("source creation failed");

        let free_blocks_before = fs.superblock.free_blocks_count();
        let free_inodes_before = fs.superblock.s_free_inodes_count;
        let (source_number, _) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/old-parent/source")
                .expect("source lookup failed")
                .expect("source missing");
        let (old_parent_number, mut old_parent_inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/old-parent")
                .expect("old parent lookup failed")
                .expect("old parent missing");
        let old_parent_block = loopfile::resolve_inode_block(
            &fs,
            &mut jbd2_dev,
            old_parent_number,
            &mut old_parent_inode,
            0,
        )
        .expect("old parent block lookup failed")
        .expect("old parent block missing");

        fs.sync_filesystem(&mut jbd2_dev)
            .expect("fixture sync failed");
        jbd2_dev.flush().expect("fixture checkpoint failed");
        jbd2_dev
            .set_journal_use(false)
            .expect("disable journal for direct fault injection");
        fail_after_write_sector.set(Some(old_parent_block.raw()));

        let error = rename(
            &mut jbd2_dev,
            &mut fs,
            "/old-parent/source",
            "/new-parent/destination",
            RenameOptions::REPLACE,
        )
        .expect_err("old-directory write failure must abort the whole rename");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        drop(fs);
        let device = jbd2_dev.into_inner();
        let mut remount_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_dev).expect("remount after failed rename");
        let (remounted_source, _) =
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/old-parent/source")
                .expect("remounted source lookup failed")
                .expect("failed rename must retain its source name");
        assert_eq!(remounted_source, source_number);
        assert_eq!(
            read_file(&mut remount_dev, &mut remounted, "/old-parent/source")
                .expect("retained source must stay readable"),
            payload
        );
        assert!(
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/new-parent/destination")
                .expect("remounted destination lookup failed")
                .is_none(),
            "failed rename must not publish the destination name"
        );
        assert_eq!(remounted.superblock.free_blocks_count(), free_blocks_before);
        assert_eq!(remounted.superblock.s_free_inodes_count, free_inodes_before);
    }

    #[test]
    fn failed_rename_exchange_restores_both_entries_after_remount() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkdir(&mut jbd2_dev, &mut fs, "/exchange-old").expect("old parent mkdir failed");
        mkdir(&mut jbd2_dev, &mut fs, "/exchange-new").expect("new parent mkdir failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/exchange-old/source",
            Some(b"source payload"),
            None,
        )
        .expect("source creation failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/exchange-new/target",
            Some(b"target payload"),
            None,
        )
        .expect("target creation failed");

        let (source_number, _) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/exchange-old/source")
                .expect("source lookup failed")
                .expect("source missing");
        let (target_number, _) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/exchange-new/target")
                .expect("target lookup failed")
                .expect("target missing");
        let (old_parent_number, mut old_parent_inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/exchange-old")
                .expect("old parent lookup failed")
                .expect("old parent missing");
        let old_parent_block = loopfile::resolve_inode_block(
            &fs,
            &mut jbd2_dev,
            old_parent_number,
            &mut old_parent_inode,
            0,
        )
        .expect("old parent block lookup failed")
        .expect("old parent block missing");

        fs.sync_filesystem(&mut jbd2_dev)
            .expect("fixture sync failed");
        jbd2_dev.flush().expect("fixture checkpoint failed");
        jbd2_dev
            .set_journal_use(false)
            .expect("disable journal for direct fault injection");
        fail_after_write_sector.set(Some(old_parent_block.raw()));

        let error = rename(
            &mut jbd2_dev,
            &mut fs,
            "/exchange-old/source",
            "/exchange-new/target",
            RenameOptions::EXCHANGE,
        )
        .expect_err("second exchange-side write failure must abort both replacements");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        drop(fs);
        let device = jbd2_dev.into_inner();
        let mut remount_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_dev).expect("remount after failed exchange");
        let (source_after, _) =
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/exchange-old/source")
                .expect("remounted source lookup failed")
                .expect("source must remain after failed exchange");
        let (target_after, _) =
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/exchange-new/target")
                .expect("remounted target lookup failed")
                .expect("target must remain after failed exchange");
        assert_eq!(source_after, source_number);
        assert_eq!(target_after, target_number);
        assert_eq!(
            read_file(&mut remount_dev, &mut remounted, "/exchange-old/source")
                .expect("source must stay readable"),
            b"source payload"
        );
        assert_eq!(
            read_file(&mut remount_dev, &mut remounted, "/exchange-new/target")
                .expect("target must stay readable"),
            b"target payload"
        );
    }

    #[test]
    fn failed_replacing_rename_restores_target_links_and_orphan_head() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkdir(&mut jbd2_dev, &mut fs, "/replace-old").expect("old parent mkdir failed");
        mkdir(&mut jbd2_dev, &mut fs, "/replace-new").expect("new parent mkdir failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/replace-old/source",
            Some(b"replacement source"),
            None,
        )
        .expect("source creation failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/replace-new/target",
            Some(b"original target"),
            None,
        )
        .expect("target creation failed");

        let (source_number, _) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/replace-old/source")
                .expect("source lookup failed")
                .expect("source missing");
        let (target_number, target_inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/replace-new/target")
                .expect("target lookup failed")
                .expect("target missing");
        let orphan_head_before = fs.superblock.s_last_orphan;
        let inode_table = fs.group_descs[0].inode_table();

        fs.sync_filesystem(&mut jbd2_dev)
            .expect("fixture sync failed");
        jbd2_dev.flush().expect("fixture checkpoint failed");
        jbd2_dev
            .set_journal_use(false)
            .expect("disable journal for direct fault injection");
        fail_after_write_sector.set(Some(inode_table));

        let error = rename(
            &mut jbd2_dev,
            &mut fs,
            "/replace-old/source",
            "/replace-new/target",
            RenameOptions::REPLACE,
        )
        .expect_err("inode-table write failure must abort replacement rename");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        drop(fs);
        let device = jbd2_dev.into_inner();
        let mut remount_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_dev).expect("remount after failed replacement");
        let (source_after, _) =
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/replace-old/source")
                .expect("remounted source lookup failed")
                .expect("source must remain after failed replacement");
        let (target_after, target_inode_after) =
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/replace-new/target")
                .expect("remounted target lookup failed")
                .expect("target must remain after failed replacement");
        assert_eq!(source_after, source_number);
        assert_eq!(target_after, target_number);
        assert_eq!(target_inode_after.i_links_count, target_inode.i_links_count);
        assert_eq!(remounted.superblock.s_last_orphan, orphan_head_before);
        assert_eq!(
            read_file(&mut remount_dev, &mut remounted, "/replace-old/source")
                .expect("source must stay readable"),
            b"replacement source"
        );
        assert_eq!(
            read_file(&mut remount_dev, &mut remounted, "/replace-new/target")
                .expect("target must stay readable"),
            b"original target"
        );
    }

    #[test]
    fn failed_directory_move_restores_dotdot_and_parent_links_after_remount() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkdir(&mut jbd2_dev, &mut fs, "/directory-old").expect("old parent mkdir failed");
        mkdir(&mut jbd2_dev, &mut fs, "/directory-new").expect("new parent mkdir failed");
        mkdir(&mut jbd2_dev, &mut fs, "/directory-old/moved")
            .expect("moved directory creation failed");

        let (old_parent_number, old_parent_inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/directory-old")
                .expect("old parent lookup failed")
                .expect("old parent missing");
        let (new_parent_number, new_parent_inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/directory-new")
                .expect("new parent lookup failed")
                .expect("new parent missing");
        let (moved_number, mut moved_inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/directory-old/moved")
                .expect("moved directory lookup failed")
                .expect("moved directory missing");
        let moved_block =
            loopfile::resolve_inode_block(&fs, &mut jbd2_dev, moved_number, &mut moved_inode, 0)
                .expect("moved directory block lookup failed")
                .expect("moved directory block missing");

        fs.sync_filesystem(&mut jbd2_dev)
            .expect("fixture sync failed");
        jbd2_dev.flush().expect("fixture checkpoint failed");
        jbd2_dev
            .set_journal_use(false)
            .expect("disable journal for direct fault injection");
        fail_after_write_sector.set(Some(moved_block.raw()));

        let error = rename(
            &mut jbd2_dev,
            &mut fs,
            "/directory-old/moved",
            "/directory-new/moved",
            RenameOptions::REPLACE,
        )
        .expect_err("dotdot write failure must abort the directory move");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        drop(fs);
        let device = jbd2_dev.into_inner();
        let mut remount_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_dev).expect("remount after failed directory move");
        let (moved_after, _) =
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/directory-old/moved")
                .expect("remounted moved-directory lookup failed")
                .expect("failed move must retain the old directory name");
        assert_eq!(moved_after, moved_number);
        assert!(
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/directory-new/moved")
                .expect("remounted destination lookup failed")
                .is_none()
        );
        let mut moved_directory_data = vec![0; rsext4::BLOCK_SIZE];
        remount_dev
            .read_blocks(&mut moved_directory_data, moved_block, 1)
            .expect("read remounted moved-directory block");
        let dot_rec_len = usize::from(u16::from_le_bytes([
            moved_directory_data[4],
            moved_directory_data[5],
        ]));
        let dotdot_raw = u32::from_le_bytes(
            moved_directory_data[dot_rec_len..dot_rec_len + 4]
                .try_into()
                .expect("dotdot inode field"),
        );
        assert_eq!(dotdot_raw, old_parent_number.raw());
        let old_parent_after = remounted
            .get_inode_by_num(&mut remount_dev, old_parent_number)
            .expect("remounted old parent inode");
        let new_parent_after = remounted
            .get_inode_by_num(&mut remount_dev, new_parent_number)
            .expect("remounted new parent inode");
        assert_eq!(
            old_parent_after.i_links_count,
            old_parent_inode.i_links_count
        );
        assert_eq!(
            new_parent_after.i_links_count,
            new_parent_inode.i_links_count
        );
    }

    /// Verifies cross-directory moves by checking that the source path disappears
    /// and the destination path keeps the original payload.
    #[test]
    fn test_file_move() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/sourcedir").expect("mkdir failed");
        mkdir(&mut jbd2_dev, &mut fs, "/destdir").expect("mkdir failed");

        let test_data = b"Data for move test";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/sourcedir/movefile",
            Some(test_data),
            None,
        )
        .expect("mkfile failed");

        let _ = rename(
            &mut jbd2_dev,
            &mut fs,
            "/sourcedir/movefile",
            "/destdir/movedfile",
            RenameOptions::NO_REPLACE,
        )
        .expect("mv failed");

        // The source entry should be removed after the move.
        let old_err = read_file(&mut jbd2_dev, &mut fs, "/sourcedir/movefile")
            .expect_err("old path should not exist");
        assert_eq!(old_err.kind(), Ext4ErrorKind::NotFound);

        // The destination should still resolve to the original file contents.
        let new_data =
            read_file(&mut jbd2_dev, &mut fs, "/destdir/movedfile").expect("read_file failed");
        assert_eq!(new_data, test_data.to_vec());

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies that deleting a file removes the directory entry and makes later
    /// reads fail with `ENOENT`.
    #[test]
    fn test_file_delete() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/deletetest").expect("mkdir failed");

        let test_data = b"Data for delete test";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/deletetest/deletefile",
            Some(test_data),
            None,
        )
        .expect("mkfile failed");

        // Confirm the file exists before deletion.
        let initial_data =
            read_file(&mut jbd2_dev, &mut fs, "/deletetest/deletefile").expect("read_file failed");
        assert_eq!(initial_data, test_data.to_vec());

        delete_file(&mut fs, &mut jbd2_dev, "/deletetest/deletefile").expect("delete failed");

        // The deleted path must no longer be readable.
        let deleted_err = read_file(&mut jbd2_dev, &mut fs, "/deletetest/deletefile")
            .expect_err("deleted path should not exist");
        assert_eq!(deleted_err.kind(), Ext4ErrorKind::NotFound);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    #[test]
    fn deleted_data_block_cache_does_not_survive_physical_block_reuse() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        let old_data = vec![0x2a; BLOCK_SIZE];
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/old-incarnation",
            Some(&old_data),
            None,
        )
        .expect("old file creation failed");
        let old_inode = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/old-incarnation")
            .unwrap()
            .unwrap()
            .0;
        let old_extent = inspect_inode_extents(
            &mut jbd2_dev,
            &mut fs,
            old_inode,
            0,
            BLOCK_SIZE as u64,
            FileExtentTarget::Data,
            1,
        )
        .unwrap()
        .extents[0];

        delete_file(&mut fs, &mut jbd2_dev, "/old-incarnation").expect("old file deletion failed");
        let old_block = AbsoluteBN::new(old_extent.physical_start / BLOCK_SIZE as u64);
        assert!(
            fs.datablock_cache.get(old_block).is_none(),
            "reaping an inode must discard the old physical-block incarnation"
        );

        let new_data = vec![0x51; BLOCK_SIZE];
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/new-incarnation",
            Some(&new_data),
            None,
        )
        .expect("new file creation failed");
        let new_inode = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/new-incarnation")
            .unwrap()
            .unwrap()
            .0;
        let new_extent = inspect_inode_extents(
            &mut jbd2_dev,
            &mut fs,
            new_inode,
            0,
            BLOCK_SIZE as u64,
            FileExtentTarget::Data,
            1,
        )
        .unwrap()
        .extents[0];

        assert_eq!(
            new_extent.physical_start, old_extent.physical_start,
            "fixture must exercise a reused physical block"
        );
        assert_eq!(
            read_file(&mut jbd2_dev, &mut fs, "/new-incarnation").unwrap(),
            new_data
        );
    }

    /// Verifies that a hard link publishes a second name for the same inode and
    /// persists the matching link count.
    #[test]
    fn test_hard_link() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/linktest").expect("mkdir failed");

        let test_data = b"Data for link test";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/linktest/original",
            Some(test_data),
            None,
        )
        .expect("mkfile failed");

        link(
            &mut fs,
            &mut jbd2_dev,
            "/linktest/hardlink",
            "/linktest/original",
        )
        .expect("hard-link creation failed");

        let original_data =
            read_file(&mut jbd2_dev, &mut fs, "/linktest/original").expect("read_file failed");
        assert_eq!(original_data, test_data.to_vec());
        let linked_data =
            read_file(&mut jbd2_dev, &mut fs, "/linktest/hardlink").expect("link read failed");
        assert_eq!(linked_data, test_data.to_vec());
        let (original_number, original_inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/linktest/original")
                .expect("original lookup failed")
                .expect("original missing");
        let (linked_number, linked_inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/linktest/hardlink")
                .expect("link lookup failed")
                .expect("link missing");
        assert_eq!(linked_number, original_number);
        assert_eq!(original_inode.i_links_count, 2);
        assert_eq!(linked_inode.i_links_count, 2);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    #[test]
    fn failed_hard_link_directory_publish_is_atomic_after_remount() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkdir(&mut jbd2_dev, &mut fs, "/source").expect("source mkdir failed");
        mkdir(&mut jbd2_dev, &mut fs, "/destination").expect("destination mkdir failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/source/original",
            Some(b"atomic hard-link target"),
            None,
        )
        .expect("target creation failed");

        let (target_number, target_inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/source/original")
                .expect("target lookup failed")
                .expect("target missing");
        let old_links = target_inode.i_links_count;
        let (destination_number, mut destination_inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/destination")
                .expect("destination lookup failed")
                .expect("destination missing");
        let destination_block = loopfile::resolve_inode_block(
            &fs,
            &mut jbd2_dev,
            destination_number,
            &mut destination_inode,
            0,
        )
        .expect("destination block lookup failed")
        .expect("destination block missing");

        fs.sync_filesystem(&mut jbd2_dev)
            .expect("fixture sync failed");
        jbd2_dev.flush().expect("fixture checkpoint failed");
        jbd2_dev
            .set_journal_use(false)
            .expect("disable journal for direct fault injection");
        fail_after_write_sector.set(Some(destination_block.raw()));

        let error = link(
            &mut fs,
            &mut jbd2_dev,
            "/destination/new-link",
            "/source/original",
        )
        .expect_err("directory publish failure must abort hard-link creation");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        drop(fs);
        let device = jbd2_dev.into_inner();
        let mut remount_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_dev).expect("remount after failed link");
        let (remounted_target, remounted_inode) =
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/source/original")
                .expect("remounted target lookup failed")
                .expect("remounted target missing");
        assert_eq!(remounted_target, target_number);
        assert_eq!(remounted_inode.i_links_count, old_links);
        assert!(
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/destination/new-link")
                .expect("remounted destination lookup failed")
                .is_none(),
            "a failed hard link must not publish its destination name"
        );
    }

    #[test]
    fn failed_hard_link_directory_growth_restores_allocation_after_remount() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkdir(&mut jbd2_dev, &mut fs, "/source").expect("source mkdir failed");
        mkdir(&mut jbd2_dev, &mut fs, "/destination").expect("destination mkdir failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/source/original",
            Some(b"directory growth target"),
            None,
        )
        .expect("target creation failed");

        for index in 0..15 {
            let name = format!("{index:03}{}", "a".repeat(252));
            let path = format!("/destination/{name}");
            mkfile(&mut jbd2_dev, &mut fs, &path, None, None)
                .expect("directory fill entry creation failed");
        }

        let (target_number, target_inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/source/original")
                .expect("target lookup failed")
                .expect("target missing");
        let old_links = target_inode.i_links_count;
        let (destination_number, destination_inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/destination")
                .expect("destination lookup failed")
                .expect("destination missing");
        assert_eq!(destination_inode.size(), BLOCK_SIZE as u64);

        fs.sync_filesystem(&mut jbd2_dev)
            .expect("fixture sync failed");
        jbd2_dev.flush().expect("fixture checkpoint failed");
        let free_blocks_before = fs.superblock.free_blocks_count();
        let block_bitmap = fs.group_descs[0].block_bitmap();
        jbd2_dev
            .set_journal_use(false)
            .expect("disable journal for direct fault injection");
        fail_after_write_sector.set(Some(block_bitmap));

        let link_name = format!("grow{}", "z".repeat(251));
        let link_path = format!("/destination/{link_name}");
        let error = link(&mut fs, &mut jbd2_dev, &link_path, "/source/original")
            .expect_err("block-bitmap failure must abort growing hard link");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        drop(fs);
        let device = jbd2_dev.into_inner();
        let mut remount_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_dev).expect("remount after failed growth");
        let (remounted_target, remounted_inode) =
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/source/original")
                .expect("remounted target lookup failed")
                .expect("remounted target missing");
        assert_eq!(remounted_target, target_number);
        assert_eq!(remounted_inode.i_links_count, old_links);
        assert!(
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, &link_path)
                .expect("remounted destination lookup failed")
                .is_none(),
            "a failed growing hard link must not publish its destination name"
        );
        let remounted_destination = remounted
            .get_inode_by_num(&mut remount_dev, destination_number)
            .expect("remounted destination inode read failed");
        assert_eq!(remounted_destination.size(), BLOCK_SIZE as u64);
        assert_eq!(remounted.superblock.free_blocks_count(), free_blocks_before);
        let second_block = loopfile::resolve_inode_block(
            &remounted,
            &mut remount_dev,
            destination_number,
            &mut { remounted_destination },
            1,
        )
        .expect("remounted second block lookup failed");
        assert!(second_block.is_none());
    }

    #[test]
    fn final_unlink_keeps_inode_alive_until_explicit_reap() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        let contents = b"open inode survives unlink";
        mkfile(&mut jbd2_dev, &mut fs, "/open-unlink", Some(contents), None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/open-unlink")
            .expect("lookup failed")
            .expect("created file missing")
            .0;

        let outcome = unlink(&mut fs, &mut jbd2_dev, "/open-unlink").expect("unlink failed");
        assert_eq!(outcome.inode, inode_number);
        assert!(outcome.requires_reap());

        assert!(
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/open-unlink")
                .expect("post-unlink lookup failed")
                .is_none(),
            "the directory entry must disappear"
        );
        assert!(
            fs.inode_num_already_allocated(&mut jbd2_dev, inode_number)
                .expect("unlinked inode allocation lookup failed"),
            "the zero-link inode must remain allocated while an open reference may exist"
        );
        let mut output = [0u8; 26];
        let read = read_inode_data_into(&mut jbd2_dev, &mut fs, inode_number, 0, &mut output)
            .expect("reading the unlinked inode by number failed");
        assert_eq!(&output[..read], contents);
    }

    #[test]
    fn failed_final_unlink_restores_dentry_link_count_and_orphan_head() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        let contents = b"failed final unlink stays reachable";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/unlink-fault",
            Some(contents),
            None,
        )
        .expect("file creation failed");
        let (inode_number, inode) =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/unlink-fault")
                .expect("target lookup failed")
                .expect("target missing");
        assert_eq!(inode.i_links_count, 1);
        let root_number = fs.root_inode;
        let mut root_inode = fs
            .get_inode_by_num(&mut jbd2_dev, root_number)
            .expect("root inode");
        let root_block =
            loopfile::resolve_inode_block(&fs, &mut jbd2_dev, root_number, &mut root_inode, 0)
                .expect("root block lookup failed")
                .expect("root block missing");

        fs.sync_filesystem(&mut jbd2_dev)
            .expect("fixture sync failed");
        jbd2_dev.flush().expect("fixture checkpoint failed");
        jbd2_dev
            .set_journal_use(false)
            .expect("disable journal for direct fault injection");
        fail_after_write_sector.set(Some(root_block.raw()));

        let error = unlink(&mut fs, &mut jbd2_dev, "/unlink-fault")
            .expect_err("directory write failure must abort final unlink");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        drop(fs);
        let device = jbd2_dev.into_inner();
        let mut remount_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_dev).expect("remount after failed unlink");
        let (remounted_number, remounted_inode) =
            dir::get_inode_with_num(&mut remounted, &mut remount_dev, "/unlink-fault")
                .expect("remounted target lookup failed")
                .expect("failed unlink must preserve the original name");
        assert_eq!(remounted_number, inode_number);
        assert_eq!(remounted_inode.i_links_count, 1);
        assert_eq!(remounted.superblock.s_last_orphan, 0);
        assert!(
            remounted
                .inode_num_already_allocated(&mut remount_dev, inode_number)
                .expect("inode allocation lookup failed")
        );
        let restored =
            read_file(&mut remount_dev, &mut remounted, "/unlink-fault").expect("restored read");
        assert_eq!(restored, contents);
    }

    #[cfg(not(feature = "USE_MULTILEVEL_CACHE"))]
    #[test]
    fn failed_orphan_reap_does_not_lose_freed_block_accounting() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        let free_blocks_before_file = fs.superblock.free_blocks_count();
        let contents = vec![0x5a; 2 * BLOCK_SIZE];
        mkfile(&mut jbd2_dev, &mut fs, "/reap-fault", Some(&contents), None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/reap-fault")
            .expect("target lookup failed")
            .expect("target missing")
            .0;
        let outcome = unlink(&mut fs, &mut jbd2_dev, "/reap-fault").expect("fixture unlink failed");
        assert_eq!(outcome.inode, inode_number);
        assert!(outcome.requires_reap());

        fs.sync_filesystem(&mut jbd2_dev)
            .expect("fixture sync failed");
        jbd2_dev.flush().expect("fixture checkpoint failed");
        let block_bitmap = fs.group_descs[0].block_bitmap();
        jbd2_dev
            .set_journal_use(false)
            .expect("disable journal for direct fault injection");
        fail_after_write_sector.set(Some(block_bitmap));

        let error = reap_unlinked_inode(&mut fs, &mut jbd2_dev, inode_number)
            .expect_err("block-bitmap write failure must abort orphan reap");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);
        assert_eq!(fs.superblock.s_last_orphan, inode_number.raw());
        assert!(
            fs.inode_num_already_allocated(&mut jbd2_dev, inode_number)
                .expect("failed reap allocation lookup")
        );

        drop(fs);
        let device = jbd2_dev.into_inner();
        let mut remount_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_dev).expect("orphan recovery mount failed");
        assert_eq!(remounted.superblock.s_last_orphan, 0);
        assert!(
            !remounted
                .inode_num_already_allocated(&mut remount_dev, inode_number)
                .expect("recovered allocation lookup failed")
        );
        assert_eq!(
            remounted.superblock.free_blocks_count(),
            free_blocks_before_file,
            "retrying a partially failed reap must account every freed block exactly once"
        );
    }

    #[test]
    fn hard_link_propagates_corrupt_destination_parent_extent() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkdir(&mut jbd2_dev, &mut fs, "/source").expect("source mkdir failed");
        mkdir(&mut jbd2_dev, &mut fs, "/destination").expect("destination mkdir failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/source/original",
            Some(b"link target"),
            None,
        )
        .expect("target creation failed");

        let destination_ino = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/destination")
            .expect("destination lookup failed")
            .expect("destination missing")
            .0;
        fs.modify_inode(&mut jbd2_dev, destination_ino, |inode| {
            inode.i_block[0] = 0;
        })
        .expect("extent corruption injection failed");

        let error = link(
            &mut fs,
            &mut jbd2_dev,
            "/destination/missing/new-link",
            "/source/original",
        )
        .expect_err("corrupt destination parent must abort hard-link creation");
        assert_eq!(error.kind(), Ext4ErrorKind::Corrupted);
        assert_eq!(
            error.context(),
            Some(ErrorContext::Operation {
                op: "extent:bad_magic",
            })
        );
    }

    #[test]
    fn legacy_indirect_unlink_defers_blocks_until_reap() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-indirect", None, None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-indirect")
            .unwrap()
            .unwrap()
            .0;
        let free_blocks_before = fs.superblock.free_blocks_count();
        let indirect_root = fs.alloc_block(&mut jbd2_dev).unwrap();
        let indirect_data = fs.alloc_block(&mut jbd2_dev).unwrap();
        write_block_fixture(&mut jbd2_dev, indirect_root, true, |image| {
            image[..core::mem::size_of::<u32>()]
                .copy_from_slice(&indirect_data.to_u32().unwrap().to_le_bytes());
        });
        fs.datablock_cache
            .modify_new(&mut jbd2_dev, indirect_data, |block| block[0] = 0x5a)
            .unwrap();
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_block[12] = indirect_root.to_u32().unwrap();
            inode.i_size_lo = (13 * BLOCK_SIZE) as u32;
            inode.i_size_high = 0;
            inode.i_blocks_lo = 2 * (BLOCK_SIZE / 512) as u32;
        })
        .unwrap();

        let outcome = unlink(&mut fs, &mut jbd2_dev, "/legacy-indirect")
            .expect("final unlink must retain a legacy inode for open references");
        assert_eq!(outcome.inode, inode_number);
        assert!(outcome.requires_reap());
        assert!(
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-indirect")
                .unwrap()
                .is_none()
        );
        assert!(
            fs.inode_num_already_allocated(&mut jbd2_dev, inode_number)
                .unwrap()
        );
        let inode = fs.get_inode_by_num(&mut jbd2_dev, inode_number).unwrap();
        assert_eq!(inode.i_links_count, 0);
        assert_eq!(inode.i_block[12], indirect_root.to_u32().unwrap());
        assert_eq!(fs.superblock.s_last_orphan, inode_number.raw());
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before - 2);
        let mut marker = [0u8; 1];
        assert_eq!(
            read_inode_data_into(
                &mut jbd2_dev,
                &mut fs,
                inode_number,
                12 * BLOCK_SIZE as u64,
                &mut marker,
            )
            .unwrap(),
            1
        );
        assert_eq!(marker, [0x5a]);

        reap_unlinked_inode(&mut fs, &mut jbd2_dev, inode_number)
            .expect("final reference release must reclaim legacy blocks and inode");
        assert_eq!(fs.superblock.s_last_orphan, 0);
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
        assert!(
            !fs.inode_num_already_allocated(&mut jbd2_dev, inode_number)
                .unwrap()
        );
    }

    #[test]
    fn legacy_indirect_delete_reclaims_data_metadata_and_inode() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-delete", None, None).expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-delete")
            .unwrap()
            .unwrap()
            .0;
        let free_blocks_before = fs.superblock.free_blocks_count();
        let indirect_root = fs.alloc_block(&mut jbd2_dev).unwrap();
        let indirect_data = fs.alloc_block(&mut jbd2_dev).unwrap();
        write_block_fixture(&mut jbd2_dev, indirect_root, true, |image| {
            image[..core::mem::size_of::<u32>()]
                .copy_from_slice(&indirect_data.to_u32().unwrap().to_le_bytes());
        });
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_block[12] = indirect_root.to_u32().unwrap();
            inode.i_size_lo = (13 * BLOCK_SIZE) as u32;
            inode.i_size_high = 0;
            inode.i_blocks_lo = 2 * (BLOCK_SIZE / 512) as u32;
        })
        .unwrap();

        delete_file(&mut fs, &mut jbd2_dev, "/legacy-delete")
            .expect("legacy indirect delete must reclaim the inode");

        assert!(
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-delete")
                .unwrap()
                .is_none()
        );
        assert_eq!(fs.superblock.s_last_orphan, 0);
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
        assert!(
            !fs.inode_num_already_allocated(&mut jbd2_dev, inode_number)
                .unwrap()
        );
    }

    #[test]
    fn corrupt_legacy_indirect_unlink_fails_before_inode_mutation() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-corrupt", None, None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-corrupt")
            .unwrap()
            .unwrap()
            .0;
        let indirect_root = fs.alloc_block(&mut jbd2_dev).unwrap();
        write_block_fixture(&mut jbd2_dev, indirect_root, true, |image| {
            image[..core::mem::size_of::<u32>()].copy_from_slice(&u32::MAX.to_le_bytes());
        });
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_block[12] = indirect_root.to_u32().unwrap();
        })
        .unwrap();
        let free_blocks_before = fs.superblock.free_blocks_count();

        let error = unlink(&mut fs, &mut jbd2_dev, "/legacy-corrupt")
            .expect_err("corrupt indirect ownership must abort final unlink");
        assert_eq!(error.kind(), Ext4ErrorKind::Corrupted);
        assert_eq!(
            error.context(),
            Some(ErrorContext::Operation {
                op: "indirect:physical_range",
            })
        );
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);

        let (_, inode) = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-corrupt")
            .unwrap()
            .expect("failed unlink must preserve the directory entry");
        assert_eq!(inode.i_links_count, 1);
        assert_eq!(
            inode.i_block[12],
            indirect_root
                .to_u32()
                .expect("allocated block must fit u32")
        );
    }

    #[test]
    fn deleting_legacy_direct_inode_frees_blocks_beyond_inode_size() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-hidden-direct", None, None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-hidden-direct")
            .unwrap()
            .unwrap()
            .0;
        let free_blocks_before = fs.superblock.free_blocks_count();
        let hidden_data = fs.alloc_block(&mut jbd2_dev).unwrap();
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_block[0] = hidden_data.to_u32().unwrap();
            inode.i_size_lo = 0;
            inode.i_size_high = 0;
            inode.i_blocks_lo = (BLOCK_SIZE / 512) as u32;
        })
        .unwrap();

        delete_file(&mut fs, &mut jbd2_dev, "/legacy-hidden-direct")
            .expect("direct-only legacy inode must remain deletable");

        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
        assert!(
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-hidden-direct")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn growing_legacy_inode_across_triple_boundary_keeps_sparse_holes() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-sparse-grow", None, None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-sparse-grow")
            .unwrap()
            .unwrap()
            .0;
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_blocks_lo = 0;
        })
        .unwrap();
        let pointers = BLOCK_SIZE / core::mem::size_of::<u32>();
        let triple_first_lbn = 12usize + pointers + pointers * pointers;
        let grown_blocks = triple_first_lbn + 2;
        let grown_size = grown_blocks as u64 * BLOCK_SIZE as u64;
        let free_blocks_before = fs.superblock.free_blocks_count();

        truncate(&mut jbd2_dev, &mut fs, "/legacy-sparse-grow", grown_size)
            .expect("legacy sparse growth must not allocate indirect branches");

        let inode = fs.get_inode_by_num(&mut jbd2_dev, inode_number).unwrap();
        assert_eq!(inode.size(), grown_size);
        assert_eq!(inode.i_block, [0; 15]);
        assert_eq!(inode.i_blocks_lo, 0);
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);

        let mut probe = [0xa5; 64];
        let offset = triple_first_lbn as u64 * BLOCK_SIZE as u64;
        let read =
            read_inode_data_into(&mut jbd2_dev, &mut fs, inode_number, offset, &mut probe).unwrap();
        assert_eq!(read, probe.len());
        assert_eq!(probe, [0; 64]);

        truncate(
            &mut jbd2_dev,
            &mut fs,
            "/legacy-sparse-grow",
            BLOCK_SIZE as u64,
        )
        .expect("shrinking an unallocated legacy hole needs no indirect free");
        let inode = fs.get_inode_by_num(&mut jbd2_dev, inode_number).unwrap();
        assert_eq!(inode.size(), BLOCK_SIZE as u64);
        assert_eq!(inode.i_block, [0; 15]);
        assert_eq!(inode.i_blocks_lo, 0);
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
    }

    #[test]
    fn shrinking_legacy_inode_frees_hidden_indirect_tree() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-hidden-root", None, None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-hidden-root")
            .unwrap()
            .unwrap()
            .0;
        let indirect_root = fs.alloc_block(&mut jbd2_dev).unwrap();
        let hidden_data = fs.alloc_block(&mut jbd2_dev).unwrap();
        write_block_fixture(&mut jbd2_dev, indirect_root, true, |image| {
            image[..core::mem::size_of::<u32>()]
                .copy_from_slice(&hidden_data.to_u32().unwrap().to_le_bytes());
        });
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_block[12] = indirect_root.to_u32().unwrap();
            inode.i_size_lo = 1;
            inode.i_size_high = 0;
            inode.i_blocks_lo = 2 * (BLOCK_SIZE / 512) as u32;
        })
        .unwrap();
        let free_blocks_before = fs.superblock.free_blocks_count();

        truncate(&mut jbd2_dev, &mut fs, "/legacy-hidden-root", 0)
            .expect("truncate must free hidden indirect data and metadata");

        let inode = fs.get_inode_by_num(&mut jbd2_dev, inode_number).unwrap();
        assert_eq!(inode.size(), 0);
        assert_eq!(inode.i_block[12], 0);
        assert_eq!(inode.i_blocks_lo, 0);
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before + 2);
    }

    #[test]
    fn shrinking_legacy_inode_prunes_partial_indirect_leaf() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        let pointers = BLOCK_SIZE / core::mem::size_of::<u32>();
        let cases = [
            ("/truncate-single-leaf", 12u32, 1usize, 12usize),
            ("/truncate-double-leaf", (12 + pointers) as u32, 2, 13),
            (
                "/truncate-triple-leaf",
                (12 + pointers + pointers * pointers) as u32,
                3,
                14,
            ),
        ];

        for (path, first_logical, metadata_depth, root_slot) in cases {
            mkfile(&mut jbd2_dev, &mut fs, path, None, None).expect("file creation failed");
            let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, path)
                .unwrap()
                .unwrap()
                .0;
            fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
                inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
                inode.i_block = [0; 15];
                inode.i_blocks_lo = 0;
            })
            .unwrap();
            let free_blocks_before = fs.superblock.free_blocks_count();

            for logical in [first_logical, first_logical + 1] {
                write_file(
                    &mut jbd2_dev,
                    &mut fs,
                    path,
                    u64::from(logical) * BLOCK_SIZE as u64,
                    &[logical as u8],
                )
                .unwrap();
            }
            let free_blocks_before_truncate = fs.superblock.free_blocks_count();
            truncate(
                &mut jbd2_dev,
                &mut fs,
                path,
                (u64::from(first_logical) + 1) * BLOCK_SIZE as u64,
            )
            .expect("partial indirect leaf truncate failed");

            let mut inode = fs.get_inode_by_num(&mut jbd2_dev, inode_number).unwrap();
            assert_ne!(inode.i_block[root_slot], 0);
            assert!(
                loopfile::resolve_inode_block(
                    &fs,
                    &mut jbd2_dev,
                    inode_number,
                    &mut inode,
                    first_logical,
                )
                .unwrap()
                .is_some()
            );
            assert_eq!(
                loopfile::resolve_inode_block(
                    &fs,
                    &mut jbd2_dev,
                    inode_number,
                    &mut inode,
                    first_logical + 1,
                )
                .unwrap(),
                None
            );
            let huge_file = fs.superblock.has_feature_ro_compat(
                superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE,
            );
            assert_eq!(
                inode.blocks_count(BLOCK_SIZE as u32, huge_file),
                (metadata_depth as u64 + 1) * (BLOCK_SIZE / 512) as u64
            );
            assert_eq!(
                fs.superblock.free_blocks_count(),
                free_blocks_before_truncate + 1
            );

            truncate(&mut jbd2_dev, &mut fs, path, 0).expect("full indirect truncate failed");
            let inode = fs.get_inode_by_num(&mut jbd2_dev, inode_number).unwrap();
            assert_eq!(inode.i_block, [0; 15]);
            assert_eq!(inode.i_blocks_lo, 0);
            assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
        }
    }

    #[test]
    fn shrinking_legacy_inode_prunes_right_indirect_subtrees() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        let pointers = BLOCK_SIZE / core::mem::size_of::<u32>();
        let double = pointers * pointers;
        let cases = [
            (
                "/truncate-double-subtree",
                (12 + pointers) as u32,
                pointers as u32,
                3u64,
                2u64,
                13usize,
            ),
            (
                "/truncate-triple-subtree",
                (12 + pointers + double) as u32,
                double as u32,
                4u64,
                3u64,
                14usize,
            ),
        ];

        for (path, first_logical, subtree_stride, kept_blocks, freed_blocks, root_slot) in cases {
            mkfile(&mut jbd2_dev, &mut fs, path, None, None).expect("file creation failed");
            let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, path)
                .unwrap()
                .unwrap()
                .0;
            fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
                inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
                inode.i_block = [0; 15];
                inode.i_blocks_lo = 0;
            })
            .unwrap();
            let free_blocks_before = fs.superblock.free_blocks_count();
            let right_logical = first_logical + subtree_stride;

            for logical in [first_logical, right_logical] {
                write_file(
                    &mut jbd2_dev,
                    &mut fs,
                    path,
                    u64::from(logical) * BLOCK_SIZE as u64,
                    &[logical as u8],
                )
                .unwrap();
            }
            let free_blocks_before_truncate = fs.superblock.free_blocks_count();
            truncate(
                &mut jbd2_dev,
                &mut fs,
                path,
                u64::from(right_logical) * BLOCK_SIZE as u64,
            )
            .expect("right indirect subtree truncate failed");

            let mut inode = fs.get_inode_by_num(&mut jbd2_dev, inode_number).unwrap();
            assert_ne!(inode.i_block[root_slot], 0);
            assert!(
                loopfile::resolve_inode_block(
                    &fs,
                    &mut jbd2_dev,
                    inode_number,
                    &mut inode,
                    first_logical,
                )
                .unwrap()
                .is_some()
            );
            assert_eq!(
                loopfile::resolve_inode_block(
                    &fs,
                    &mut jbd2_dev,
                    inode_number,
                    &mut inode,
                    right_logical,
                )
                .unwrap(),
                None
            );
            let huge_file = fs.superblock.has_feature_ro_compat(
                superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE,
            );
            assert_eq!(
                inode.blocks_count(BLOCK_SIZE as u32, huge_file),
                kept_blocks * (BLOCK_SIZE / 512) as u64
            );
            assert_eq!(
                fs.superblock.free_blocks_count(),
                free_blocks_before_truncate + freed_blocks
            );

            truncate(&mut jbd2_dev, &mut fs, path, 0).expect("full indirect truncate failed");
            let inode = fs.get_inode_by_num(&mut jbd2_dev, inode_number).unwrap();
            assert_eq!(inode.i_block, [0; 15]);
            assert_eq!(inode.i_blocks_lo, 0);
            assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
        }
    }

    #[test]
    fn growing_extent_inode_keeps_sparse_holes_and_logical_read_order() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/extent-sparse-grow", None, None)
            .expect("file creation failed");
        let free_blocks_before = fs.superblock.free_blocks_count();
        let grown_size = 20 * BLOCK_SIZE as u64;

        truncate(&mut jbd2_dev, &mut fs, "/extent-sparse-grow", grown_size)
            .expect("extent sparse growth must not allocate blocks");

        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
        let data = read_file(&mut jbd2_dev, &mut fs, "/extent-sparse-grow").unwrap();
        assert_eq!(data.len(), grown_size as usize);
        assert!(data.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn deleting_fast_symlink_does_not_treat_inline_bytes_as_indirect_blocks() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        let target = "/abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz";
        assert!(target.len() > 48 && target.len() < 60);
        mkfile(&mut jbd2_dev, &mut fs, target, None, None).expect("target creation failed");
        create_symbol_link(&mut jbd2_dev, &mut fs, target, "/fast-link")
            .expect("symlink creation failed");

        delete_file(&mut fs, &mut jbd2_dev, "/fast-link")
            .expect("fast symlink deletion must not inspect inline bytes as block pointers");
        assert!(
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/fast-link")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn legacy_sparse_read_zero_fills_holes_and_continues() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-sparse", None, None).expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-sparse")
            .unwrap()
            .unwrap()
            .0;
        let first = fs.alloc_block(&mut jbd2_dev).unwrap();
        let third = fs.alloc_block(&mut jbd2_dev).unwrap();
        for (block, value) in [(first, 0x31), (third, 0x33)] {
            write_block_fixture(&mut jbd2_dev, block, false, |image| image.fill(value));
        }
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_block[0] = first.to_u32().unwrap();
            inode.i_block[2] = third.to_u32().unwrap();
            let size = 3 * BLOCK_SIZE as u64;
            inode.i_size_lo = size as u32;
            inode.i_size_high = (size >> 32) as u32;
        })
        .unwrap();

        let data = read_file(&mut jbd2_dev, &mut fs, "/legacy-sparse").unwrap();
        assert_eq!(data.len(), 3 * BLOCK_SIZE);
        assert!(data[..BLOCK_SIZE].iter().all(|&byte| byte == 0x31));
        assert!(
            data[BLOCK_SIZE..2 * BLOCK_SIZE]
                .iter()
                .all(|&byte| byte == 0)
        );
        assert!(data[2 * BLOCK_SIZE..].iter().all(|&byte| byte == 0x33));
    }

    #[test]
    fn legacy_single_indirect_overwrite_preserves_inode_format_and_mapping() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-overwrite", None, None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-overwrite")
            .unwrap()
            .unwrap()
            .0;
        let indirect_root = fs.alloc_block(&mut jbd2_dev).unwrap();
        let data_block = fs.alloc_block(&mut jbd2_dev).unwrap();
        write_block_fixture(&mut jbd2_dev, indirect_root, true, |image| {
            image[..4].copy_from_slice(&data_block.to_u32().unwrap().to_le_bytes());
        });
        fs.datablock_cache
            .modify_new(&mut jbd2_dev, data_block, |data| data.fill(0x41))
            .unwrap();
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_block[12] = indirect_root.to_u32().unwrap();
            inode.i_blocks_lo = 2 * (BLOCK_SIZE / 512) as u32;
            inode.l_i_blocks_high = 0;
            let size = 13 * BLOCK_SIZE as u64;
            inode.i_size_lo = size as u32;
            inode.i_size_high = (size >> 32) as u32;
        })
        .unwrap();

        let payload = b"legacy-single-indirect";
        write_file(
            &mut jbd2_dev,
            &mut fs,
            "/legacy-overwrite",
            12 * BLOCK_SIZE as u64,
            payload,
        )
        .unwrap();

        let (_, inode) = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-overwrite")
            .unwrap()
            .unwrap();
        assert!(!inode.uses_extents());
        assert_eq!(
            inode.i_block[12],
            indirect_root.to_u32().unwrap(),
            "an overwrite must not replace the legacy root"
        );
        let data = read_file(&mut jbd2_dev, &mut fs, "/legacy-overwrite").unwrap();
        assert_eq!(
            &data[12 * BLOCK_SIZE..12 * BLOCK_SIZE + payload.len()],
            payload
        );
    }

    #[test]
    fn legacy_sparse_write_allocates_direct_and_all_indirect_levels() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        let pointers = BLOCK_SIZE / core::mem::size_of::<u32>();
        let cases = [
            ("/legacy-direct", 0u32, 1u64),
            ("/legacy-single", 12u32, 2u64),
            ("/legacy-double", (12 + pointers) as u32, 3u64),
            (
                "/legacy-triple",
                (12 + pointers + pointers * pointers) as u32,
                4u64,
            ),
        ];

        for (case_index, (path, logical, allocated_blocks)) in cases.into_iter().enumerate() {
            mkfile(&mut jbd2_dev, &mut fs, path, None, None).expect("file creation failed");
            let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, path)
                .unwrap()
                .unwrap()
                .0;
            fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
                inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
                inode.i_block = [0; 15];
            })
            .unwrap();

            let payload = [0xa0 + case_index as u8; 32];
            write_file(
                &mut jbd2_dev,
                &mut fs,
                path,
                u64::from(logical) * BLOCK_SIZE as u64,
                &payload,
            )
            .unwrap();

            let (_, mut inode) = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, path)
                .unwrap()
                .unwrap();
            assert!(!inode.uses_extents());
            let physical = loopfile::resolve_inode_block(
                &fs,
                &mut jbd2_dev,
                inode_number,
                &mut inode,
                logical,
            )
            .unwrap()
            .expect("new legacy mapping must be reachable");
            let cached = fs
                .datablock_cache
                .get_or_load(&mut jbd2_dev, physical)
                .unwrap();
            assert_eq!(&cached.data[..payload.len()], &payload);
            let huge_file = fs.superblock.has_feature_ro_compat(
                superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE,
            );
            assert_eq!(
                inode.blocks_count(BLOCK_SIZE as u32, huge_file),
                allocated_blocks * (BLOCK_SIZE / 512) as u64
            );
        }
    }

    #[test]
    fn failed_indirect_parent_publish_restores_pointer_before_freeing_branch() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-publish", None, None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-publish")
            .unwrap()
            .unwrap()
            .0;
        let indirect_root = fs.alloc_block(&mut jbd2_dev).unwrap();
        jbd2_dev.umount_commit().unwrap();
        jbd2_dev.set_journal_use(false).unwrap();
        write_block_fixture(&mut jbd2_dev, indirect_root, true, |_| {});
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_block[12] = indirect_root.to_u32().unwrap();
            inode.i_blocks_lo = (BLOCK_SIZE / 512) as u32;
            let size = 13 * BLOCK_SIZE as u64;
            inode.i_size_lo = size as u32;
            inode.i_size_high = (size >> 32) as u32;
        })
        .unwrap();
        let free_blocks_before = fs.superblock.free_blocks_count();

        fail_after_write_sector.set(Some(indirect_root.raw()));
        let error = write_file(
            &mut jbd2_dev,
            &mut fs,
            "/legacy-publish",
            12 * BLOCK_SIZE as u64,
            b"publish-failure",
        )
        .unwrap_err();
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        jbd2_dev.read_block(indirect_root).unwrap();
        assert_eq!(
            u32::from_le_bytes(jbd2_dev.buffer()[..4].try_into().unwrap()),
            0,
            "the failed child pointer must be restored before its block is freed"
        );
        let (_, inode) = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-publish")
            .unwrap()
            .unwrap();
        assert_eq!(
            inode.blocks_count(BLOCK_SIZE as u32, true),
            (BLOCK_SIZE / 512) as u64
        );
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
    }

    #[cfg(not(feature = "USE_MULTILEVEL_CACHE"))]
    #[test]
    fn failed_legacy_truncate_finalize_restores_pointer_and_inode_before_freeing() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/legacy-truncate-finalize",
            None,
            None,
        )
        .expect("file creation failed");
        let inode_number =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-truncate-finalize")
                .unwrap()
                .unwrap()
                .0;
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_blocks_lo = 0;
        })
        .unwrap();
        for logical in [12u64, 13] {
            write_file(
                &mut jbd2_dev,
                &mut fs,
                "/legacy-truncate-finalize",
                logical * BLOCK_SIZE as u64,
                &[logical as u8],
            )
            .unwrap();
        }
        jbd2_dev.umount_commit().unwrap();
        jbd2_dev.set_journal_use(false).unwrap();

        let original_inode = fs.get_inode_by_num(&mut jbd2_dev, inode_number).unwrap();
        let indirect_root = bmalloc::AbsoluteBN::from(original_inode.i_block[12]);
        jbd2_dev.read_block(indirect_root).unwrap();
        let original_pointers = jbd2_dev.buffer()[..2 * core::mem::size_of::<u32>()].to_vec();
        let free_blocks_before = fs.superblock.free_blocks_count();
        let inode_table = fs.group_descs[0].inode_table();

        fail_after_write_sector.set(Some(inode_table));
        let error = truncate(
            &mut jbd2_dev,
            &mut fs,
            "/legacy-truncate-finalize",
            13 * BLOCK_SIZE as u64,
        )
        .expect_err("inode finalize failure must abort legacy truncate");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        jbd2_dev.read_block(indirect_root).unwrap();
        assert_eq!(
            &jbd2_dev.buffer()[..original_pointers.len()],
            &original_pointers,
            "the indirect pointer block must be restored before returning"
        );
        let inode = fs.get_inode_by_num(&mut jbd2_dev, inode_number).unwrap();
        assert_eq!(inode.size(), original_inode.size());
        assert_eq!(inode.i_block, original_inode.i_block);
        assert_eq!(inode.i_blocks_lo, original_inode.i_blocks_lo);
        assert_eq!(inode.l_i_blocks_high, original_inode.l_i_blocks_high);
        assert_eq!(inode.i_flags, original_inode.i_flags);
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
    }

    #[test]
    fn failed_legacy_truncate_bitmap_write_restores_mapping_and_accounting() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/legacy-truncate-bitmap",
            None,
            None,
        )
        .expect("file creation failed");
        let inode_number =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-truncate-bitmap")
                .expect("fixture lookup failed")
                .expect("fixture missing")
                .0;
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_blocks_lo = 0;
        })
        .expect("legacy mapping setup failed");
        for logical in [12u64, 13] {
            write_file(
                &mut jbd2_dev,
                &mut fs,
                "/legacy-truncate-bitmap",
                logical * BLOCK_SIZE as u64,
                &[logical as u8],
            )
            .expect("legacy block write failed");
        }
        fs.sync_filesystem(&mut jbd2_dev)
            .expect("fixture sync failed");
        jbd2_dev.flush().expect("fixture checkpoint failed");
        jbd2_dev
            .set_journal_use(false)
            .expect("disable journal for direct fault injection");

        let original_inode = fs
            .get_inode_by_num(&mut jbd2_dev, inode_number)
            .expect("fixture inode read failed");
        let indirect_root = bmalloc::AbsoluteBN::from(original_inode.i_block[12]);
        jbd2_dev
            .read_block(indirect_root)
            .expect("indirect root read failed");
        let original_pointers = jbd2_dev.buffer()[..2 * core::mem::size_of::<u32>()].to_vec();
        let released_data = bmalloc::AbsoluteBN::from(u32::from_le_bytes(
            original_pointers[4..8]
                .try_into()
                .expect("second pointer slice"),
        ));
        let (released_group, released_in_group) = fs
            .block_allocator
            .global_to_group(released_data)
            .expect("released block group lookup failed");
        let block_bitmap = fs.group_descs[released_group.as_usize().unwrap()].block_bitmap();
        let free_blocks_before = fs.superblock.free_blocks_count();

        fail_after_write_sector.set(Some(block_bitmap));
        let error = truncate_inode(&mut jbd2_dev, &mut fs, inode_number, 13 * BLOCK_SIZE as u64)
            .expect_err("block-bitmap write failure must abort legacy truncate");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        let inode = fs
            .get_inode_by_num(&mut jbd2_dev, inode_number)
            .expect("inode read after failed truncate");
        assert_eq!(inode.size(), original_inode.size());
        assert_eq!(inode.i_block, original_inode.i_block);
        assert_eq!(inode.i_blocks_lo, original_inode.i_blocks_lo);
        assert_eq!(inode.l_i_blocks_high, original_inode.l_i_blocks_high);
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
        let bitmap = fs
            .bitmap_cache
            .get_or_load(
                &mut jbd2_dev,
                cache::bitmap::CacheKey::new_block(released_group),
                bmalloc::AbsoluteBN::new(block_bitmap),
            )
            .expect("bitmap read after failed truncate");
        let byte = bitmap.data[released_in_group.as_usize().unwrap() / 8];
        assert_ne!(byte & (1 << (released_in_group.raw() % 8)), 0);
        jbd2_dev
            .read_block(indirect_root)
            .expect("indirect root reread failed");
        assert_eq!(
            &jbd2_dev.buffer()[..original_pointers.len()],
            &original_pointers,
            "failed allocator publication must restore the old pointer tree"
        );

        drop(fs);
        let device = jbd2_dev.into_inner();
        let mut remount_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_dev).expect("remount after failed truncate");
        let remounted_inode = remounted
            .get_inode_by_num(&mut remount_dev, inode_number)
            .expect("remounted inode read failed");
        assert_eq!(remounted_inode.size(), original_inode.size());
        assert_eq!(remounted_inode.i_block, original_inode.i_block);
        assert_eq!(remounted_inode.i_blocks_lo, original_inode.i_blocks_lo);
        assert_eq!(remounted.superblock.free_blocks_count(), free_blocks_before);
        let remounted_bitmap = remounted
            .bitmap_cache
            .get_or_load(
                &mut remount_dev,
                cache::bitmap::CacheKey::new_block(released_group),
                bmalloc::AbsoluteBN::new(block_bitmap),
            )
            .expect("remounted bitmap read failed");
        let remounted_byte = remounted_bitmap.data[released_in_group.as_usize().unwrap() / 8];
        assert_ne!(remounted_byte & (1 << (released_in_group.raw() % 8)), 0);
        remount_dev
            .read_block(indirect_root)
            .expect("remounted indirect root read failed");
        assert_eq!(
            &remount_dev.buffer()[..original_pointers.len()],
            &original_pointers
        );
    }

    #[test]
    fn undersized_journal_rejects_legacy_truncate_before_mutation() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/legacy-truncate-capacity",
            None,
            None,
        )
        .expect("file creation failed");
        let inode_number =
            dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-truncate-capacity")
                .expect("fixture lookup failed")
                .expect("fixture missing")
                .0;
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_blocks_lo = 0;
        })
        .expect("legacy mapping setup failed");
        for logical in [12u64, 13] {
            write_file(
                &mut jbd2_dev,
                &mut fs,
                "/legacy-truncate-capacity",
                logical * BLOCK_SIZE as u64,
                &[logical as u8],
            )
            .expect("legacy block write failed");
        }
        fs.sync_filesystem(&mut jbd2_dev)
            .expect("fixture sync failed");
        jbd2_dev.flush().expect("fixture checkpoint failed");
        let original_inode = fs
            .get_inode_by_num(&mut jbd2_dev, inode_number)
            .expect("fixture inode read failed");
        let free_blocks_before = fs.superblock.free_blocks_count();
        let journal_sequence_before = jbd2_dev.journal_sequence();

        let journal_start = fs
            .journal_sb_block_start
            .expect("internal journal block missing");
        let mut small_journal = jbd2::jbdstruct::JournalSuperBlock::default();
        small_journal.s_header.h_blocktype = jbd2::jbdstruct::JBD2_BLOCKTYPE_SUPERBLOCK_V1;
        small_journal.s_blocksize = BLOCK_SIZE as u32;
        // Linux limits one transaction to one third of the journal. Fifteen
        // blocks preserve this fixture's original three user-credit limit
        // after descriptor and commit bookkeeping.
        small_journal.s_maxlen = 15;
        small_journal.s_first = 1;
        small_journal.s_sequence = journal_sequence_before.unwrap_or(1);
        small_journal.s_start = 0;
        jbd2_dev
            .set_journal_superblock(small_journal, journal_start)
            .expect("small journal installation failed");

        let error = truncate_inode(&mut jbd2_dev, &mut fs, inode_number, 13 * BLOCK_SIZE as u64)
            .expect_err("legacy truncate must preflight its complete metadata footprint");
        assert_eq!(error.kind(), Ext4ErrorKind::NoSpace);
        let inode = fs
            .get_inode_by_num(&mut jbd2_dev, inode_number)
            .expect("inode read after capacity failure");
        assert_eq!(inode.size(), original_inode.size());
        assert_eq!(inode.i_block, original_inode.i_block);
        assert_eq!(inode.i_blocks_lo, original_inode.i_blocks_lo);
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
        assert_eq!(fs.superblock.s_last_orphan, 0);
        assert_eq!(jbd2_dev.journal_sequence(), journal_sequence_before);
    }

    #[cfg(not(feature = "USE_MULTILEVEL_CACHE"))]
    #[test]
    fn failed_legacy_inode_finalize_restores_cached_inode_before_freeing_blocks() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-finalize", None, None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-finalize")
            .unwrap()
            .unwrap()
            .0;
        jbd2_dev.umount_commit().unwrap();
        jbd2_dev.set_journal_use(false).unwrap();
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
        })
        .unwrap();
        let inode_table = fs.group_descs[0].inode_table();
        let free_blocks_before = fs.superblock.free_blocks_count();

        fail_after_write_sector.set(Some(inode_table));
        let error = write_file(
            &mut jbd2_dev,
            &mut fs,
            "/legacy-finalize",
            0,
            b"inode-failure",
        )
        .unwrap_err();
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        let (_, inode) = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-finalize")
            .unwrap()
            .unwrap();
        assert!(!inode.uses_extents());
        assert_eq!(inode.i_block, [0; 15]);
        assert_eq!(inode.size(), 0);
        assert_eq!(inode.blocks_count(BLOCK_SIZE as u32, true), 0);
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
    }

    /// Verifies symbolic-link resolution by reading the target through the link path.
    #[test]
    fn test_symbolic_link() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/symlinktest").expect("mkdir failed");

        let test_data = b"Data for symbolic link test";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/symlinktest/original",
            Some(test_data),
            None,
        )
        .expect("mkfile failed");

        create_symbol_link(
            &mut jbd2_dev,
            &mut fs,
            "/symlinktest/original",
            "/symlinktest/symlink",
        )
        .expect("create_symbol_link failed");

        // The symlink path should resolve to the target file data.
        let link_data =
            read_file(&mut jbd2_dev, &mut fs, "/symlinktest/symlink").expect("read_file failed");
        assert_eq!(link_data, test_data.to_vec());

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Documents current error semantics for missing paths, implicit parent
    /// creation, and deleting entries that are already gone.
    #[test]
    fn test_file_operation_errors() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        // Missing paths should return `ENOENT`.
        let non_existent = read_file(&mut jbd2_dev, &mut fs, "/nonexistent/file")
            .expect_err("missing file should fail");
        assert_eq!(non_existent.kind(), Ext4ErrorKind::NotFound);

        // The current implementation auto-creates parent directories for `mkfile`.
        let result = mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/nonexistent/file",
            Some(b"data"),
            None,
        );
        assert!(result.is_ok(), "mkfile should auto-create missing parents");

        // Deleting a path that is already absent is currently tolerated.
        delete_file(&mut fs, &mut jbd2_dev, "/nonexistent/file").expect("delete failed");

        // The path must still resolve as missing afterwards.
        let non_existent = read_file(&mut jbd2_dev, &mut fs, "/nonexistent/file")
            .expect_err("deleted file should fail");
        assert_eq!(non_existent.kind(), Ext4ErrorKind::NotFound);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }
}
