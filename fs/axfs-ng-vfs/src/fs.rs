use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use ax_sync::Mutex;
use inherit_methods_macro::inherit_methods;

use crate::{CachedWriteAdmission, DirEntry, Location, RawSpinLock, VfsError, VfsResult};

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
    /// Whether this owner implements the closed native namespace boundary.
    fn has_native_namespace(&self) -> bool {
        false
    }

    /// Acquires only the existing operation admission, never a backend gate.
    /// The VFS binds the returned guard to this receiver before exposing it.
    fn enter_namespace(&self) -> VfsResult<alloc::boxed::Box<dyn crate::NamespaceOperation + '_>> {
        Ok(alloc::boxed::Box::new(()))
    }

    /// Backend implementation hook. The request is privately constructed by
    /// the same-FS admission factory; this is not a public raw-attempt factory.
    fn begin_namespace<'a>(
        &'a self,
        request: crate::NamespaceDriverRequest<'a>,
    ) -> crate::NamespaceStart<'a> {
        request.fail(VfsError::OperationNotSupported)
    }

    /// Admitted native reads, without ordinary entry points that may reenter
    /// operation admission. The caller holds no native backend gate.
    fn query_namespace(
        &self,
        _admission: &crate::NamespaceAdmission<'_>,
        _query: crate::NamespaceQuery<'_>,
        _retired: &crate::MutationRetire,
    ) -> VfsResult<crate::NamespaceRead> {
        Err(VfsError::OperationNotSupported)
    }

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

    /// Returns the immutable stacking depth, without I/O or backend gates.
    /// Native filesystems have depth zero; all backing edges must descend.
    fn stack_depth(&self) -> u32 {
        0
    }

    /// Returns immutable writable backing roots without I/O or backend gates.
    /// Dependencies used only for reading belong in [`Self::read_backing_roots`].
    fn write_backing_roots(&self) -> &[Location] {
        &[]
    }

    /// Returns immutable read-only backing roots without I/O or backend gates.
    /// Write preparation pins these locations for the operation-admission
    /// factory without granting mount write authority. Reading a layered root
    /// also prepares all of its dependencies as read-only, including its
    /// writable roots.
    fn read_backing_roots(&self) -> &[Location] {
        &[]
    }

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

/// Stable identity shared by every mount of one filesystem instance.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FilesystemId(u64);

static NEXT_FILESYSTEM_ID: AtomicU64 = AtomicU64::new(1);

/// VFS superblock flags shared by every mount of a filesystem instance.
pub(crate) struct FilesystemMountState {
    pub(crate) id: FilesystemId,
    /// Lock order: reconfiguration, shared writes, mount-local writes.
    pub(crate) reconfiguration: Mutex<()>,
    pub(crate) writes: RawSpinLock<MountWriteState>,
}

impl core::fmt::Debug for FilesystemMountState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FilesystemMountState")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Flags and counts are observed together during write admission.
#[derive(Debug)]
pub(crate) struct MountWriteState {
    pub(crate) readonly: bool,
    pub(crate) writers: usize,
}

impl MountWriteState {
    pub(crate) fn new(readonly: bool) -> Self {
        Self {
            readonly,
            writers: 0,
        }
    }
}

impl FilesystemMountState {
    pub(crate) fn new(readonly: bool) -> Self {
        let id = NEXT_FILESYSTEM_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("filesystem identity space exhausted");
        Self {
            id: FilesystemId(id),
            reconfiguration: Mutex::new(()),
            writes: RawSpinLock::new(MountWriteState::new(readonly)),
        }
    }

    pub(crate) fn is_readonly(&self) -> bool {
        self.writes.lock().readonly
    }

    /// Caller holds reconfiguration, but no state or backend lock across I/O.
    pub(crate) fn prepare_readonly(
        &self,
        ops: &dyn FilesystemOps,
        readonly: bool,
    ) -> VfsResult<()> {
        let previous = {
            let state = self.writes.lock();
            if readonly && !state.readonly && state.writers != 0 {
                return Err(VfsError::ResourceBusy);
            }
            state.readonly
        };
        match (previous, readonly) {
            (false, true) => ops.flush(),
            (true, false) => ops.remount_read_write(),
            _ => Ok(()),
        }
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

    pub fn root_dir(&self) -> DirEntry;

    pub fn stat(&self) -> VfsResult<StatFs>;

    pub fn shutdown(&self) -> VfsResult<()>;
}

impl Filesystem {
    /// Creates a filesystem instance. Clone this handle to share its mounts.
    pub fn new(ops: Arc<dyn FilesystemOps>) -> Self {
        Self::new_with_readonly(ops, false)
    }

    /// Sets initial VFS protection without entering reconfiguration.
    /// Native write protection cannot be cleared by construction; querying it
    /// retains the backend's own synchronization requirements.
    pub fn new_with_readonly(ops: Arc<dyn FilesystemOps>, readonly: bool) -> Self {
        let mount_state = Arc::new(FilesystemMountState::new(readonly || ops.is_readonly()));
        Self { ops, mount_state }
    }

    /// Returns the VFS read-only state shared by all mounts of this instance.
    pub fn is_readonly(&self) -> bool {
        self.mount_state.is_readonly()
    }

    /// Reconfigures shared VFS protection. Only a read-write to read-only
    /// transition rejects active writers with `ResourceBusy`; same-state
    /// requests do not reconfigure the backend.
    /// This may sleep and perform synchronization/native I/O.
    /// Failure leaves VFS flags unchanged. Mount-local flags are not changed;
    /// use `Mountpoint::set_filesystem_readonly` for an ordinary remount.
    pub fn set_readonly(&self, readonly: bool) -> VfsResult<()> {
        let _configuration = self.mount_state.reconfiguration.lock();
        self.mount_state
            .prepare_readonly(self.ops.as_ref(), readonly)?;
        self.mount_state.writes.lock().readonly = readonly;
        Ok(())
    }
}
