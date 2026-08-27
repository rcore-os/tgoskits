//! Functional tests for directory-oriented operations.
//!
//! The suite emphasizes tree creation, lookup, deletion semantics, and the
//! current behavior around implicit parent creation.

use std::cell::Cell;

use rsext4::{
    checksum::{update_ext4_dirblock_csum32, verify_ext4_dirblock_checksum},
    dir::get_inode_with_num,
    disknode::Ext4Inode,
    error::{Ext4Error, Ext4Result},
    loopfile::resolve_inode_block,
    *,
};

fn test_mkdir<B: BlockIo + rsext4::Clock>(
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

impl BlockIo for MockBlockDevice {
    fn read(&mut self, buffer: &mut [u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
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
        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
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
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

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

    #[test]
    fn indexed_directory_link_count_uses_dir_nlink_sentinel_at_linux_limit() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        let root = fs.root_inode;
        let mut root_inode = fs
            .get_inode_by_num(&mut jbd2_dev, root)
            .expect("load root inode");
        root_inode.i_flags |= Ext4Inode::EXT4_INDEX_FL;
        root_inode.i_links_count = 65_000;
        assert_eq!(root_inode.incremented_links_count(true).unwrap(), 1);
        root_inode.i_links_count = 1;
        assert_eq!(root_inode.incremented_links_count(true).unwrap(), 1);
    }

    #[test]
    fn directory_link_limit_without_dir_nlink_fails_before_allocation() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        fs.superblock.s_feature_ro_compat &=
            !rsext4::superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_DIR_NLINK;
        let root = fs.root_inode;
        fs.modify_inode(&mut jbd2_dev, root, |inode| {
            inode.i_flags |= Ext4Inode::EXT4_INDEX_FL;
            inode.i_links_count = Ext4Inode::EXT4_LINK_MAX;
        })
        .expect("prepare root at the Linux link limit");
        let free_inodes_before = fs.superblock.s_free_inodes_count;
        let free_blocks_before = fs.superblock.free_blocks_count();

        let error = mkdir(&mut jbd2_dev, &mut fs, "/must-not-allocate").unwrap_err();

        assert_eq!(error.kind(), rsext4::Ext4ErrorKind::TooManyLinks);
        assert_eq!(fs.superblock.s_free_inodes_count, free_inodes_before);
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
        assert!(
            rsext4::dir::get_inode_with_num(&mut fs, &mut jbd2_dev, "/must-not-allocate")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn failed_directory_allocation_rolls_back_child_and_parent_accounting() {
        let device = MockBlockDevice::new(64 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        // Maximum-length records force the root through Linux's one-block
        // linear-to-HTree conversion and then fill the selected leaf.
        for index in 0..30 {
            let name = format!("{index:03}{}", "a".repeat(252));
            mkfile(&mut jbd2_dev, &mut fs, &format!("/{name}"), None, None)
                .expect("fill root directory");
        }
        let root = fs.root_inode;
        let root_before = fs
            .get_inode_by_num(&mut jbd2_dev, root)
            .expect("read root before failed mkdir");
        assert!(root_before.size() >= 3 * fs.superblock.block_size());
        assert_ne!(root_before.i_flags & Ext4Inode::EXT4_INDEX_FL, 0);

        while fs.superblock.free_blocks_count() > 0 {
            fs.alloc_block(&mut jbd2_dev)
                .expect("reserve data block before rollback probe");
        }
        let free_inodes_before = fs.superblock.s_free_inodes_count;
        let free_blocks_before = fs.superblock.free_blocks_count();
        let used_dirs_before: u32 = fs
            .group_descs
            .iter()
            .map(|descriptor| descriptor.used_dirs_count())
            .sum();

        let child = "z".repeat(255);
        let error = mkdir(&mut jbd2_dev, &mut fs, &format!("/{child}"))
            .expect_err("child directory allocation must run out of space");
        assert_eq!(error.kind(), Ext4ErrorKind::NoSpace);

        let root_after = fs
            .get_inode_by_num(&mut jbd2_dev, root)
            .expect("read root after failed mkdir");
        let used_dirs_after: u32 = fs
            .group_descs
            .iter()
            .map(|descriptor| descriptor.used_dirs_count())
            .sum();
        assert_eq!(root_after.i_links_count, root_before.i_links_count);
        assert_eq!(root_after.size(), root_before.size());
        assert_eq!(root_after.i_blocks_lo, root_before.i_blocks_lo);
        assert_eq!(root_after.l_i_blocks_high, root_before.l_i_blocks_high);
        assert_eq!(root_after.i_flags, root_before.i_flags);
        assert_eq!(root_after.l_i_version, root_before.l_i_version);
        assert_eq!(root_after.i_version_hi, root_before.i_version_hi);
        assert_eq!(used_dirs_after, used_dirs_before);
        assert_eq!(fs.superblock.s_free_inodes_count, free_inodes_before);
        assert_eq!(fs.superblock.free_blocks_count(), free_blocks_before);
        assert!(
            get_inode_with_num(&mut fs, &mut jbd2_dev, &format!("/{child}"))
                .expect("lookup after failed mkdir")
                .is_none()
        );
    }

    #[test]
    fn created_directory_block_checksum_matches_persisted_inode_generation() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        let (dir_ino, mut dir_inode) = get_inode_with_num(&mut fs, &mut jbd2_dev, "/checksum-dir")
            .expect("lookup should succeed")
            .unwrap_or_else(|| {
                mkdir(&mut jbd2_dev, &mut fs, "/checksum-dir").expect("mkdir failed");
                get_inode_with_num(&mut fs, &mut jbd2_dev, "/checksum-dir")
                    .expect("lookup after mkdir should succeed")
                    .expect("created directory should exist")
            });
        let dir_block = resolve_inode_block(&fs, &mut jbd2_dev, dir_ino, &mut dir_inode, 0)
            .expect("resolve directory block failed")
            .expect("directory should have a first block");
        let cached = fs
            .datablock_cache
            .get_or_load(&mut jbd2_dev, dir_block)
            .expect("load directory block failed");

        assert!(
            verify_ext4_dirblock_checksum(
                &fs.superblock,
                dir_ino.raw(),
                dir_inode.i_generation,
                &cached.data
            ),
            "new directory checksum must use the inode generation persisted on disk"
        );

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    #[test]
    fn empty_directory_rejects_dot_entry_for_another_inode() {
        let device = MockBlockDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        mkdir(&mut jbd2_dev, &mut fs, "/victim").expect("mkdir failed");

        let (inode_num, mut inode) = get_inode_with_num(&mut fs, &mut jbd2_dev, "/victim")
            .expect("lookup failed")
            .expect("victim directory missing");
        let block = resolve_inode_block(&fs, &mut jbd2_dev, inode_num, &mut inode, 0)
            .expect("resolve directory block")
            .expect("directory must have a first block");
        let superblock = fs.superblock;
        let generation = inode.i_generation;
        let wrong_inode = fs.root_inode.raw();
        assert_ne!(wrong_inode, inode_num.raw());
        fs.datablock_cache
            .modify(&mut jbd2_dev, block, |data| {
                data[..4].copy_from_slice(&wrong_inode.to_le_bytes());
                update_ext4_dirblock_csum32(&superblock, inode_num.raw(), generation, data);
            })
            .expect("corrupt dot entry");

        let error = is_dir_empty(&mut fs, &mut jbd2_dev, inode_num, &mut inode)
            .expect_err("a dot entry naming another inode is corruption");
        assert_eq!(error.kind(), Ext4ErrorKind::Corrupted);
    }

    /// Verifies empty-directory deletion and records the current behavior for
    /// recreating paths under a directory that was previously removed.
    #[test]
    fn test_directory_delete() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

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
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

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
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

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
        assert_eq!(not_found.kind(), Ext4ErrorKind::NotFound);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies recursive helper deletion and exact namespace errors.
    #[test]
    fn test_directory_error_handling() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        // Removing a missing directory should fail with `ENOENT`.
        let err = delete_dir(&mut fs, &mut jbd2_dev, "/definitely-missing")
            .expect_err("missing directory should fail");
        assert_eq!(err.kind(), Ext4ErrorKind::NotFound);

        // `delete_dir` is the recursive core helper, not the VFS rmdir entry.
        test_mkdir(&mut jbd2_dev, &mut fs, "/nonempty").expect("mkdir failed");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/nonempty/file.txt",
            Some(b"data"),
            None,
        )
        .expect("mkfile failed");

        delete_dir(&mut fs, &mut jbd2_dev, "/nonempty")
            .expect("recursive helper must remove a non-empty tree");
        let error = read_file(&mut jbd2_dev, &mut fs, "/nonempty/file.txt")
            .expect_err("removed tree must be unreachable");
        assert_eq!(error.kind(), Ext4ErrorKind::NotFound);

        test_mkdir(&mut jbd2_dev, &mut fs, "/nonempty").expect("recreate directory");
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/nonempty/another_file.txt",
            Some(b"data"),
            None,
        )
        .expect("create below recreated directory");
        // Duplicate directory creation should still return `EEXIST`.
        test_mkdir(&mut jbd2_dev, &mut fs, "/duplicate").expect("mkdir failed");
        let result = test_mkdir(&mut jbd2_dev, &mut fs, "/duplicate");
        let err = result.expect_err("duplicate mkdir should fail");
        assert_eq!(err.kind(), Ext4ErrorKind::AlreadyExists);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Builds a larger tree that mixes user, system, and web-style paths to
    /// ensure traversal continues to work across a wider namespace.
    #[test]
    fn test_complex_directory_structure() {
        let device = MockBlockDevice::new(200 * 1024 * 1024); // 200MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

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
