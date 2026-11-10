mod dir;
mod file;
mod link;
mod name;

use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    any::{Any, TypeId},
    fmt, iter,
    ops::Deref,
};

use axpoll::{IoEvents, Pollable};
use bitflags::bitflags;
pub use dir::*;
pub use file::*;
use inherit_methods_macro::inherit_methods;
pub use link::*;
pub use name::*;
use smallvec::SmallVec;

use crate::{
    FilesystemOps, Metadata, MetadataUpdate, NodeType, RawSpinLock, RawSpinLockGuard, VfsError,
    VfsResult, WritebackPolicy, path::PathBuf,
};

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct NodeFlags: u32 {
        /// Indicates that this file behaves like a stream.
        ///
        /// Presence of this flag could inform the higher layers to omit
        /// maintaining a position for this file. `read_at` and `write_at` would
        /// be called with zero offset instead.
        const STREAM = 0x0001;

        /// Indicates that this file should not be cached.
        ///
        /// For instance, files in `/proc` or `/sys` may contain dynamic data
        /// that should not be cached.
        const NON_CACHEABLE = 0x0002;

        /// Indicates that this file should always be cached.
        ///
        /// For instance, files in tmpfs relies on page caching and do not have
        /// a backing device.
        const ALWAYS_CACHE = 0x0004;

        /// Indicates that operations on this file are always blocking.
        ///
        /// This could prevent higher layers from attempting to add unnecessary
        /// non-blocking handling.
        const BLOCKING = 0x0008;

        /// Indicates that every read or write transfers exactly one record.
        ///
        /// The direct backend then calls the node once per request instead
        /// of filling or draining the caller's buffer across records.
        const PACKET = 0x0010;
    }
}

/// Filesystem node operationss
#[allow(clippy::len_without_is_empty)]
pub trait NodeOps: Send + Sync + 'static {
    /// Gets the inode number of the node.
    fn inode(&self) -> u64;

    /// Gets the metadata of the node.
    fn metadata(&self) -> VfsResult<Metadata>;

    /// Updates the metadata of the node.
    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;

    /// Gets the filesystem
    fn filesystem(&self) -> &dyn FilesystemOps;

    /// Gets the size of the node.
    fn len(&self) -> VfsResult<u64> {
        self.metadata().map(|m| m.size)
    }

    /// Synchronizes the file to disk.
    fn sync(&self, data_only: bool) -> VfsResult<()>;

    /// Casts the node to a `&dyn core::any::Any`.
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;

    /// Returns the flags of the node.
    fn flags(&self) -> NodeFlags {
        NodeFlags::empty()
    }

    /// Queries inode-owned persistence flags. Mount policy is applied by the
    /// caller separately. Returns storage errors instead of hiding a failed
    /// inode read behind a buffered default.
    fn writeback_policy(&self) -> VfsResult<WritebackPolicy> {
        Ok(WritebackPolicy::empty())
    }

    /// Returns the optional persistent extended-attribute capability.
    fn xattr_ops(&self) -> Option<&dyn XattrOps> {
        None
    }

    /// Returns inode-owned state that survives eviction of directory entries.
    /// Memory filesystems use this for file contents shared by hard links.
    fn inode_user_data(&self) -> Option<&RawSpinLock<TypeMap>> {
        None
    }

    /// Identifies a dynamic node independently of a freshly allocated inode.
    fn identity(&self) -> Option<NodeIdentity> {
        None
    }

    fn link_ops(&self) -> Option<&dyn LinkOps> {
        None
    }
}

/// Persistent extended-attribute capability owned by a filesystem inode.
pub trait XattrOps: Send + Sync {
    fn get_xattr(&self, name: &[u8]) -> VfsResult<Vec<u8>>;

    fn list_xattrs(&self) -> VfsResult<Vec<Vec<u8>>>;

    fn set_xattr(&self, name: &[u8], value: &[u8], mode: XattrSetMode) -> VfsResult<()>;

    fn remove_xattr(&self, name: &[u8]) -> VfsResult<()>;
}

/// Create/replace policy passed through VFS without Linux flag bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XattrSetMode {
    Upsert,
    Create,
    Replace,
}

enum Node {
    File(FileNode),
    Dir(DirNode),
}

impl Node {
    pub fn clone_inner(&self) -> Arc<dyn NodeOps> {
        match self {
            Node::File(file) => file.inner().clone(),
            Node::Dir(dir) => dir.inner().clone(),
        }
    }
}

impl Deref for Node {
    type Target = dyn NodeOps;

    fn deref(&self) -> &Self::Target {
        match &self {
            Node::File(file) => file.deref(),
            Node::Dir(dir) => dir.deref(),
        }
    }
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Node::File(file) => write!(f, "FileNode({})", file.inode()),
            Node::Dir(dir) => write!(f, "DirNode({})", dir.inode()),
        }
    }
}

#[derive(Default, Clone)]
pub struct TypeMap(SmallVec<[(TypeId, Arc<dyn Any + Send + Sync>); 2]>);
impl TypeMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert<T: Any + Send + Sync>(&mut self, value: T) {
        self.0.push((TypeId::of::<T>(), Arc::new(value)));
    }

    pub fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.0
            .iter()
            .find_map(|(id, value)| {
                if id == &TypeId::of::<T>() {
                    Some(value.clone())
                } else {
                    None
                }
            })
            .and_then(|value| value.downcast().ok())
    }

    pub fn get_or_insert_with<T: Any + Send + Sync>(&mut self, f: impl FnOnce() -> T) -> Arc<T> {
        if let Some(value) = self.get::<T>() {
            value
        } else {
            let value = f();
            self.insert(value);
            self.get::<T>().unwrap()
        }
    }
}

struct Inner {
    node: Node,
    node_type: NodeType,
    reference: RawSpinLock<NameSnapshot>,
    identity: Option<NodeIdentity>,
    backend_key: (usize, u64),
    user_data: RawSpinLock<TypeMap>,
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner")
            .field("node", &self.node)
            .field("node_type", &self.node_type)
            .field("reference", &self.reference)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct DirEntry(Arc<Inner>);

#[derive(Debug, Clone)]
pub struct WeakDirEntry(Weak<Inner>);

impl WeakDirEntry {
    pub fn upgrade(&self) -> Option<DirEntry> {
        self.0.upgrade().map(DirEntry)
    }

    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.0.strong_count() != 0
    }
}

impl From<Node> for Arc<dyn NodeOps> {
    fn from(node: Node) -> Self {
        match node {
            Node::File(file) => file.into(),
            Node::Dir(dir) => dir.into(),
        }
    }
}

#[inherit_methods(from = "self.0.node")]
impl DirEntry {
    pub fn inode(&self) -> u64;

    pub fn filesystem(&self) -> &dyn FilesystemOps;

    pub fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> VfsResult<u64>;

    pub fn flags(&self) -> NodeFlags;

    pub fn writeback_policy(&self) -> VfsResult<WritebackPolicy>;

    pub fn sync(&self, data_only: bool) -> VfsResult<()>;
}

impl DirEntry {
    pub fn new_file(node: FileNode, node_type: NodeType, reference: Reference) -> Self {
        let identity = node.identity();
        let backend_key = (
            node.filesystem() as *const dyn FilesystemOps as *const () as usize,
            node.inode(),
        );
        Self(Arc::new(Inner {
            node: Node::File(node),
            node_type,
            reference: RawSpinLock::new(reference.0),
            identity,
            backend_key,
            user_data: RawSpinLock::new(TypeMap::default()),
        }))
    }

    pub fn new_dir(node_fn: impl FnOnce(WeakDirEntry) -> DirNode, reference: Reference) -> Self {
        Self(Arc::new_cyclic(|this| {
            let mut node = node_fn(WeakDirEntry(this.clone()));
            node.set_owner(WeakDirEntry(this.clone()));
            let identity = node.identity();
            let backend_key = (
                node.filesystem() as *const dyn FilesystemOps as *const () as usize,
                node.inode(),
            );
            Inner {
                node: Node::Dir(node),
                node_type: NodeType::Directory,
                reference: RawSpinLock::new(reference.0),
                identity,
                backend_key,
                user_data: RawSpinLock::new(TypeMap::default()),
            }
        }))
    }

    pub fn metadata(&self) -> VfsResult<Metadata> {
        self.0.node.metadata().map(|mut metadata| {
            metadata.node_type = self.0.node_type;
            metadata
        })
    }

    pub fn downcast<T: NodeOps>(&self) -> VfsResult<Arc<T>> {
        self.0
            .node
            .clone_inner()
            .into_any()
            .downcast()
            .map_err(|_| VfsError::InvalidInput)
    }

    pub fn downgrade(&self) -> WeakDirEntry {
        WeakDirEntry(Arc::downgrade(&self.0))
    }

    pub(super) fn has_other_owners(&self) -> bool {
        Arc::strong_count(&self.0) != 1
    }

    pub fn key(&self) -> DirEntryId {
        DirEntryId(self.as_ptr())
    }

    /// Observes parent, name, and linkage under one lock.
    ///
    /// Keep the snapshot alive while borrowing its name:
    ///
    /// ```
    /// use axfs_ng_vfs::DirEntry;
    /// fn owned_name(entry: &DirEntry) -> String {
    ///     entry.name_snapshot().name().to_owned()
    /// }
    /// ```
    ///
    /// A name cannot outlive its snapshot:
    ///
    /// ```compile_fail
    /// use axfs_ng_vfs::DirEntry;
    /// fn borrowed_name(entry: &DirEntry) -> &str {
    ///     entry.name_snapshot().name()
    /// }
    /// ```
    pub fn name_snapshot(&self) -> NameSnapshot {
        self.0.reference.lock().clone()
    }

    pub fn name_state(&self) -> NameState {
        self.0.reference.lock().state
    }

    pub fn identity(&self) -> Option<&NodeIdentity> {
        self.0.identity.as_ref()
    }

    pub(crate) fn same_backend_object(&self, other: &Self) -> bool {
        match (self.identity(), other.identity()) {
            (Some(left), Some(right)) => left == right,
            (None, None) => {
                self.0.backend_key == other.0.backend_key && self.node_type() == other.node_type()
            }
            _ => false,
        }
    }

    pub(crate) fn move_to(&self, parent: DirEntry, name: String) {
        let old = {
            let mut reference = self.0.reference.lock();
            core::mem::replace(
                &mut *reference,
                NameSnapshot {
                    parent: Some(parent),
                    name,
                    state: NameState::Linked,
                },
            )
        };
        drop(old);
    }

    pub(crate) fn exchange_names(
        left: &Self,
        left_parent: Self,
        left_name: String,
        right: &Self,
        right_parent: Self,
        right_name: String,
    ) {
        let (first, second) = if left.as_ptr() < right.as_ptr() {
            (left, right)
        } else {
            (right, left)
        };
        let retired = {
            let mut first_state = first.0.reference.lock();
            let mut second_state = second.0.reference.lock();
            let (left_state, right_state) = if left.ptr_eq(first) {
                (&mut *first_state, &mut *second_state)
            } else {
                (&mut *second_state, &mut *first_state)
            };
            (
                core::mem::replace(
                    left_state,
                    NameSnapshot {
                        parent: Some(left_parent),
                        name: left_name,
                        state: NameState::Linked,
                    },
                ),
                core::mem::replace(
                    right_state,
                    NameSnapshot {
                        parent: Some(right_parent),
                        name: right_name,
                        state: NameState::Linked,
                    },
                ),
            )
        };
        drop(retired);
    }

    pub(crate) fn set_name_state(&self, state: NameState) {
        self.0.reference.lock().state = state;
    }

    pub fn node_type(&self) -> NodeType {
        self.0.node_type
    }

    pub fn parent(&self) -> Option<Self> {
        self.name_snapshot().parent
    }

    pub fn name(&self) -> String {
        self.name_snapshot().name
    }

    /// Checks if the entry is a root of a mount point.
    pub fn is_root_of_mount(&self) -> bool {
        self.name_state() == NameState::Root
    }

    pub fn is_ancestor_of(&self, other: &Self) -> VfsResult<bool> {
        let mut current = other.clone();
        loop {
            if current.ptr_eq(self) {
                return Ok(true);
            }
            let snapshot = current.name_snapshot();
            if snapshot.state == NameState::Untrusted {
                return Err(VfsError::WouldBlock);
            }
            if let Some(parent) = snapshot.parent {
                current = parent;
            } else {
                break;
            }
        }
        Ok(false)
    }

    pub(crate) fn collect_absolute_path(&self, components: &mut Vec<String>) {
        let mut current = self.clone();
        loop {
            let snapshot = current.name_snapshot();
            components.push(snapshot.name);
            if let Some(parent) = snapshot.parent {
                current = parent;
            } else {
                break;
            }
        }
    }

    pub fn absolute_path(&self) -> VfsResult<PathBuf> {
        let mut components = vec![];
        self.collect_absolute_path(&mut components);
        Ok(iter::once("/")
            .chain(components.iter().map(String::as_str).rev())
            .collect())
    }

    pub fn is_file(&self) -> bool {
        matches!(self.0.node, Node::File(_))
    }

    pub fn is_dir(&self) -> bool {
        matches!(self.0.node, Node::Dir(_))
    }

    pub fn as_file(&self) -> VfsResult<&FileNode> {
        match &self.0.node {
            Node::File(file) => Ok(file),
            _ => Err(VfsError::IsADirectory),
        }
    }

    pub fn as_dir(&self) -> VfsResult<&DirNode> {
        match &self.0.node {
            Node::Dir(dir) => Ok(dir),
            _ => Err(VfsError::NotADirectory),
        }
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn as_ptr(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }

    pub fn read_link(&self) -> VfsResult<String> {
        if self.node_type() != NodeType::Symlink {
            return Err(VfsError::InvalidData);
        }
        if let Some(link) = self.0.node.link_ops() {
            return link.read_link();
        }
        let file = self.as_file()?;
        let mut buf = vec![0; file.len()? as usize];
        file.read_at(&mut buf, 0)?;
        String::from_utf8(buf).map_err(|_| VfsError::InvalidData)
    }

    pub fn link_target(&self) -> VfsResult<LinkTarget> {
        if self.node_type() != NodeType::Symlink {
            return Err(VfsError::InvalidData);
        }
        match self.0.node.link_ops() {
            Some(link) => link.target(),
            None => self.read_link().map(LinkTarget::Text),
        }
    }

    pub fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match &self.0.node {
            Node::File(file) => file.ioctl(cmd, arg),
            Node::Dir(_) => Err(VfsError::NotATty),
        }
    }

    pub fn user_data(&self) -> RawSpinLockGuard<'_, TypeMap> {
        match self.0.node.inode_user_data() {
            Some(state) => state.lock(),
            None => self.0.user_data.lock(),
        }
    }

    pub fn get_xattr(&self, name: &[u8]) -> VfsResult<Vec<u8>> {
        self.0
            .node
            .xattr_ops()
            .ok_or(VfsError::OperationNotSupported)?
            .get_xattr(name)
    }

    pub fn list_xattrs(&self) -> VfsResult<Vec<Vec<u8>>> {
        self.0
            .node
            .xattr_ops()
            .ok_or(VfsError::OperationNotSupported)?
            .list_xattrs()
    }

    pub fn set_xattr(&self, name: &[u8], value: &[u8], mode: XattrSetMode) -> VfsResult<()> {
        self.0
            .node
            .xattr_ops()
            .ok_or(VfsError::OperationNotSupported)?
            .set_xattr(name, value, mode)
    }

    pub fn remove_xattr(&self, name: &[u8]) -> VfsResult<()> {
        self.0
            .node
            .xattr_ops()
            .ok_or(VfsError::OperationNotSupported)?
            .remove_xattr(name)
    }
}

impl Pollable for DirEntry {
    fn poll(&self) -> IoEvents {
        match &self.0.node {
            Node::File(file) => file.poll(),
            Node::Dir(_dir) => IoEvents::IN | IoEvents::OUT,
        }
    }

    unsafe fn register_shared(
        &self,
        sink: &mut dyn axpoll::SharedRegistrationSink,
        events: IoEvents,
    ) {
        match &self.0.node {
            Node::File(file) => unsafe { file.register_shared(sink, events) },
            Node::Dir(_) => {}
        }
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        match &self.0.node {
            Node::File(file) => unsafe { file.register_exclusive(sink, events) },
            Node::Dir(_) => {}
        }
    }
}
