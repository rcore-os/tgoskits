use alloc::{string::ToString, sync::Arc, vec::Vec};
use core::any::Any;

use ax_cgroup::{CgroupError, CgroupNode};
use axfs_ng_vfs::{
    DirEntry, DirEntrySink, DirNode, DirNodeOps, DirectoryCursor, FileNode, Filesystem,
    FilesystemOps, Location, Metadata, MetadataUpdate, NodeOps, NodePermission, NodeType,
    Reference, RenameOptions, VfsError, VfsResult, WeakDirEntry,
    path::{DOT, DOTDOT},
};
use inherit_methods_macro::inherit_methods;

use super::{DirMaker, DirectRwFsFileOps, SimpleFs, SimpleFsNode, SpecialFsFile};

const CGROUP2_SUPER_MAGIC: u32 = 0x6367_7270;

#[derive(Clone, Copy)]
enum CgroupFileKind {
    Controllers,
    Procs,
    SubtreeControl,
    PidsMax,
    PidsCurrent,
    PidsPeak,
    PidsEvents,
}

impl CgroupFileKind {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "cgroup.controllers" => Some(Self::Controllers),
            "cgroup.procs" => Some(Self::Procs),
            "cgroup.subtree_control" => Some(Self::SubtreeControl),
            "pids.max" => Some(Self::PidsMax),
            "pids.current" => Some(Self::PidsCurrent),
            "pids.peak" => Some(Self::PidsPeak),
            "pids.events" => Some(Self::PidsEvents),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Controllers => "cgroup.controllers",
            Self::Procs => "cgroup.procs",
            Self::SubtreeControl => "cgroup.subtree_control",
            Self::PidsMax => "pids.max",
            Self::PidsCurrent => "pids.current",
            Self::PidsPeak => "pids.peak",
            Self::PidsEvents => "pids.events",
        }
    }

    fn permission(self) -> NodePermission {
        let mode = match self {
            Self::Controllers | Self::PidsCurrent | Self::PidsPeak | Self::PidsEvents => 0o444,
            Self::Procs | Self::SubtreeControl | Self::PidsMax => 0o644,
        };
        NodePermission::from_bits_truncate(mode)
    }

    fn is_available(self, cgroup: &CgroupNode) -> bool {
        !matches!(
            self,
            Self::PidsMax | Self::PidsCurrent | Self::PidsPeak | Self::PidsEvents
        ) || cgroup.has_pids_interface()
    }
}

const CGROUP_FILES: [CgroupFileKind; 7] = [
    CgroupFileKind::Controllers,
    CgroupFileKind::Procs,
    CgroupFileKind::SubtreeControl,
    CgroupFileKind::PidsMax,
    CgroupFileKind::PidsCurrent,
    CgroupFileKind::PidsPeak,
    CgroupFileKind::PidsEvents,
];

struct CgroupFile {
    cgroup: Arc<CgroupNode>,
    kind: CgroupFileKind,
}

impl CgroupFile {
    fn read_content(&self) -> VfsResult<Vec<u8>> {
        Ok(match self.kind {
            CgroupFileKind::Controllers => crate::cgroup::controllers_text(&self.cgroup)
                .as_bytes()
                .to_vec(),
            CgroupFileKind::Procs => crate::cgroup::procs_text(&self.cgroup).into_bytes(),
            CgroupFileKind::SubtreeControl => crate::cgroup::subtree_control_text(&self.cgroup)
                .as_bytes()
                .to_vec(),
            CgroupFileKind::PidsMax => crate::cgroup::pids_max_text(&self.cgroup)?.into_bytes(),
            CgroupFileKind::PidsCurrent => {
                crate::cgroup::pids_current_text(&self.cgroup)?.into_bytes()
            }
            CgroupFileKind::PidsPeak => crate::cgroup::pids_peak_text(&self.cgroup)?.into_bytes(),
            CgroupFileKind::PidsEvents => {
                crate::cgroup::pids_events_text(&self.cgroup)?.into_bytes()
            }
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
                return Err(VfsError::PermissionDenied);
            }
            CgroupFileKind::Procs => crate::cgroup::write_procs(self.cgroup.clone(), buf)?,
            CgroupFileKind::SubtreeControl => {
                crate::cgroup::write_subtree_control(&self.cgroup, buf)?
            }
            CgroupFileKind::PidsMax => crate::cgroup::write_pids_max(&self.cgroup, buf)?,
            CgroupFileKind::PidsCurrent | CgroupFileKind::PidsPeak | CgroupFileKind::PidsEvents => {
                return Err(VfsError::OperationNotPermitted);
            }
        }
        Ok(buf.len())
    }
}

struct CgroupDir {
    node: SimpleFsNode,
    this: WeakDirEntry,
    fs: Arc<SimpleFs>,
    cgroup: Arc<CgroupNode>,
}

impl CgroupDir {
    fn new(fs: Arc<SimpleFs>, cgroup: Arc<CgroupNode>, this: WeakDirEntry) -> Arc<Self> {
        Arc::new(Self {
            node: SimpleFsNode::new(
                fs.clone(),
                NodeType::Directory,
                NodePermission::from_bits_truncate(0o755),
            ),
            this,
            fs,
            cgroup,
        })
    }

    fn new_maker(fs: Arc<SimpleFs>, cgroup: Arc<CgroupNode>) -> DirMaker {
        Arc::new(move |this| Self::new(fs.clone(), cgroup.clone(), this))
    }

    fn this_entry(&self) -> VfsResult<DirEntry> {
        self.this.upgrade().ok_or(VfsError::NotFound)
    }

    fn file_entry(&self, kind: CgroupFileKind) -> VfsResult<DirEntry> {
        if !kind.is_available(&self.cgroup) {
            return Err(VfsError::NotFound);
        }
        let file = SpecialFsFile::new_regular_with_perm(
            self.fs.clone(),
            CgroupFile {
                cgroup: self.cgroup.clone(),
                kind,
            },
            kind.permission(),
        );
        let reference = Reference::new(self.this.upgrade(), kind.name().to_string());
        Ok(DirEntry::new_file(
            FileNode::new(file),
            NodeType::RegularFile,
            reference,
        ))
    }

    fn child_dir_entry(&self, name: &str, cgroup: Arc<CgroupNode>) -> DirEntry {
        let maker = Self::new_maker(self.fs.clone(), cgroup);
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
    fn read_dir(&self, cursor: DirectoryCursor, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let mut names = Vec::new();
        names.push(DOT.to_string());
        names.push(DOTDOT.to_string());
        for kind in CGROUP_FILES {
            if kind.is_available(&self.cgroup) {
                names.push(kind.name().to_string());
            }
        }
        names.extend(self.cgroup.child_names());

        let this_entry = self.this_entry()?;
        let this_dir = this_entry.as_dir()?;
        let mut count = 0;
        for (i, name) in names.iter().enumerate().skip(cursor.offset() as usize) {
            let metadata = match name.as_str() {
                DOT => this_entry.metadata(),
                DOTDOT => this_entry
                    .parent()
                    .map_or_else(|| this_entry.metadata(), |parent| parent.metadata()),
                other => this_dir.lookup(other)?.metadata(),
            }?;
            if !sink.accept(
                name.as_bytes(),
                metadata.inode,
                metadata.node_type,
                DirectoryCursor::new(i as u64 + 1),
            ) {
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

        let child = self
            .cgroup
            .lookup_child(name)
            .map_err(cgroup_error_to_vfs_error)?;
        Ok(self.child_dir_entry(name, child))
    }

    fn is_cacheable(&self) -> bool {
        false
    }

    fn has_children(&self) -> VfsResult<bool> {
        Ok(!self.cgroup.child_names().is_empty())
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

        let child = self
            .cgroup
            .create_child(name)
            .map_err(cgroup_error_to_vfs_error)?;
        Ok(self.child_dir_entry(name, child))
    }

    fn create_symlink(
        &self,
        _name: &str,
        _target: &str,
        _permission: NodePermission,
        _uid: u32,
        _gid: u32,
    ) -> VfsResult<DirEntry> {
        Err(VfsError::OperationNotPermitted)
    }

    fn link(&self, _name: &str, _node: &DirEntry) -> VfsResult<DirEntry> {
        Err(VfsError::OperationNotPermitted)
    }

    fn unlink(&self, name: &str, _is_dir: bool) -> VfsResult<()> {
        if crate::cgroup::is_interface_file_name(name) {
            return Err(VfsError::OperationNotPermitted);
        }
        self.cgroup
            .remove_child(name)
            .map_err(cgroup_error_to_vfs_error)
    }

    fn rename(
        &self,
        _src_name: &str,
        _dst_dir: &DirNode,
        _dst_name: &str,
        _options: RenameOptions,
    ) -> VfsResult<()> {
        Err(VfsError::OperationNotPermitted)
    }
}

fn cgroup_error_to_vfs_error(error: CgroupError) -> VfsError {
    match error {
        CgroupError::NotInitialized | CgroupError::InvalidInput => VfsError::InvalidInput,
        CgroupError::NotFound => VfsError::NotFound,
        CgroupError::AlreadyExists => VfsError::AlreadyExists,
        CgroupError::ResourceBusy => VfsError::ResourceBusy,
        CgroupError::LimitExceeded => VfsError::WouldBlock,
        CgroupError::NoSuchProcess => VfsError::NotFound,
        CgroupError::DirectoryNotEmpty => VfsError::DirectoryNotEmpty,
    }
}

/// Create a cgroup2 filesystem rooted at a stable namespace snapshot.
pub(crate) fn new_cgroup2fs(root: Arc<CgroupNode>) -> Filesystem {
    SimpleFs::new_with("cgroup2".into(), CGROUP2_SUPER_MAGIC, move |fs| {
        CgroupDir::new_maker(fs, root)
    })
}

/// Return the cgroup node represented by an open cgroup2 directory.
pub(crate) fn node_from_location(location: &Location) -> Option<Arc<CgroupNode>> {
    location
        .entry()
        .downcast::<CgroupDir>()
        .ok()
        .map(|directory| directory.cgroup.clone())
}
