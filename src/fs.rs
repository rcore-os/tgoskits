use alloc::sync::Arc;
use lock_api::RawMutex;

use crate::DirEntry;

/// Trait for filesystem operations
pub trait FilesystemOps<M>: Send + Sync {
    fn root_dir(&self) -> DirEntry<M>;
}

pub struct Filesystem<M> {
    ops: Arc<dyn FilesystemOps<M>>,
}
impl<M> Clone for Filesystem<M> {
    fn clone(&self) -> Self {
        Self {
            ops: self.ops.clone(),
        }
    }
}

impl<M: RawMutex> Filesystem<M> {
    pub fn new(ops: Arc<dyn FilesystemOps<M>>) -> Self {
        Self { ops }
    }

    pub fn root_dir(&self) -> DirEntry<M> {
        self.ops.root_dir()
    }
}
