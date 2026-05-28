use alloc::{sync::Arc, vec::Vec};

use ax_errno::LinuxError;
use axfs_ng_vfs::{Filesystem, NodePermission, VfsError, VfsResult};

use super::{DirMaker, DirMapping, DirectRwFsFileOps, SimpleDir, SimpleFs, SpecialFsFile};

const CGROUP2_SUPER_MAGIC: u32 = 0x6367_7270;

enum RootFile {
    Procs,
    Controllers,
    SubtreeControl,
}

impl RootFile {
    fn read_content(&self) -> Vec<u8> {
        match self {
            Self::Procs => crate::cgroup::root_procs_text().into_bytes(),
            Self::Controllers => crate::cgroup::root_controllers_text().as_bytes().to_vec(),
            Self::SubtreeControl => crate::cgroup::root_subtree_control_text()
                .as_bytes()
                .to_vec(),
        }
    }
}

impl DirectRwFsFileOps for RootFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let content = self.read_content();
        let offset = offset as usize;
        if offset >= content.len() {
            return Ok(0);
        }

        let content = &content[offset..];
        let read = content.len().min(buf.len());
        buf[..read].copy_from_slice(&content[..read]);
        Ok(read)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        match self {
            Self::Procs => crate::cgroup::write_root_procs(buf)?,
            Self::Controllers => return Err(VfsError::from(LinuxError::EACCES)),
            Self::SubtreeControl => crate::cgroup::write_root_subtree_control(buf)?,
        }
        Ok(buf.len())
    }
}

/// Creates a minimal cgroup v2 pseudo filesystem.
pub(crate) fn new_cgroup2fs() -> Filesystem {
    SimpleFs::new_with("cgroup2".into(), CGROUP2_SUPER_MAGIC, cgroup2fs_builder)
}

fn cgroup2fs_builder(fs: Arc<SimpleFs>) -> DirMaker {
    let mut root = DirMapping::new();
    root.add(
        "cgroup.procs",
        SpecialFsFile::new_regular_with_perm(
            fs.clone(),
            RootFile::Procs,
            NodePermission::from_bits_truncate(0o644),
        ),
    );
    root.add(
        "cgroup.controllers",
        SpecialFsFile::new_regular_with_perm(
            fs.clone(),
            RootFile::Controllers,
            NodePermission::from_bits_truncate(0o444),
        ),
    );
    root.add(
        "cgroup.subtree_control",
        SpecialFsFile::new_regular_with_perm(
            fs.clone(),
            RootFile::SubtreeControl,
            NodePermission::from_bits_truncate(0o644),
        ),
    );
    SimpleDir::new_maker(fs, Arc::new(root))
}
