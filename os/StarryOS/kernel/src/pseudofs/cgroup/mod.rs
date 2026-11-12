use alloc::{string::ToString, sync::Arc, vec::Vec};
use core::any::Any;

use ax_cgroup::{
    CgroupError, CgroupFileKind, CgroupFsAttributes, CgroupFsTarget, CgroupFsUpdate, CgroupNode,
};
use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, DirectoryCursor, FileNode, Filesystem,
    FilesystemOps, Location, Metadata, MetadataRequest, MetadataUpdate, MutationRetire,
    NamespaceCompletion, NamespaceEffect, NamespaceMutation, NamespaceOutcome, NodeIdentity,
    NodeOps, NodePermission, NodeType, Reference, VfsError, VfsResult, WeakDirEntry,
    path::{DOT, DOTDOT},
};

use self::file::CgroupFile;
use super::{DirMaker, SimpleFs};

mod file;
mod metadata;
mod mutation;

const CGROUP2_SUPER_MAGIC: u32 = 0x6367_7270;

struct CgroupDir {
    this: WeakDirEntry,
    fs: Arc<SimpleFs>,
    cgroup: Arc<CgroupNode>,
}

impl CgroupDir {
    fn new(fs: Arc<SimpleFs>, cgroup: Arc<CgroupNode>, this: WeakDirEntry) -> Arc<Self> {
        Arc::new(Self { this, fs, cgroup })
    }

    fn new_maker(fs: Arc<SimpleFs>, cgroup: Arc<CgroupNode>) -> DirMaker {
        Arc::new(move |this| Self::new(fs.clone(), cgroup.clone(), this))
    }

    fn this_entry(&self) -> VfsResult<DirEntry> {
        self.this.upgrade().ok_or(VfsError::NotFound)
    }

    fn file_entry(&self, parent: &DirEntry, kind: CgroupFileKind) -> DirEntry {
        let file = Arc::new(CgroupFile {
            fs: self.fs.clone(),
            cgroup: self.cgroup.clone(),
            kind,
        });
        DirEntry::new_file(
            FileNode::new(file),
            NodeType::RegularFile,
            Reference::new(Some(parent.clone()), kind.name().to_string()),
        )
    }

    fn child_dir_entry(&self, parent: &DirEntry, name: &str, cgroup: Arc<CgroupNode>) -> DirEntry {
        let reference = Reference::new(Some(parent.clone()), name.to_string());
        DirEntry::new_dir(
            |this| DirNode::new(Self::new(self.fs.clone(), cgroup, this)),
            reference,
        )
    }
}

impl NodeOps for CgroupDir {
    fn inode(&self) -> u64 {
        self.cgroup.fs_inode(CgroupFsTarget::Directory)
    }

    fn identity(&self) -> Option<NodeIdentity> {
        Some(inode_identity(&self.cgroup, self.inode()))
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        inode_metadata(&self.cgroup, CgroupFsTarget::Directory)
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        update_inode_metadata(&self.cgroup, CgroupFsTarget::Directory, update)
    }

    fn change_metadata(&self, request: &MetadataRequest<'_>) -> NamespaceCompletion<Metadata> {
        metadata::change_inode_metadata(&self.fs, &self.cgroup, CgroupFsTarget::Directory, request)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.fs.as_ref()
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl DirNodeOps for CgroupDir {
    fn namespace_domain(&self) -> usize {
        core::ptr::from_ref(self.cgroup.mutation_domain()) as usize
    }

    fn read_dir(&self, cursor: DirectoryCursor, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let mut names = Vec::new();
        names.push(DOT.to_string());
        names.push(DOTDOT.to_string());
        {
            let guard = self.cgroup.mutation_domain().lock();
            for kind in CgroupFileKind::ALL {
                match guard.fs_attributes(&self.cgroup, CgroupFsTarget::File(kind)) {
                    Ok(_) => names.push(kind.name().to_string()),
                    Err(CgroupError::NotFound) => {}
                    Err(error) => return Err(cgroup_error_to_vfs_error(error)),
                }
            }
            names.extend(self.cgroup.child_names());
        }

        let this_entry = self.this_entry()?;
        let this_dir = this_entry.as_dir()?;
        let mut count = 0;
        for (i, name) in names.iter().enumerate().skip(cursor.offset() as usize) {
            let entry = match name.as_str() {
                DOT => this_entry.clone(),
                DOTDOT => this_entry.parent().unwrap_or_else(|| this_entry.clone()),
                other => this_dir.lookup(other)?,
            };
            if !sink.accept(
                name.as_bytes(),
                entry.inode(),
                entry.node_type(),
                DirectoryCursor::new(i as u64 + 1),
            ) {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        let parent = self.this_entry()?;
        let child = {
            let guard = self.cgroup.mutation_domain().lock();
            guard.check_live(&self.cgroup).map_err(path_error)?;
            if let Some(kind) = CgroupFileKind::from_name(name) {
                guard
                    .fs_attributes(&self.cgroup, CgroupFsTarget::File(kind))
                    .map_err(cgroup_error_to_vfs_error)?;
                drop(guard);
                return Ok(self.file_entry(&parent, kind));
            }
            guard.lookup_child(&self.cgroup, name).map_err(path_error)?
        };
        Ok(self.child_dir_entry(&parent, name, child))
    }

    fn is_cacheable(&self) -> bool {
        false
    }

    fn has_children(&self) -> VfsResult<bool> {
        Ok(self.cgroup.has_children())
    }

    fn mutate_namespace(
        &self,
        command: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        self.mutate_names(command, retired)
    }
}

fn inode_identity(node: &CgroupNode, inode: u64) -> NodeIdentity {
    // All view aliases retain the same domain and node. The stable inode
    // encodes the non-reused node generation and target slot.
    NodeIdentity::new([core::ptr::from_ref(node.mutation_domain()) as u64, inode])
}

fn inode_metadata(node: &CgroupNode, target: CgroupFsTarget) -> VfsResult<Metadata> {
    node.fs_attributes(target)
        .map(|attributes| metadata_from_attributes(target, attributes))
        .map_err(cgroup_error_to_vfs_error)
}

fn metadata_from_attributes(target: CgroupFsTarget, attributes: CgroupFsAttributes) -> Metadata {
    Metadata {
        device: 0,
        inode: attributes.inode,
        nlink: 1,
        mode: NodePermission::from_bits_retain(attributes.mode),
        node_type: match target {
            CgroupFsTarget::Directory => NodeType::Directory,
            CgroupFsTarget::File(_) => NodeType::RegularFile,
        },
        uid: attributes.uid,
        gid: attributes.gid,
        size: 0,
        block_size: 0,
        blocks: 0,
        rdev: DeviceId::default(),
        atime: attributes.atime,
        mtime: attributes.mtime,
        ctime: attributes.ctime,
    }
}

fn update_inode_metadata(
    node: &CgroupNode,
    target: CgroupFsTarget,
    update: MetadataUpdate,
) -> VfsResult<()> {
    if update.rdev.is_some() {
        return Err(VfsError::OperationNotSupported);
    }
    // Transitional raw consumers still supply these fields. Native requests
    // use the real-owner attempt and must never fall back to this bridge.
    node.update_fs_attributes(
        target,
        CgroupFsUpdate {
            uid: update.owner.map(|(uid, _)| uid),
            gid: update.owner.map(|(_, gid)| gid),
            mode: update.mode.map(|mode| mode.bits()),
            atime: update.atime,
            mtime: update.mtime,
            ctime: None,
        },
    )
    .map_err(cgroup_error_to_vfs_error)
}

fn path_error(error: CgroupError) -> VfsError {
    match error {
        CgroupError::Removed => VfsError::NotFound,
        other => cgroup_error_to_vfs_error(other),
    }
}

fn cgroup_error_to_vfs_error(error: CgroupError) -> VfsError {
    match error {
        CgroupError::NotInitialized | CgroupError::InvalidInput => VfsError::InvalidInput,
        CgroupError::NotFound => VfsError::NotFound,
        CgroupError::Removed => VfsError::NoSuchDevice,
        CgroupError::AlreadyExists => VfsError::AlreadyExists,
        CgroupError::ResourceBusy => VfsError::ResourceBusy,
        CgroupError::LimitExceeded => VfsError::WouldBlock,
        CgroupError::NoSuchProcess => VfsError::NotFound,
        CgroupError::DirectoryNotEmpty => VfsError::DirectoryNotEmpty,
        CgroupError::NoSpace => VfsError::StorageFull,
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
