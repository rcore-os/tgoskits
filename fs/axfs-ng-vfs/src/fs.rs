use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use inherit_methods_macro::inherit_methods;

use crate::{CachedWriteAdmission, DirEntry, VfsResult};

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

    /// Returns whether the native filesystem backend currently rejects writes.
    fn is_readonly(&self) -> bool {
        false
    }

    /// Reconfigures a native read-only backend before VFS publishes write access.
    /// Implementations retain the same filesystem and backing owner. A failure
    /// must keep write protection; devices without writable authority refuse it.
    fn remount_read_write(&self) -> VfsResult<()> {
        if self.is_readonly() {
            Err(crate::VfsError::PermissionDenied)
        } else {
            Ok(())
        }
    }

    /// Gets the root directory entry of the filesystem
    fn root_dir(&self) -> DirEntry;

    /// Retains mount ownership separately from cached inode references.
    /// Bind mounts and namespace copies retain the same filesystem lease.
    ///
    /// `Ok(None)` means the filesystem has no mount-scoped lifetime owner.
    /// Filesystems with such an owner return an error once a new mount can no
    /// longer acquire it.
    fn mount_lease(&self) -> VfsResult<Option<Arc<dyn FilesystemMountLease>>> {
        Ok(None)
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

/// Stable identity shared by every mount of one filesystem instance.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FilesystemId(u64);

static NEXT_FILESYSTEM_ID: AtomicU64 = AtomicU64::new(1);

/// VFS superblock flags shared by every mount of a filesystem instance.
#[derive(Debug)]
pub(crate) struct FilesystemMountState {
    pub(crate) id: FilesystemId,
    readonly: AtomicBool,
}

impl FilesystemMountState {
    pub(crate) fn new(readonly: bool) -> Self {
        let id = NEXT_FILESYSTEM_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("filesystem identity space exhausted");
        Self {
            id: FilesystemId(id),
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

/// A registry entry that does not extend a filesystem's lifetime.
#[derive(Clone)]
pub struct WeakFilesystem {
    ops: alloc::sync::Weak<dyn FilesystemOps>,
    mount_state: alloc::sync::Weak<FilesystemMountState>,
}

impl WeakFilesystem {
    /// Reports whether both filesystem owners are still present.
    pub fn is_alive(&self) -> bool {
        self.ops.strong_count() != 0 && self.mount_state.strong_count() != 0
    }

    /// Retains the instance only while a mount, file, or explicit owner is live.
    pub fn upgrade(&self) -> Option<Filesystem> {
        Some(Filesystem {
            ops: self.ops.upgrade()?,
            mount_state: self.mount_state.upgrade()?,
        })
    }

    /// Retains the filesystem implementation without reconstructing VFS mount
    /// state.
    ///
    /// Cached inodes can keep a filesystem alive after its last Mountpoint is
    /// detached. Callers that only need filesystem-owned flush or retirement
    /// work can use this entry. It does not itself prevent a caller from
    /// constructing a fresh [`Filesystem`] around the ops; mount revival is
    /// rejected by [`FilesystemOps::mount_lease`] outside the active lifecycle,
    /// and I/O safety additionally depends on the filesystem's admission
    /// gates.
    pub fn upgrade_ops(&self) -> Option<Arc<dyn FilesystemOps>> {
        self.ops.upgrade()
    }
}

#[inherit_methods(from = "self.ops")]
impl Filesystem {
    /// Creates a non-owning entry for shutdown and diagnostics registries.
    pub fn downgrade(&self) -> WeakFilesystem {
        WeakFilesystem {
            ops: Arc::downgrade(&self.ops),
            mount_state: Arc::downgrade(&self.mount_state),
        }
    }
    pub fn name(&self) -> &str;

    /// Retains the current mount generation, or reports that a new mount can
    /// no longer acquire one.
    pub fn mount_lease(&self) -> VfsResult<Option<Arc<dyn FilesystemMountLease>>>;

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
