use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use inherit_methods_macro::inherit_methods;

use crate::{CachedWriteAdmission, DirEntry, VfsResult, WritebackPolicy};

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

    /// Returns the optional boundary used to drain buffered writes and mmap
    /// dirtying before shutdown. Internal writeback uses a separate boundary.
    fn cached_write_admission(&self) -> Option<&dyn CachedWriteAdmission> {
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
    writeback_policy: AtomicU8,
}

impl FilesystemMountState {
    pub(crate) fn new(readonly: bool) -> Self {
        Self {
            readonly: AtomicBool::new(readonly),
            writeback_policy: AtomicU8::new(WritebackPolicy::empty().bits()),
        }
    }

    pub(crate) fn is_readonly(&self) -> bool {
        self.readonly.load(Ordering::Acquire)
    }

    pub(crate) fn set_readonly(&self, readonly: bool) {
        self.readonly.store(readonly, Ordering::Release);
    }

    pub(crate) fn writeback_policy(&self) -> WritebackPolicy {
        WritebackPolicy::from_bits_retain(self.writeback_policy.load(Ordering::Acquire))
    }

    pub(crate) fn set_writeback_policy(&self, policy: WritebackPolicy) {
        self.writeback_policy
            .store(policy.bits(), Ordering::Release);
    }

    pub(crate) fn set_synchronous(&self, synchronous: bool) {
        // Only this bit is mutable on Linux remount. Preserve directory sync,
        // including when another mount concurrently updates the shared policy.
        if synchronous {
            self.writeback_policy
                .fetch_or(WritebackPolicy::SYNCHRONOUS.bits(), Ordering::AcqRel);
        } else {
            self.writeback_policy
                .fetch_and(!WritebackPolicy::SYNCHRONOUS.bits(), Ordering::AcqRel);
        }
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

    /// Sets persistence requirements before publishing the initial mount.
    /// Clones and bind mounts share this policy rather than copying its value.
    pub fn set_writeback_policy(&self, policy: WritebackPolicy) {
        self.mount_state.set_writeback_policy(policy);
    }
}
