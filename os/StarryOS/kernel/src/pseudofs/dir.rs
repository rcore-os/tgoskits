use alloc::{
    borrow::{Cow, ToOwned},
    boxed::Box,
    collections::btree_map::BTreeMap,
    string::String,
    sync::Arc,
};
use core::any::Any;

use axfs_ng_vfs::{
    AuthorizedMutation, CreateOwnerPolicy, DirEntry, DirEntrySink, DirNode, DirNodeOps,
    DirectoryCursor, FileNode, FilesystemOps, LockedAttempt, Metadata, MetadataRequest,
    MetadataUpdate, MutationObservation, MutationRetire, NamespaceCompletion, NamespaceEffect,
    NamespaceMutation, NamespaceNode, NamespaceOutcome, NodeIdentity, NodeOps, NodePermission,
    NodeType, Reference, RenameNode, VfsError, VfsResult, WeakDirEntry,
    path::{DOT, DOTDOT},
};
use inherit_methods_macro::inherit_methods;

use super::{
    DirMaker, NodeOpsMux, SimpleFsContext, SimpleFsNode, SimpleFsOwner, SimpleInode,
    recipe::DirRecipe,
};
use crate::pseudofs::NodeOpsMuxTy;

/// Operations for a simple directory.
pub trait SimpleDirOps: Send + Sync + 'static {
    /// Semantic identity for dynamically materialized directories.
    fn identity(&self) -> Option<NodeIdentity> {
        None
    }

    /// Get the names of all children in the directory.
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a>;
    /// Look up a child directory or file by name.
    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux>;

    /// Check if the directory is cacheable.
    ///
    /// See [`DirNodeOps::is_cacheable`].
    fn is_cacheable(&self) -> bool {
        true
    }

    /// Check whether one child has a stable directory entry.
    fn is_cacheable_child(&self, _name: &str) -> bool {
        self.is_cacheable()
    }

    /// Combines two directories into one.
    fn chain<N: SimpleDirOps>(self, other: N) -> ChainedDirOps<Self, N>
    where
        Self: Sized,
    {
        ChainedDirOps(self, other)
    }
}

impl SimpleDirOps for DirMapping {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(self.map.keys().map(|s| s.as_str().into()))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match self.map.get(name).ok_or(VfsError::NotFound)? {
            NodeOpsMuxTy::Static(ops) => Ok(ops.clone()),
            NodeOpsMuxTy::Dynamic(maker) => maker(),
        }
    }

    fn is_cacheable(&self) -> bool {
        self.cacheable
    }
}

/// A mapping of directory names to entries.
pub struct DirMapping {
    map: BTreeMap<String, NodeOpsMuxTy>,
    cacheable: bool,
}

impl DirMapping {
    /// Create a new empty directory mapping.
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            cacheable: true,
        }
    }

    /// Set whether the directory is cacheable.
    pub fn set_cacheable(&mut self, cacheable: bool) {
        self.cacheable = cacheable;
    }

    /// Add a new entry to the directory mapping.
    pub fn add(&mut self, name: impl Into<String>, ops: impl Into<NodeOpsMux>) {
        self.map
            .insert(name.into(), NodeOpsMuxTy::Static(ops.into()));
    }

    /// Add a new entry to the directory mapping, created on demand.
    pub fn add_dynamic(
        &mut self,
        name: impl Into<String>,
        maker: impl Fn() -> VfsResult<NodeOpsMux> + Send + Sync + 'static,
    ) {
        self.map
            .insert(name.into(), NodeOpsMuxTy::Dynamic(Box::new(maker)));
    }
}

impl Default for DirMapping {
    fn default() -> Self {
        Self::new()
    }
}

/// Directory created by [`SimpleDirOps::chain`].
pub struct ChainedDirOps<A, B>(A, B);

impl<A: SimpleDirOps, B: SimpleDirOps> SimpleDirOps for ChainedDirOps<A, B> {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(self.0.child_names().chain(self.1.child_names()))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match self.0.lookup_child(name) {
            Ok(ops) => Ok(ops),
            Err(VfsError::NotFound) => self.1.lookup_child(name),
            Err(e) => Err(e),
        }
    }

    fn is_cacheable(&self) -> bool {
        // TODO: If one of the ops is not cacheable while the other is, the
        // behavior is undefined.
        self.0.is_cacheable() && self.1.is_cacheable()
    }
}

/// Simple directory.
pub struct SimpleDir<O> {
    node: SimpleFsNode,
    this: WeakDirEntry,
    ops: Arc<O>,
}

impl<O: SimpleDirOps> SimpleDir<O> {
    fn new(node: SimpleFsNode, ops: Arc<O>, this: WeakDirEntry) -> Arc<Self> {
        Arc::new(Self { node, this, ops })
    }

    /// Create a [`DirMaker`] from given directory operations.
    pub fn new_maker(context: SimpleFsContext, ops: Arc<O>) -> VfsResult<DirMaker> {
        let inode = context.allocate_inode(
            NodeType::Directory,
            NodePermission::from_bits_truncate(0o755),
        )?;
        Ok(DirMaker::new(context, SimpleDirRecipe { inode, ops }))
    }
}

struct SimpleDirRecipe<O> {
    inode: Arc<SimpleInode>,
    ops: Arc<O>,
}

impl<O: SimpleDirOps> DirRecipe for SimpleDirRecipe<O> {
    fn materialize(
        self: Arc<Self>,
        owner: SimpleFsOwner,
        this: WeakDirEntry,
    ) -> Arc<dyn DirNodeOps> {
        SimpleDir::new(
            SimpleFsNode::bind(owner, self.inode.clone()),
            self.ops.clone(),
            this,
        )
    }
}

#[inherit_methods(from = "self.node")]
impl<O: SimpleDirOps> NodeOps for SimpleDir<O> {
    fn identity(&self) -> Option<NodeIdentity> {
        self.ops.identity()
    }

    fn inode(&self) -> u64;

    fn metadata(&self) -> VfsResult<Metadata>;

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;

    fn change_metadata(&self, request: &MetadataRequest<'_>) -> NamespaceCompletion<Metadata>;

    fn filesystem(&self) -> &dyn FilesystemOps;

    fn sync(&self, data_only: bool) -> VfsResult<()>;

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl<O: SimpleDirOps> DirNodeOps for SimpleDir<O> {
    fn namespace_domain(&self) -> usize {
        self.filesystem() as *const dyn FilesystemOps as *const () as usize
    }

    fn read_dir(&self, cursor: DirectoryCursor, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let children = [DOT, DOTDOT]
            .into_iter()
            .map(Cow::Borrowed)
            .chain(self.ops.child_names());

        let this_entry = self.this.upgrade().unwrap();
        let this_dir = this_entry.as_dir()?;

        let mut count = 0;
        for (i, name) in children.enumerate().skip(cursor.offset() as usize) {
            let metadata = match name.as_ref() {
                DOT => this_entry.metadata(),
                DOTDOT => this_entry
                    .parent()
                    .map_or_else(|| this_entry.metadata(), |parent| parent.metadata()),
                other => {
                    let entry = this_dir.lookup(other)?;
                    entry.metadata()
                }
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
        let ops = self.ops.lookup_child(name)?;
        let reference = Reference::new(self.this.upgrade(), name.to_owned());
        Ok(match ops {
            NodeOpsMux::Dir(maker) => {
                let prepared = maker.prepare(self.node.owner())?;
                DirEntry::new_dir(|this| DirNode::new(prepared.materialize(this)), reference)
            }
            NodeOpsMux::File(maker) => {
                let ops = maker.prepare(self.node.owner())?.materialize();
                let node_type = ops.metadata()?.node_type;
                DirEntry::new_file(FileNode::new(ops.clone()), node_type, reference)
            }
        })
    }

    fn is_cacheable(&self) -> bool {
        self.ops.is_cacheable()
    }

    fn is_cacheable_child(&self, name: &str) -> bool {
        self.ops.is_cacheable_child(name)
    }

    fn mutate_namespace(
        &self,
        command: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        if !matches!(command, NamespaceMutation::Create(_)) {
            return NamespaceOutcome::unchanged(Err(VfsError::OperationNotPermitted));
        }
        let (parent, _) = command.parents();
        if !parent
            .downcast::<Self>()
            .is_ok_and(|directory| core::ptr::eq(directory.as_ref(), self))
        {
            return NamespaceOutcome::unchanged(Err(VfsError::BadState));
        }
        let _gate = self.node.lock_namespace();
        command.execute(&mut ExistingOpenAttempt { directory: self }, retired)
    }
}

/// The fixed authorizer can select an existing entry, but this immutable
/// directory never commits a create. All aliases share the actual FS gate.
struct ExistingOpenAttempt<'a, O> {
    directory: &'a SimpleDir<O>,
}

impl<O: SimpleDirOps> LockedAttempt for ExistingOpenAttempt<'_, O> {
    fn parents(&mut self) -> VfsResult<(Metadata, Metadata)> {
        let parent = self.directory.metadata()?;
        Ok((parent.clone(), parent))
    }

    fn observe(
        &mut self,
        request: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<MutationObservation> {
        let NamespaceMutation::Create(request) = request else {
            return Err(VfsError::OperationNotPermitted);
        };
        let name = request.name();
        let target = match self.directory.lookup(name) {
            Ok(entry) => {
                retired.retire(entry.clone());
                Some(RenameNode {
                    metadata: entry.metadata()?,
                    entry,
                    canonical_name: name.into(),
                    cacheable: self.directory.is_cacheable_child(name),
                    empty: false,
                })
            }
            Err(VfsError::NotFound) => None,
            Err(error) => return Err(error),
        };
        Ok(MutationObservation::Create {
            target,
            owner_policy: CreateOwnerPolicy::ParentSetgid,
        })
    }

    fn is_ancestor(&mut self, _: &NamespaceNode, _: u64) -> VfsResult<bool> {
        Err(VfsError::OperationNotPermitted)
    }

    fn commit(
        &mut self,
        _: &AuthorizedMutation<'_>,
        _: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        NamespaceOutcome::unchanged(Err(VfsError::OperationNotPermitted))
    }
}
