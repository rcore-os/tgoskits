use alloc::sync::Arc;

use inherit_methods_macro::inherit_methods;

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
pub trait FilesystemOps: Send + Sync {
    /// Gets the name of the filesystem
    fn name(&self) -> &str;

    /// Gets the root directory entry of the filesystem
    fn root_dir(&self) -> DirEntry;

    /// Returns whether the filesystem is cacheable
    ///
    /// Special filesystems like `procfs` or `sysfs` are not cacheable. This
    /// flag serves as a mere hint for upper layers and does not enforce any
    /// behavior.
    fn is_cacheable(&self) -> bool;

    /// Returns statistics about the filesystem
    fn stat(&self) -> VfsResult<StatFs>;
}

pub struct Filesystem {
    ops: Arc<dyn FilesystemOps>,
}

impl Clone for Filesystem {
    fn clone(&self) -> Self {
        Self {
            ops: self.ops.clone(),
        }
    }
}

#[inherit_methods(from = "self.ops")]
impl Filesystem {
    pub fn name(&self) -> &str;

    pub fn root_dir(&self) -> DirEntry;

    pub fn stat(&self) -> VfsResult<StatFs>;
}

impl Filesystem {
    pub fn new(ops: Arc<dyn FilesystemOps>) -> Self {
        Self { ops }
    }
}
