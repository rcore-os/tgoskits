use alloc::sync::Arc;

use inherit_methods_macro::inherit_methods;
use lock_api::RawMutex;

use crate::{DirEntry, VfsResult};

pub struct StatFs {
    pub fs_type: u32,
    pub block_size: u32,
    pub blocks: u64,
    pub blocks_free: u64,
    pub blocks_available: u64,

    pub file_count: u64,
    pub free_file_count: u64,

    pub name_length: u32,
    pub fragment_size: u32,
    pub mount_flags: u32,
}

/// Trait for filesystem operations
pub trait FilesystemOps<M>: Send + Sync {
    fn name(&self) -> &str;
    fn root_dir(&self) -> DirEntry<M>;
    fn stat(&self) -> VfsResult<StatFs>;
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

    pub fn stat(&self) -> VfsResult<StatFs>;
}

impl<M: RawMutex> Filesystem<M> {
    pub fn new(ops: Arc<dyn FilesystemOps<M>>) -> Self {
        Self { ops }
    }
}
