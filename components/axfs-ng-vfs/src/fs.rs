use alloc::sync::Arc;

use inherit_methods_macro::inherit_methods;

use crate::{DirEntry, VfsResult};

/// Describes whether open locations survive a root-filesystem handoff.
///
/// Disk-backed and overlay filesystems are detachable by default. Synthetic
/// kernel filesystems may opt into [`Self::NonDetachable`] when their nodes do
/// not depend on a controller, mount recipe, or detachable backing store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemDetachPolicy {
    /// Locations must remain tied to the publishing filesystem generation.
    Detachable,
    /// The filesystem is kernel-owned and remains valid across root handoff.
    NonDetachable,
}

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

    /// Returns whether this filesystem was mounted read-only.
    fn is_readonly(&self) -> bool {
        false
    }

    /// Returns the handoff policy for locations created by this filesystem.
    fn detach_policy(&self) -> FilesystemDetachPolicy {
        FilesystemDetachPolicy::Detachable
    }

    /// Gets the root directory entry of the filesystem
    fn root_dir(&self) -> DirEntry;

    /// Returns statistics about the filesystem
    fn stat(&self) -> VfsResult<StatFs>;

    /// Flushes the filesystem, ensuring all data is written to disk
    fn flush(&self) -> VfsResult<()> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct Filesystem {
    ops: Arc<dyn FilesystemOps>,
}

#[inherit_methods(from = "self.ops")]
impl Filesystem {
    pub fn name(&self) -> &str;

    pub fn is_readonly(&self) -> bool;

    pub fn detach_policy(&self) -> FilesystemDetachPolicy;

    pub fn root_dir(&self) -> DirEntry;

    pub fn stat(&self) -> VfsResult<StatFs>;
}

impl Filesystem {
    pub fn new(ops: Arc<dyn FilesystemOps>) -> Self {
        Self { ops }
    }
}
