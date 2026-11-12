use alloc::sync::Arc;

use super::{
    Location, MOUNT_TOPOLOGY_MUTATION, MountUseGuard, Mountpoint, UnmountKind, UnmountPlan,
};
use crate::{VfsError, VfsResult, namespace_admission::owner_address};

/// Owns a location and one shared, counted use of its exact mount.
///
/// This is not proof of name linkage, scope, permissions, or write access.
/// Clones and same-mount rebindings share the original use without acquiring
/// the topology gate. Long-lived consumers must retain this handle, not just
/// clone its borrowed [`Location`].
///
/// ```
/// use axfs_ng_vfs::{Location, PathHandle, VfsResult};
///
/// fn retain_child(parent: Location) -> VfsResult<PathHandle> {
///     let parent = PathHandle::acquire(parent)?;
///     let child = parent.location().lookup_no_follow("child")?;
///     parent.rebind(child) // Fails if lookup crossed into another mount.
/// }
/// ```
#[derive(Debug, Clone)]
pub struct PathHandle {
    // Declaration order keeps inode/backend retirement inside the mount use.
    location: Location,
    guard: Arc<MountUseGuard>,
}

impl PathHandle {
    /// Checks the actual filesystem owner, then acquires a counted mount use.
    ///
    /// Returns [`VfsError::CrossesDevices`] for a mismatched entry, or
    /// [`VfsError::NotFound`] if normal unmount already removed the mount.
    /// The latter is an acquisition failure, not a missing pathname.
    ///
    /// May sleep waiting for the topology gate. Call outside cache, context,
    /// descriptor-table, and backend locks; this is not a CACHED fallback.
    pub fn acquire(location: Location) -> VfsResult<Self> {
        Self::check_filesystem(&location)?;
        let guard = Arc::new(location.mountpoint.acquire_use()?);
        Ok(Self { location, guard })
    }

    /// Shares this use with another entry on the exact same mount.
    ///
    /// A different mount (including a bind of the same filesystem), or a
    /// mismatched actual filesystem owner, returns [`VfsError::CrossesDevices`].
    /// Success allocates nothing and does not acquire the topology gate.
    pub fn rebind(&self, location: Location) -> VfsResult<Self> {
        if !Arc::ptr_eq(&self.location.mountpoint, &location.mountpoint) {
            return Err(VfsError::CrossesDevices);
        }
        Self::check_filesystem(&location)?;
        Ok(Self {
            location,
            guard: self.guard.clone(),
        })
    }

    /// Borrows the location while this handle retains its mount use.
    ///
    /// Cloning this location does not grant a separate use or extend this one.
    pub fn location(&self) -> &Location {
        &self.location
    }

    /// Keeps ownership in the caller across validation failures. The caller
    /// must retain its guard before invoking this fallible internal bridge.
    pub(in crate::mount) fn from_mount_use(
        location: &Location,
        guard: &Arc<MountUseGuard>,
    ) -> VfsResult<Self> {
        if !Arc::ptr_eq(&location.mountpoint, &guard.mountpoint) {
            return Err(VfsError::CrossesDevices);
        }
        Self::check_filesystem(location)?;
        Ok(Self {
            location: location.clone(),
            guard: guard.clone(),
        })
    }

    pub(in crate::mount) fn check_filesystem(location: &Location) -> VfsResult<()> {
        if owner_address(location.entry.filesystem())
            != owner_address(location.mountpoint.root.filesystem())
        {
            return Err(VfsError::CrossesDevices);
        }
        Ok(())
    }

    /// Consumes an exclusive mount-root use to prepare a normal unmount.
    ///
    /// Non-mount-root entries and parentless mounts return
    /// [`VfsError::InvalidInput`] before checking shared ownership. Shared uses
    /// return [`VfsError::ResourceBusy`]. Original planning errors propagate;
    /// no failure releases the use before retiring this handle's location.
    pub fn prepare_unmount(self) -> VfsResult<PathUnmount> {
        if !self.location.is_root_of_mount() || self.location.mountpoint.is_root() {
            return Err(VfsError::InvalidInput);
        }
        let Self { location, guard } = self;
        let guard = match Arc::try_unwrap(guard) {
            Ok(guard) => guard,
            Err(guard) => {
                drop(Self { location, guard });
                return Err(VfsError::ResourceBusy);
            }
        };
        let planned = {
            let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
            location
                .mountpoint
                .plan_unmount_locked_with_use(UnmountKind::Normal, Some(&guard))
        };
        // Restore the owning layout outside the gate on both success and
        // failure. Never return the counted use and reacquire it for the plan.
        let target = Self {
            location,
            guard: Arc::new(guard),
        };
        match planned {
            Ok(mut plan) => {
                // Exclusive recovery above permits only these two internal
                // owners. They still account for exactly one real mount use.
                plan.target_use = Some(target.guard.clone());
                Ok(PathUnmount { target, plan })
            }
            Err(error) => {
                drop(target);
                Err(error)
            }
        }
    }
}

/// An opaque, single-use normal-unmount transaction owning its target path.
///
/// Dropping this value only releases resources; it does not flush or detach.
/// Neither the target handle nor its guard or inner plan can be extracted.
///
/// ```
/// use axfs_ng_vfs::{PathHandle, VfsResult};
///
/// fn unmount(target: PathHandle) -> VfsResult<()> {
///     let pending = target.prepare_unmount()?;
///     for mount in pending.targets() {
///         let _device = mount.device(); // Additional caller-side busy checks.
///     }
///     pending.commit()
/// }
/// ```
///
/// ```compile_fail,E0382
/// use axfs_ng_vfs::{PathUnmount, VfsResult};
///
/// fn commit_twice(pending: PathUnmount) -> VfsResult<()> {
///     pending.commit()?;
///     pending.commit() // The first commit moved the transaction.
/// }
/// ```
#[derive(Debug)]
pub struct PathUnmount {
    target: PathHandle,
    plan: UnmountPlan,
}

impl PathUnmount {
    /// Borrows the original propagation targets for additional busy checks.
    pub fn targets(&self) -> impl Iterator<Item = &Arc<Mountpoint>> {
        self.plan.targets()
    }

    /// Flushes and commits through the original normal-unmount protocol.
    ///
    /// The private target keeps its use through flush errors, post-flush
    /// revalidation errors, and successful cache retirement.
    pub fn commit(self) -> VfsResult<()> {
        let Self { target, plan } = self;
        let result = target.location().commit_unmount(plan);
        drop(target);
        result
    }
}
