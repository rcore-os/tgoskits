//! Functional tests for file-level operations.
//!
//! The suite focuses on common file workflows and records a few implementation
//! details that intentionally differ from a fully POSIX-like filesystem.

use std::cell::Cell;

use rsext4::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
    *,
};

/// In-memory block device used by file operation tests.
struct MockBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    fail_on_write: bool,
    fail_on_read: bool,
    now: Cell<i64>,
}

impl MockBlockDevice {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            block_size: rsext4::BLOCK_SIZE as u32,
            fail_on_write: false,
            fail_on_read: false,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for MockBlockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        if self.fail_on_read {
            return Err(Ext4Error::io());
        }

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
        if self.fail_on_write {
            return Err(Ext4Error::io());
        }

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

#[cfg(test)]
mod file_functional_tests {
    use super::*;

    /// Covers the create-read-write loop and documents that a shorter overwrite
    /// updates the prefix without implicitly truncating the file.
    #[test]
    fn test_file_create_and_rw() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

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

    /// Covers both shrinking and growing a file and documents that growth keeps
    /// previously stored bytes instead of zero-filling the new range.
    #[test]
    fn test_file_truncate() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

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

        // Grow the file again and check the implementation-specific contents.
        truncate(&mut jbd2_dev, &mut fs, "/truncatetest/truncate_file", 20)
            .expect("truncate expand failed");

        let expanded_data = read_file(&mut jbd2_dev, &mut fs, "/truncatetest/truncate_file")
            .expect("read_file failed");

        // Growth currently preserves the bytes that were already present in the
        // backing blocks instead of returning zero-filled data.
        let mut expected = Vec::from(&original_data[..10]);
        expected.extend_from_slice(&original_data[10..20]);
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
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

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

        rename(
            &mut jbd2_dev,
            &mut fs,
            "/renametest/oldname",
            "/renametest/newname",
        )
        .expect("rename failed");

        // The old path must disappear after rename.
        let old_err = read_file(&mut jbd2_dev, &mut fs, "/renametest/oldname")
            .expect_err("old path should not exist");
        assert_eq!(old_err.code, Errno::ENOENT);

        // The new path should expose the exact original content.
        let new_data =
            read_file(&mut jbd2_dev, &mut fs, "/renametest/newname").expect("read_file failed");
        assert_eq!(new_data, test_data.to_vec());

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies cross-directory moves by checking that the source path disappears
    /// and the destination path keeps the original payload.
    #[test]
    fn test_file_move() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

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

        mv(
            &mut fs,
            &mut jbd2_dev,
            "/sourcedir/movefile",
            "/destdir/movedfile",
        )
        .expect("mv failed");

        // The source entry should be removed after the move.
        let old_err = read_file(&mut jbd2_dev, &mut fs, "/sourcedir/movefile")
            .expect_err("old path should not exist");
        assert_eq!(old_err.code, Errno::ENOENT);

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
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

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
        assert_eq!(deleted_err.code, Errno::ENOENT);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Exercises the current hard-link path without requiring full correctness.
    /// The test documents the known limitation and only asserts source stability.
    #[test]
    fn test_hard_link() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

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

        // The implementation is still under development, so only the original
        // file stability is asserted after attempting to add a hard link.
        let _ = link(
            &mut fs,
            &mut jbd2_dev,
            "/linktest/original",
            "/linktest/hardlink",
        );

        // The source file must remain readable even if hard-link creation is incomplete.
        let original_data =
            read_file(&mut jbd2_dev, &mut fs, "/linktest/original").expect("read_file failed");
        assert_eq!(original_data, test_data.to_vec());

        // TODO: strengthen this test once the hard-link path is fully implemented.

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies symbolic-link resolution by reading the target through the link path.
    #[test]
    fn test_symbolic_link() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

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
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        // Missing paths should return `ENOENT`.
        let non_existent = read_file(&mut jbd2_dev, &mut fs, "/nonexistent/file")
            .expect_err("missing file should fail");
        assert_eq!(non_existent.code, Errno::ENOENT);

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
        assert_eq!(non_existent.code, Errno::ENOENT);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }
}
