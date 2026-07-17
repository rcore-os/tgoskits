//! Root-context publication and mount-namespace ownership.

#[cfg(feature = "vfs")]
use alloc::vec;
use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
#[cfg(feature = "vfs")]
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "vfs")]
use axfs_ng_vfs::Mountpoint;
use axfs_ng_vfs::{VfsError, VfsResult};

use super::state::FsContext;
#[cfg(feature = "vfs")]
use crate::file::MountIdentity;
#[cfg(feature = "vfs")]
use crate::lifecycle::{FsGenerationAccess, FsOperationLease, FsRuntimeError};
use crate::os::sync::{PiMutex, SpinMutex};

/// Transactionally replaceable root filesystem context.
pub struct RootFsContext {
    publication: SpinMutex<RootFsPublication>,
}

struct RootFsPublication {
    context: Option<FsContext>,
    revision: u64,
}

/// Global root context published by the filesystem mount transaction.
pub static ROOT_FS_CONTEXT: RootFsContext = RootFsContext::new();

/// Registry of all live `FsContext` instances (weak references).
///
/// Each time a task-local [`FS_CONTEXT`] is created, it registers its
/// `Arc<PiMutex<FsContext>>` here via [`register_fs_context`]. This allows
/// [`FsContext::propagate_pivot_root`] to iterate over every task's
/// filesystem context and apply the same root / cwd fixup that Linux
/// performs in `chroot_fs_refs()` after `pivot_root(2)`.
static FS_REGISTRY: SpinMutex<Vec<Weak<PiMutex<FsContext>>>> = SpinMutex::new(Vec::new());
#[cfg(feature = "vfs")]
static MOUNT_NAMESPACE_ID: AtomicU64 = AtomicU64::new(1);

impl RootFsContext {
    const fn new() -> Self {
        Self {
            publication: SpinMutex::new(RootFsPublication {
                context: None,
                revision: 0,
            }),
        }
    }

    /// Returns an owned snapshot suitable for a task-local context.
    pub fn snapshot(&self) -> Option<FsContext> {
        self.publication.lock().context.clone()
    }

    /// Creates and registers a task context without racing root replacement.
    ///
    /// Registration happens before the publication is checked a second time.
    /// A replacement either observes this context in the registry or is
    /// observed by the second snapshot, so a newly created task cannot retain
    /// the old root merely because registration was delayed.
    pub(crate) fn registered_snapshot(&self) -> Option<Arc<PiMutex<FsContext>>> {
        self.registered_snapshot_after_initial(|| {})
    }

    fn registered_snapshot_after_initial(
        &self,
        before_register: impl FnOnce(),
    ) -> Option<Arc<PiMutex<FsContext>>> {
        let (initial, revision) = self.snapshot_with_revision()?;
        let task_context = Arc::new(PiMutex::new(initial));
        before_register();
        register_fs_context(&task_context);

        let (current, current_revision) = self.snapshot_with_revision()?;
        if current_revision != revision {
            *task_context.lock() = current;
        }
        Some(task_context)
    }

    #[cfg(test)]
    pub(crate) fn registered_snapshot_with_interleaving(
        &self,
        before_register: impl FnOnce(),
    ) -> Option<Arc<PiMutex<FsContext>>> {
        self.registered_snapshot_after_initial(before_register)
    }

    fn snapshot_with_revision(&self) -> Option<(FsContext, u64)> {
        let publication = self.publication.lock();
        publication
            .context
            .clone()
            .map(|context| (context, publication.revision))
    }
}

pub(crate) fn install_root_context(context: FsContext) -> VfsResult<()> {
    let mut publication = ROOT_FS_CONTEXT.publication.lock();
    if publication.context.is_some() {
        return Err(VfsError::AlreadyExists);
    }
    publication.context = Some(context);
    publication.revision = publication.revision.wrapping_add(1);
    Ok(())
}

pub(crate) fn replace_root_context(context: FsContext) {
    let previous_generation = {
        let mut publication = ROOT_FS_CONTEXT.publication.lock();
        let previous_generation = publication
            .context
            .replace(context.clone())
            .and_then(|previous| previous.generation());
        publication.revision = publication.revision.wrapping_add(1);
        previous_generation
    };
    let contexts = registered_contexts();
    for task_context in contexts {
        let mut task_context = task_context.lock();
        if task_context.generation() == previous_generation {
            *task_context = context.clone();
        }
    }
}

/// Register an `FsContext` in the global [`FS_REGISTRY`].
pub(crate) fn register_fs_context(ctx: &Arc<PiMutex<FsContext>>) {
    let mut registry = FS_REGISTRY.lock();
    // Prune dead weak references so the registry does not grow unboundedly
    // in long-running scenarios where pivot_root is never invoked.
    registry.retain(|weak| weak.upgrade().is_some());
    registry.push(Arc::downgrade(ctx));
}

/// Returns strong references to every currently registered filesystem context.
///
/// The registry lock is released before callers acquire an individual context
/// lock, which keeps registry publication independent from filesystem work.
pub(super) fn registered_contexts() -> Vec<Arc<PiMutex<FsContext>>> {
    let mut registry = FS_REGISTRY.lock();
    registry.retain(|weak| weak.upgrade().is_some());
    registry.iter().filter_map(Weak::upgrade).collect()
}

/// Returns `true` if any live `FsContext` has its `root_dir` or `current_dir`
/// inside the given `mountpoint`.
#[cfg(feature = "vfs")]
pub fn is_mount_busy(mount: &MountIdentity) -> bool {
    let mp = mount.mountpoint();
    let refs = registered_contexts();
    for ctx_arc in refs {
        let ctx = ctx_arc.lock();
        if !ctx.mount_namespace_contains(mp) {
            continue;
        }
        if Arc::ptr_eq(ctx.root_dir().mountpoint(), mp)
            || Arc::ptr_eq(ctx.current_dir().mountpoint(), mp)
        {
            return true;
        }
    }
    false
}

/// Namespace-local mount tree visible to an [`FsContext`].
#[cfg(feature = "vfs")]
#[derive(Debug, Clone)]
pub struct MountNamespace {
    id: u64,
    root_mount: Arc<Mountpoint>,
    generation_access: Option<FsGenerationAccess>,
}

#[cfg(feature = "vfs")]
impl MountNamespace {
    pub(super) fn new(
        root_mount: Arc<Mountpoint>,
        generation_access: Option<FsGenerationAccess>,
    ) -> Self {
        Self {
            id: MOUNT_NAMESPACE_ID.fetch_add(1, Ordering::Relaxed),
            root_mount,
            generation_access,
        }
    }

    /// Returns a kernel-local identifier for diagnostics.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Returns the root mountpoint of this namespace.
    pub(super) fn root_mount(&self) -> &Arc<Mountpoint> {
        &self.root_mount
    }

    pub(super) fn clone_namespace(&self) -> Arc<Self> {
        Arc::new(Self::new(
            self.root_mount.clone_tree(),
            self.generation_access.clone(),
        ))
    }

    pub(super) fn validate_operation(
        &self,
        operation: Option<&FsOperationLease>,
    ) -> Result<(), FsRuntimeError> {
        match (&self.generation_access, operation) {
            (Some(access), Some(operation)) => access.validate_operation(operation),
            (None, None) => Ok(()),
            _ => Err(FsRuntimeError::InvalidPermit),
        }
    }

    pub(super) fn contains_mountpoint(&self, mountpoint: &Arc<Mountpoint>) -> bool {
        let mut stack = vec![self.root_mount.clone()];
        while let Some(current) = stack.pop() {
            if Arc::ptr_eq(&current, mountpoint) {
                return true;
            }
            stack.extend(current.children());
        }
        false
    }
}

scope_local::scope_local! {
    /// Task-local filesystem context, defaulting to a clone of [`ROOT_FS_CONTEXT`].
    pub static FS_CONTEXT: Arc<PiMutex<FsContext>> = {
        ROOT_FS_CONTEXT
            .registered_snapshot()
            .expect("Root FS context not initialized")
    };
}

/// Returns an owned reference to the filesystem context of the active scope.
///
/// CPU pinning only covers the `Arc` clone. Callers may therefore acquire the
/// sleepable filesystem lock after preemption has been restored.
pub fn current_fs_context() -> Arc<PiMutex<FsContext>> {
    FS_CONTEXT.clone_current()
}
