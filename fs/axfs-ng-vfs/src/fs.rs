use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

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

/// Shared ownership of a filesystem's mounts, including detached open files.
///
/// Implementations may retire filesystem caches on final release. Destruction
/// can perform blocking I/O and must occur outside mount topology locks.
pub trait FilesystemMountLease: Send + Sync + core::fmt::Debug {}

/// Trait for filesystem operations
pub trait FilesystemOps: Send + Sync {
    /// Gets the name of the filesystem
    fn name(&self) -> &str;

    /// Returns whether this filesystem was mounted read-only.
    fn is_readonly(&self) -> bool {
        false
    }

    /// Gets the root directory entry of the filesystem
    fn root_dir(&self) -> DirEntry;

    /// Retains mount ownership separately from cached inode references.
    /// Bind mounts and namespace copies retain the same filesystem lease.
    fn mount_lease(&self) -> Option<Arc<dyn FilesystemMountLease>> {
        None
    }

    /// Returns statistics about the filesystem
    fn stat(&self) -> VfsResult<StatFs>;

    /// Flushes the filesystem, ensuring all data is written to disk
    fn flush(&self) -> VfsResult<()> {
        Ok(())
    }

    /// Flushes and shuts down the filesystem before its backing device becomes unavailable.
    fn shutdown(&self) -> VfsResult<()> {
        self.flush()
    }
}

/// VFS superblock flags shared by every mount of a filesystem instance.
#[derive(Debug)]
pub(crate) struct FilesystemMountState {
    readonly: AtomicBool,
}

impl FilesystemMountState {
    pub(crate) fn new(readonly: bool) -> Self {
        Self {
            readonly: AtomicBool::new(readonly),
        }
    }

    pub(crate) fn is_readonly(&self) -> bool {
        self.readonly.load(Ordering::Acquire)
    }

    pub(crate) fn set_readonly(&self, readonly: bool) {
        self.readonly.store(readonly, Ordering::Release);
    }
}

#[derive(Clone)]
pub struct Filesystem {
    ops: Arc<dyn FilesystemOps>,
    pub(crate) mount_state: Arc<FilesystemMountState>,
}

#[inherit_methods(from = "self.ops")]
impl Filesystem {
    pub fn name(&self) -> &str;

    pub fn root_dir(&self) -> DirEntry;

    pub fn stat(&self) -> VfsResult<StatFs>;

    pub fn shutdown(&self) -> VfsResult<()>;
}

impl Filesystem {
    /// Creates a filesystem instance. Clone this handle to share its mounts.
    pub fn new(ops: Arc<dyn FilesystemOps>) -> Self {
        let mount_state = Arc::new(FilesystemMountState::new(ops.is_readonly()));
        Self { ops, mount_state }
    }

    /// Returns the VFS read-only state shared by all mounts of this instance.
    pub fn is_readonly(&self) -> bool {
        self.mount_state.is_readonly()
    }

    /// Configures VFS write protection before publishing a new mount.
    /// This does not change the capabilities of the backing device.
    pub fn set_readonly(&self, readonly: bool) {
        self.mount_state.set_readonly(readonly);
    }
}
