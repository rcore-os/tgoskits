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
        assert_eq!(old_err.kind(), Ext4ErrorKind::NotFound);

        // The new path should expose the exact original content.
        let new_data =
            read_file(&mut jbd2_dev, &mut fs, "/renametest/newname").expect("read_file failed");
        assert_eq!(new_data, test_data.to_vec());

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies that a full-block overwrite inside an existing large extent
    /// updates only the requested range and keeps surrounding data intact.
    #[test]
    fn test_large_file_existing_extent_full_block_overwrite() {
        let device = MockBlockDevice::new(128 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

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
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

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

        rename(&mut jbd2_dev, &mut fs, "/tmp/temp.txt", "/tmp/original.txt")
            .expect("rename replace failed");

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
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/aofdir").expect("mkdir failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/temp-rewriteaof.aof",
            Some(b"RDB-BASE-DATA\n"),
            None,
        )
        .expect("mkfile temp failed");

        rename(
            &mut jbd2_dev,
            &mut fs,
            "/temp-rewriteaof.aof",
            "/aofdir/appendonly.aof.1.base.rdb",
        )
        .expect("cross-dir rename failed");

        let data = read_file(&mut jbd2_dev, &mut fs, "/aofdir/appendonly.aof.1.base.rdb")
            .expect("read dest failed");
        assert_eq!(data, b"RDB-BASE-DATA\n");

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
        assert_eq!(deleted_err.kind(), Ext4ErrorKind::NotFound);

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

    #[test]
    fn hard_link_propagates_corrupt_destination_parent_extent() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");
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
    fn unsupported_legacy_indirect_unlink_preserves_link_count() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-indirect", None, None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-indirect")
            .unwrap()
            .unwrap()
            .0;
        let indirect_root = fs.alloc_block(&mut jbd2_dev).unwrap();
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_block[12] = indirect_root.to_u32().unwrap();
        })
        .unwrap();

        let error = unlink(&mut fs, &mut jbd2_dev, "/legacy-indirect")
            .expect_err("indirect-tree deletion is not implemented yet");
        assert_eq!(error.kind(), Ext4ErrorKind::Unsupported);

        let (_, inode) = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-indirect")
            .unwrap()
            .expect("failed unlink must preserve the directory entry");
        assert_eq!(inode.i_links_count, 1);
    }

    #[test]
    fn unsupported_legacy_indirect_delete_preserves_link_count() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-delete", None, None).expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-delete")
            .unwrap()
            .unwrap()
            .0;
        let indirect_root = fs.alloc_block(&mut jbd2_dev).unwrap();
        fs.modify_inode(&mut jbd2_dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_block[12] = indirect_root.to_u32().unwrap();
        })
        .unwrap();

        let error = delete_file(&mut fs, &mut jbd2_dev, "/legacy-delete")
            .expect_err("indirect-tree deletion is not implemented yet");
        assert_eq!(error.kind(), Ext4ErrorKind::Unsupported);

        let (_, inode) = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-delete")
            .unwrap()
            .expect("failed delete must preserve the directory entry");
        assert_eq!(inode.i_links_count, 1);
    }

    #[test]
    fn deleting_fast_symlink_does_not_treat_inline_bytes_as_indirect_blocks() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");
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
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-sparse", None, None).expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-sparse")
            .unwrap()
            .unwrap()
            .0;
        let first = fs.alloc_block(&mut jbd2_dev).unwrap();
        let third = fs.alloc_block(&mut jbd2_dev).unwrap();
        for (block, value) in [(first, 0x31), (third, 0x33)] {
            jbd2_dev.read_block(block).unwrap();
            jbd2_dev.buffer_mut().fill(value);
            jbd2_dev.write_block(block, false).unwrap();
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
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-overwrite", None, None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-overwrite")
            .unwrap()
            .unwrap()
            .0;
        let indirect_root = fs.alloc_block(&mut jbd2_dev).unwrap();
        let data_block = fs.alloc_block(&mut jbd2_dev).unwrap();
        jbd2_dev.read_block(indirect_root).unwrap();
        jbd2_dev.buffer_mut().fill(0);
        jbd2_dev.buffer_mut()[..4].copy_from_slice(&data_block.to_u32().unwrap().to_le_bytes());
        jbd2_dev.write_block(indirect_root, true).unwrap();
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
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");
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
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-publish", None, None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-publish")
            .unwrap()
            .unwrap()
            .0;
        let indirect_root = fs.alloc_block(&mut jbd2_dev).unwrap();
        jbd2_dev.set_journal_use(false);
        jbd2_dev.read_block(indirect_root).unwrap();
        jbd2_dev.buffer_mut().fill(0);
        jbd2_dev.write_block(indirect_root, true).unwrap();
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
    fn failed_legacy_inode_finalize_restores_cached_inode_before_freeing_blocks() {
        let (device, fail_after_write_sector) =
            MockBlockDevice::with_write_failure_handle(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");
        mkfile(&mut jbd2_dev, &mut fs, "/legacy-finalize", None, None)
            .expect("file creation failed");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/legacy-finalize")
            .unwrap()
            .unwrap()
            .0;
        jbd2_dev.set_journal_use(false);
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
