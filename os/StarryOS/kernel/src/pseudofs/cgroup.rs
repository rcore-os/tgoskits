use alloc::{string::ToString, sync::Arc, vec::Vec};
use core::any::Any;

use ax_errno::LinuxError;
use axfs_ng_vfs::{
    DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, Filesystem, FilesystemOps, Metadata,
    MetadataUpdate, NodeOps, NodePermission, NodeType, Reference, VfsError, VfsResult,
    WeakDirEntry,
    path::{DOT, DOTDOT},
};
use inherit_methods_macro::inherit_methods;

use super::{DirMaker, DirectRwFsFileOps, SimpleFs, SimpleFsNode, SpecialFsFile};
use crate::cgroup::{CgroupId, root_id};

const CGROUP2_SUPER_MAGIC: u32 = 0x6367_7270;

#[derive(Clone, Copy)]
enum CgroupFileKind {
    Controllers,
    Procs,
    SubtreeControl,
}

impl CgroupFileKind {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "cgroup.controllers" => Some(Self::Controllers),
            "cgroup.procs" => Some(Self::Procs),
            "cgroup.subtree_control" => Some(Self::SubtreeControl),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Controllers => "cgroup.controllers",
            Self::Procs => "cgroup.procs",
            Self::SubtreeControl => "cgroup.subtree_control",
        }
    }

    fn permission(self) -> NodePermission {
        let mode = match self {
            Self::Controllers => 0o444,
            Self::Procs | Self::SubtreeControl => 0o644,
        };
        NodePermission::from_bits_truncate(mode)
    }
}

const CGROUP_FILES: [CgroupFileKind; 3] = [
    CgroupFileKind::Controllers,
    CgroupFileKind::Procs,
    CgroupFileKind::SubtreeControl,
];

struct CgroupFile {
    id: CgroupId,
    kind: CgroupFileKind,
}

impl CgroupFile {
    fn read_content(&self) -> VfsResult<Vec<u8>> {
        Ok(match self.kind {
            CgroupFileKind::Controllers => crate::cgroup::controllers_text(self.id)?
                .as_bytes()
                .to_vec(),
            CgroupFileKind::Procs => crate::cgroup::procs_text(self.id)?.into_bytes(),
            CgroupFileKind::SubtreeControl => crate::cgroup::subtree_control_text(self.id)?
                .as_bytes()
                .to_vec(),
        })
    }
}

impl DirectRwFsFileOps for CgroupFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let content = self.read_content()?;
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
        match self.kind {
            CgroupFileKind::Controllers => {
                crate::cgroup::ensure_node_exists(self.id)?;
                return Err(VfsError::from(LinuxError::EACCES));
            }
            CgroupFileKind::Procs => crate::cgroup::write_procs(self.id, buf)?,
            CgroupFileKind::SubtreeControl => crate::cgroup::write_subtree_control(self.id, buf)?,
        }
        Ok(buf.len())
    }
}

struct CgroupDir {
    node: SimpleFsNode,
    this: WeakDirEntry,
    fs: Arc<SimpleFs>,
    id: CgroupId,
}

impl CgroupDir {
    fn new(fs: Arc<SimpleFs>, id: CgroupId, this: WeakDirEntry) -> Arc<Self> {
        debug_assert!(crate::cgroup::path(id).is_ok());
        Arc::new(Self {
            node: SimpleFsNode::new(
                fs.clone(),
                NodeType::Directory,
                NodePermission::from_bits_truncate(0o755),
            ),
            this,
            fs,
            id,
        })
    }

    fn new_maker(fs: Arc<SimpleFs>, id: CgroupId) -> DirMaker {
        Arc::new(move |this| Self::new(fs.clone(), id, this))
    }

    fn this_entry(&self) -> VfsResult<DirEntry> {
        self.this.upgrade().ok_or(VfsError::NotFound)
    }

    fn file_entry(&self, kind: CgroupFileKind) -> VfsResult<DirEntry> {
        let file = SpecialFsFile::new_regular_with_perm(
            self.fs.clone(),
            CgroupFile { id: self.id, kind },
            kind.permission(),
        );
        let reference = Reference::new(self.this.upgrade(), kind.name().to_string());
        Ok(DirEntry::new_file(
            FileNode::new(file),
            NodeType::RegularFile,
            reference,
        ))
    }

    fn child_dir_entry(&self, name: &str, id: CgroupId) -> DirEntry {
        let maker = Self::new_maker(self.fs.clone(), id);
        let reference = Reference::new(self.this.upgrade(), name.to_string());
        DirEntry::new_dir(|this| DirNode::new(maker(this)), reference)
    }
}

#[inherit_methods(from = "self.node")]
impl NodeOps for CgroupDir {
    fn inode(&self) -> u64;

    fn metadata(&self) -> VfsResult<Metadata>;

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;

    fn filesystem(&self) -> &dyn FilesystemOps;

    fn sync(&self, data_only: bool) -> VfsResult<()>;

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl DirNodeOps for CgroupDir {
    fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let mut names = Vec::new();
        names.push(DOT.to_string());
        names.push(DOTDOT.to_string());
        for kind in CGROUP_FILES {
            names.push(kind.name().to_string());
        }
        names.extend(crate::cgroup::child_names(self.id)?);

        let this_entry = self.this_entry()?;
        let this_dir = this_entry.as_dir()?;
        let mut count = 0;
        for (i, name) in names.iter().enumerate().skip(offset as usize) {
            let metadata = match name.as_str() {
                DOT => this_entry.metadata(),
                DOTDOT => this_entry
                    .parent()
                    .map_or_else(|| this_entry.metadata(), |parent| parent.metadata()),
                other => this_dir.lookup(other)?.metadata(),
            }?;
            if !sink.accept(name, metadata.inode, metadata.node_type, i as u64 + 1) {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        if let Some(kind) = CgroupFileKind::from_name(name) {
            return self.file_entry(kind);
        }

        let child_id = crate::cgroup::lookup_child(self.id, name)?;
        Ok(self.child_dir_entry(name, child_id))
    }

    fn is_cacheable(&self) -> bool {
        false
    }

    fn has_children(&self) -> VfsResult<bool> {
        Ok(!crate::cgroup::child_names(self.id)?.is_empty())
    }

    fn create(
        &self,
        name: &str,
        node_type: NodeType,
        _permission: NodePermission,
        _uid: u32,
        _gid: u32,
    ) -> VfsResult<DirEntry> {
        if crate::cgroup::is_interface_file_name(name) {
            return Err(VfsError::AlreadyExists);
        }
        if node_type != NodeType::Directory {
            return Err(VfsError::OperationNotPermitted);
        }

        let child_id = crate::cgroup::create_child(self.id, name)?;
        Ok(self.child_dir_entry(name, child_id))
    }

    fn link(&self, _name: &str, _node: &DirEntry) -> VfsResult<DirEntry> {
        Err(VfsError::OperationNotPermitted)
    }

    fn unlink(&self, name: &str, _is_dir: bool) -> VfsResult<()> {
        if crate::cgroup::is_interface_file_name(name) {
            return Err(VfsError::OperationNotPermitted);
        }
        crate::cgroup::remove_child(self.id, name)
    }

    fn rename(&self, _src_name: &str, _dst_dir: &DirNode, _dst_name: &str) -> VfsResult<()> {
        Err(VfsError::OperationNotPermitted)
    }
}

/// Creates a cgroup v2 pseudo filesystem backed by the global cgroup hierarchy.
pub(crate) fn new_cgroup2fs() -> Filesystem {
    SimpleFs::new_with("cgroup2".into(), CGROUP2_SUPER_MAGIC, cgroup2fs_builder)
}

fn cgroup2fs_builder(fs: Arc<SimpleFs>) -> DirMaker {
    CgroupDir::new_maker(fs, root_id())
}
