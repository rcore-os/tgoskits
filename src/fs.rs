use alloc::sync::Arc;
use inherit_methods_macro::inherit_methods;
use lock_api::RawMutex;

use crate::DirEntry;

/// Trait for filesystem operations
pub trait FilesystemOps<M>: Send + Sync {
    fn name(&self) -> &str;
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

#[inherit_methods(from = "self.ops")]
impl<M: RawMutex> Filesystem<M> {
    pub fn name(&self) -> &str;
    pub fn root_dir(&self) -> DirEntry<M>;
}

impl<M: RawMutex> Filesystem<M> {
    pub fn new(ops: Arc<dyn FilesystemOps<M>>) -> Self {
        Self { ops }
    }
}
