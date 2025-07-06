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

pub type DirMaker<M = axsync::RawMutex> =
    Arc<dyn Fn(WeakDirEntry<M>) -> Arc<dyn DirNodeOps<M>> + Send + Sync>;

pub enum NodeOpsMux<M> {
    Dir(DirMaker<M>),
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
