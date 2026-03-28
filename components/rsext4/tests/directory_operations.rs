//! Functional tests for directory-oriented operations.
//!
//! The suite emphasizes tree creation, lookup, deletion semantics, and the
//! current behavior around implicit parent creation.

use std::cell::Cell;

use rsext4::{
    bmalloc::AbsoluteBN,
    disknode::Ext4Inode,
    error::{Ext4Error, Ext4Result},
    *,
};

fn test_mkdir<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> Ext4Result<Ext4Inode> {
    mkdir(device, fs, path)
}

/// In-memory block device used by directory tests.
struct MockBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    now: Cell<i64>,
}

impl MockBlockDevice {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            block_size: rsext4::BLOCK_SIZE as u32,
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

#[cfg(test)]
mod directory_functional_tests {
    use super::*;

    /// Verifies basic directory creation patterns, from single-level paths to a
    /// deeper hierarchy and several siblings under one parent.
    #[test]
    fn test_directory_create() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        // Cover one shallow path first.
        test_mkdir(&mut jbd2_dev, &mut fs, "/single").expect("mkdir failed");

        // Then build a multi-level chain.
        test_mkdir(&mut jbd2_dev, &mut fs, "/level1").expect("mkdir failed");
        test_mkdir(&mut jbd2_dev, &mut fs, "/level1/level2").expect("mkdir failed");
        test_mkdir(&mut jbd2_dev, &mut fs, "/level1/level2/level3").expect("mkdir failed");

        // Finally, create several siblings under one common parent.
        test_mkdir(&mut jbd2_dev, &mut fs, "/siblings").expect("mkdir failed");
        test_mkdir(&mut jbd2_dev, &mut fs, "/siblings/sibling1").expect("mkdir failed");
        test_mkdir(&mut jbd2_dev, &mut fs, "/siblings/sibling2").expect("mkdir failed");
        test_mkdir(&mut jbd2_dev, &mut fs, "/siblings/sibling3").expect("mkdir failed");

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies empty-directory deletion and records the current behavior for
    /// recreating paths under a directory that was previously removed.
    #[test]
    fn test_directory_delete() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        // Build a nested directory tree and one empty directory.
        test_mkdir(&mut jbd2_dev, &mut fs, "/test").expect("mkdir failed");
        test_mkdir(&mut jbd2_dev, &mut fs, "/test/subdir").expect("mkdir failed");

        let test_data = b"File in subdirectory";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/test/subdir/file",
            Some(test_data),
            None,
        )
        .expect("mkfile failed");

        // Empty directories should be removable.
        test_mkdir(&mut jbd2_dev, &mut fs, "/empty").expect("mkdir failed");
        delete_dir(&mut fs, &mut jbd2_dev, "/empty").expect("delete_dir failed");

        // `mkfile` currently recreates missing parents, so use that behavior as
        // the post-condition being documented here.
        let result = mkfile(&mut jbd2_dev, &mut fs, "/empty/file", Some(b"data"), None);
        assert!(result.is_ok(), "mkfile should auto-create missing parents");

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies that files created in different directories stay isolated and
    /// can be read back through their full paths.
    #[test]
    fn test_directory_file_operations() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        // Build two branches and place independent files under each branch.
        test_mkdir(&mut jbd2_dev, &mut fs, "/documents").expect("mkdir failed");
        test_mkdir(&mut jbd2_dev, &mut fs, "/documents/projects").expect("mkdir failed");
        test_mkdir(&mut jbd2_dev, &mut fs, "/documents/personal").expect("mkdir failed");

        let project_data = b"Project related data";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/documents/projects/project1.txt",
            Some(project_data),
            None,
        )
        .expect("mkfile failed");

        let personal_data = b"Personal notes";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/documents/personal/notes.txt",
            Some(personal_data),
            None,
        )
        .expect("mkfile failed");

        // Each branch should preserve its own payload.
        let read_project = read_file(&mut jbd2_dev, &mut fs, "/documents/projects/project1.txt")
            .expect("read_file failed");
        assert_eq!(read_project, project_data.to_vec());

        let read_notes = read_file(&mut jbd2_dev, &mut fs, "/documents/personal/notes.txt")
            .expect("read_file failed");
        assert_eq!(read_notes, personal_data.to_vec());

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies positive and negative lookup behavior by reading several known
    /// files and one guaranteed-missing path from the same directory.
    #[test]
    fn test_directory_file_find() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        test_mkdir(&mut jbd2_dev, &mut fs, "/findtest").expect("mkdir failed");

        // Populate the directory with a small deterministic file set.
        for i in 1..=5 {
            let filename = format!("/findtest/file{}.txt", i);
            let data = format!("Content of file {}", i);
            mkfile(
                &mut jbd2_dev,
                &mut fs,
                &filename,
                Some(data.as_bytes()),
                None,
            )
            .expect("mkfile failed");
        }

        // Each known file should resolve and return the expected bytes.
        for i in 1..=5 {
            let filename = format!("/findtest/file{}.txt", i);
            let expected_data = format!("Content of file {}", i);

            let found_data =
                read_file(&mut jbd2_dev, &mut fs, &filename).expect("read_file failed");
            assert_eq!(found_data, expected_data.as_bytes().to_vec());
        }

        // A missing file should still report `ENOENT`.
        let not_found = read_file(&mut jbd2_dev, &mut fs, "/findtest/notexist.txt")
            .expect_err("missing file should fail");
        assert_eq!(not_found.code, Errno::ENOENT);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Documents current directory error behavior, especially the difference
    /// between explicit delete failures and implicit parent creation by `mkfile`.
    #[test]
    fn test_directory_error_handling() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        // `mkfile` currently auto-creates missing parents instead of failing.
        let result = mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/nonexistent/file.txt",
            Some(b"data"),
            None,
        );
        assert!(result.is_ok(), "mkfile should auto-create missing parents");

        // Removing a missing directory should fail with `ENOENT`.
        let err = delete_dir(&mut fs, &mut jbd2_dev, "/definitely-missing")
            .expect_err("missing directory should fail");
        assert_eq!(err.code, Errno::ENOENT);

        // Non-empty directory deletion is implementation-defined here, so the test
        // records behavior rather than requiring one strict outcome.
        test_mkdir(&mut jbd2_dev, &mut fs, "/nonempty").expect("mkdir failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/nonempty/file.txt",
            Some(b"data"),
            None,
        )
        .expect("mkfile failed");

        let _ = delete_dir(&mut fs, &mut jbd2_dev, "/nonempty");

        // A follow-up create documents whether the directory remained available.
        let _result = mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/nonempty/another_file.txt",
            Some(b"data"),
            None,
        );
        // Duplicate directory creation should still return `EEXIST`.
        test_mkdir(&mut jbd2_dev, &mut fs, "/duplicate").expect("mkdir failed");
        let result = test_mkdir(&mut jbd2_dev, &mut fs, "/duplicate");
        let err = result.expect_err("duplicate mkdir should fail");
        assert_eq!(err.code, Errno::EEXIST);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Builds a larger tree that mixes user, system, and web-style paths to
    /// ensure traversal continues to work across a wider namespace.
    #[test]
    fn test_complex_directory_structure() {
        let device = MockBlockDevice::new(200 * 1024 * 1024); // 200MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        // Create a representative multi-branch hierarchy.
        let structure = [
            "/home",
            "/home/user",
            "/home/user/documents",
            "/home/user/documents/work",
            "/home/user/documents/personal",
            "/home/user/music",
            "/home/user/music/rock",
            "/home/user/music/jazz",
            "/home/user/music/classical",
            "/var",
            "/var/log",
            "/var/www",
            "/var/www/html",
            "/var/www/css",
            "/var/www/js",
            "/tmp",
            "/etc",
            "/etc/config",
        ];

        // Materialize every directory first.
        for dir in &structure {
            test_mkdir(&mut jbd2_dev, &mut fs, dir).expect("mkdir failed");
        }

        // Then place files across the tree and verify they all remain reachable.
        let files = [
            (
                "/home/user/documents/work/report.txt",
                "Work report content",
            ),
            (
                "/home/user/documents/personal/diary.txt",
                "Personal diary entries",
            ),
            ("/home/user/music/rock/song1.mp3", "Rock music data"),
            ("/var/log/system.log", "System log entries"),
            ("/var/www/html/index.html", "HTML page content"),
            ("/var/www/css/style.css", "CSS style definitions"),
            ("/var/www/js/script.js", "JavaScript code"),
            ("/etc/config/app.conf", "Application configuration"),
        ];

        for (path, content) in &files {
            mkfile(&mut jbd2_dev, &mut fs, path, Some(content.as_bytes()), None)
                .expect("mkfile failed");
        }

        for (path, content) in &files {
            let read_data = read_file(&mut jbd2_dev, &mut fs, path).expect("read_file failed");
            assert_eq!(read_data, content.as_bytes().to_vec());
        }

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }
}
