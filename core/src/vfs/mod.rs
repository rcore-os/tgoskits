//! Basic virtual filesystem support

mod dev;
mod dir;
mod file;
mod fs;

use alloc::sync::Arc;

use axfs_ng_vfs::{DirNodeOps, FileNodeOps, WeakDirEntry};
pub use dev::*;
pub use dir::*;
pub use file::*;
pub use fs::*;

/// A callback that builds a `Arc<dyn DirNodeOps<M>>` for a given
/// `WeakDirEntry<M>`.
pub type DirMaker<M = axsync::RawMutex> =
    Arc<dyn Fn(WeakDirEntry<M>) -> Arc<dyn DirNodeOps<M>> + Send + Sync>;

/// An enum containing either a directory ([`DirMaker`]) or a file (`Arc<dyn
/// FileNodeOps<M>>`).
pub enum NodeOpsMux<M> {
    /// A directory node.
    Dir(DirMaker<M>),
    /// A file node.
    File(Arc<dyn FileNodeOps<M>>),
}

impl<M> Clone for NodeOpsMux<M> {
    fn clone(&self) -> Self {
        match self {
            NodeOpsMux::Dir(maker) => NodeOpsMux::Dir(maker.clone()),
            NodeOpsMux::File(ops) => NodeOpsMux::File(ops.clone()),
        }
    }
}

impl<M> From<DirMaker<M>> for NodeOpsMux<M> {
    fn from(maker: DirMaker<M>) -> Self {
        Self::Dir(maker)
    }
}

impl<M, T: FileNodeOps<M> + 'static> From<Arc<T>> for NodeOpsMux<M> {
    fn from(ops: Arc<T>) -> Self {
        Self::File(ops)
    }
}
