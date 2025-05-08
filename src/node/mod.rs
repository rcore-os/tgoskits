mod dir;
mod file;

use core::ops::Deref;

pub use dir::*;
pub use file::*;

use alloc::{
    borrow::ToOwned,
    string::String,
    sync::{Arc, Weak},
    vec,
};
use lock_api::RawMutex;

use crate::{FilesystemOps, Metadata, NodeType, PathBuf, VfsError, VfsResult};

/// Filesystem node operationss
pub trait NodeOps<M>: Send + Sync {
    /// Gets the inode number of the node.
    fn inode(&self) -> u64;

    /// Gets the metadata of the node.
    fn metadata(&self) -> VfsResult<Metadata>;

    /// Gets the filesystem
    fn filesystem(&self) -> &dyn FilesystemOps<M>;

    /// Gets the size of the node.
    fn len(&self) -> VfsResult<u64> {
        self.metadata().map(|m| m.size)
    }

    /// Synchronizes the file to disk.
    fn sync(&self, data_only: bool) -> VfsResult<()>;

    /// Casts the node to a `&dyn core::any::Any`.
    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync>;
}

enum Node<M> {
    File(FileNode<M>),
    Dir(DirNode<M>),
}
impl<M: RawMutex> Node<M> {
    pub fn clone_inner(&self) -> Arc<dyn NodeOps<M>> {
        match self {
            Node::File(file) => file.inner().clone(),
            Node::Dir(dir) => dir.inner().clone(),
        }
    }
}

pub struct Reference<M> {
    parent: Option<WeakDirEntry<M>>,
    name: String,
}
impl<M> Reference<M> {
    pub fn new(parent: Option<WeakDirEntry<M>>, name: String) -> Self {
        Self { parent, name }
    }

    pub fn root() -> Self {
        Self::new(None, String::new())
    }
}

struct Inner<M> {
    node: Node<M>,
    node_type: NodeType,
    reference: Reference<M>,
}
pub struct DirEntry<M>(Arc<Inner<M>>);
impl<M> Clone for DirEntry<M> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

pub struct WeakDirEntry<M>(Weak<Inner<M>>);
impl<M> Clone for WeakDirEntry<M> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<M> WeakDirEntry<M> {
    pub fn upgrade(&self) -> VfsResult<DirEntry<M>> {
        self.0.upgrade().map(DirEntry).ok_or(VfsError::ENOENT)
    }
}

impl<M> Deref for DirEntry<M> {
    type Target = dyn NodeOps<M>;

    fn deref(&self) -> &Self::Target {
        match &self.0.node {
            Node::File(file) => file.deref(),
            Node::Dir(dir) => dir.deref(),
        }
    }
}

impl<M> From<Node<M>> for Arc<dyn NodeOps<M>> {
    fn from(node: Node<M>) -> Self {
        match node {
            Node::File(file) => file.into(),
            Node::Dir(dir) => dir.into(),
        }
    }
}

impl<M: RawMutex> DirEntry<M> {
    pub fn new_file(node: FileNode<M>, node_type: NodeType, reference: Reference<M>) -> Self {
        Self(Arc::new(Inner {
            node: Node::File(node),
            node_type,
            reference,
        }))
    }
    pub fn new_dir(
        node_fn: impl FnOnce(WeakDirEntry<M>) -> DirNode<M>,
        reference: Reference<M>,
    ) -> Self {
        Self(Arc::new_cyclic(|this| Inner {
            node: Node::Dir(node_fn(WeakDirEntry(this.clone()))),
            node_type: NodeType::Directory,
            reference,
        }))
    }

    pub fn downcast<T: Send + Sync + 'static>(&self) -> VfsResult<Arc<T>> {
        self.0
            .node
            .clone_inner()
            .into_any()
            .downcast()
            .map_err(|_| VfsError::EINVAL)
    }

    pub fn downgrade(&self) -> WeakDirEntry<M> {
        WeakDirEntry(Arc::downgrade(&self.0))
    }

    pub fn node_type(&self) -> NodeType {
        self.0.node_type
    }
    pub fn parent(&self) -> VfsResult<Option<DirEntry<M>>> {
        self.0
            .reference
            .parent
            .as_ref()
            .map(|it| it.upgrade())
            .transpose()
    }
    pub fn name(&self) -> &str {
        &self.0.reference.name
    }

    pub fn is_ancestor_of(&self, other: &Self) -> VfsResult<bool> {
        let mut current = other.clone();
        loop {
            if current.ptr_eq(self) {
                return Ok(true);
            }
            if let Some(parent) = current.parent()? {
                current = parent;
            } else {
                break;
            }
        }
        Ok(false)
    }

    pub fn absolute_path(&self) -> VfsResult<PathBuf> {
        let mut components = vec![];
        let mut current = self.clone();
        loop {
            components.push(current.name().to_owned());
            if let Some(parent) = current.parent()? {
                current = parent;
            } else {
                break;
            }
        }
        let mut path: PathBuf = "/".into();
        for comp in components.iter().rev() {
            path.push(comp);
        }
        Ok(path)
    }

    pub fn is_file(&self) -> bool {
        matches!(self.0.node, Node::File(_))
    }
    pub fn is_dir(&self) -> bool {
        matches!(self.0.node, Node::Dir(_))
    }

    pub fn as_file(&self) -> VfsResult<&FileNode<M>> {
        match &self.0.node {
            Node::File(file) => Ok(file),
            _ => Err(VfsError::EISDIR),
        }
    }
    pub fn as_dir(&self) -> VfsResult<&DirNode<M>> {
        match &self.0.node {
            Node::Dir(dir) => Ok(dir),
            _ => Err(VfsError::ENOTDIR),
        }
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
