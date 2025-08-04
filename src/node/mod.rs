mod dir;
mod file;

use alloc::{
    borrow::ToOwned,
    boxed::Box,
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{any::Any, iter, ops::Deref, task::Context};

use axio::{IoEvents, Pollable};
pub use dir::*;
pub use file::*;
use inherit_methods_macro::inherit_methods;

use crate::{
    FilesystemOps, Metadata, MetadataUpdate, Mutex, MutexGuard, NodeType, VfsError, VfsResult,
    path::PathBuf,
};

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

pub type ReferenceKey = (usize, String);

pub struct Reference {
    parent: Option<DirEntry>,
    name: String,
}

impl Reference {
    pub fn new(parent: Option<DirEntry>, name: String) -> Self {
        Self { parent, name }
    }

    pub fn root() -> Self {
        Self::new(None, String::new())
    }

    pub fn key(&self) -> ReferenceKey {
        let address = self
            .parent
            .as_ref()
            .map_or(0, |it| Arc::as_ptr(&it.0) as usize);
        (address, self.name.clone())
    }
}

struct Inner {
    node: Node,
    node_type: NodeType,
    reference: Reference,
    user_data: Mutex<Option<Box<dyn Any + Send + Sync>>>,
}

pub struct DirEntry(Arc<Inner>);

impl Clone for DirEntry {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

pub struct WeakDirEntry(Weak<Inner>);

impl Clone for WeakDirEntry {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl WeakDirEntry {
    pub fn upgrade(&self) -> Option<DirEntry> {
        self.0.upgrade().map(DirEntry)
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

    pub fn sync(&self, data_only: bool) -> VfsResult<()>;
}

impl DirEntry {
    pub fn new_file(node: FileNode, node_type: NodeType, reference: Reference) -> Self {
        Self(Arc::new(Inner {
            node: Node::File(node),
            node_type,
            reference,
            user_data: Mutex::default(),
        }))
    }

    pub fn new_dir(node_fn: impl FnOnce(WeakDirEntry) -> DirNode, reference: Reference) -> Self {
        Self(Arc::new_cyclic(|this| Inner {
            node: Node::Dir(node_fn(WeakDirEntry(this.clone()))),
            node_type: NodeType::Directory,
            reference,
            user_data: Mutex::default(),
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
            .map_err(|_| VfsError::EINVAL)
    }

    pub fn downgrade(&self) -> WeakDirEntry {
        WeakDirEntry(Arc::downgrade(&self.0))
    }

    pub fn key(&self) -> ReferenceKey {
        self.0.reference.key()
    }

    pub fn node_type(&self) -> NodeType {
        self.0.node_type
    }

    pub fn parent(&self) -> Option<Self> {
        self.0.reference.parent.clone()
    }

    pub fn name(&self) -> &str {
        &self.0.reference.name
    }

    /// Checks if the entry is a root of a mount point.
    pub fn is_root_of_mount(&self) -> bool {
        self.0.reference.parent.is_none()
    }

    pub fn is_ancestor_of(&self, other: &Self) -> VfsResult<bool> {
        let mut current = other.clone();
        loop {
            if current.ptr_eq(self) {
                return Ok(true);
            }
            if let Some(parent) = current.parent() {
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
            components.push(current.name().to_owned());
            if let Some(parent) = current.parent() {
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
            _ => Err(VfsError::EISDIR),
        }
    }

    pub fn as_dir(&self) -> VfsResult<&DirNode> {
        match &self.0.node {
            Node::Dir(dir) => Ok(dir),
            _ => Err(VfsError::ENOTDIR),
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
            return Err(VfsError::EINVAL);
        }
        let file = self.as_file()?;
        let mut buf = vec![0; file.len()? as usize];
        file.read_at(&mut buf, 0)?;
        String::from_utf8(buf).map_err(|_| VfsError::EINVAL)
    }

    pub fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match &self.0.node {
            Node::File(file) => file.ioctl(cmd, arg),
            Node::Dir(_) => Err(VfsError::ENOTTY),
        }
    }

    pub fn user_data(&self) -> MutexGuard<'_, Option<Box<dyn Any + Send + Sync>>> {
        self.0.user_data.lock()
    }
}

impl Pollable for DirEntry {
    fn poll(&self) -> IoEvents {
        match &self.0.node {
            Node::File(file) => file.poll(),
            Node::Dir(_dir) => IoEvents::IN | IoEvents::OUT,
        }
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        match &self.0.node {
            Node::File(file) => file.register(context, events),
            Node::Dir(_) => {}
        }
    }
}
