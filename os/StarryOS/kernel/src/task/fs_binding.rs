//! Coherent task-local ownership of shared paths and mount authorization.

use alloc::sync::Arc;

use ax_fs_ng::{FS_CONTEXT, FsContextOwner, FsContextSnapshot};
use scope_local::Scope;

use super::{FsCredentialsSnapshot, ProcessNamespaceUpdate, Thread};
use crate::{
    StarryError, StarryResult,
    file::CachedFileTable,
    namespace::{MNT_NAMESPACE, MntNamespace},
};

/// One frozen selection; its paths cannot promote themselves to a task owner.
pub(crate) struct FsBindingSnapshot {
    pub(crate) fs: FsContextSnapshot,
    pub(crate) mnt: Arc<MntNamespace>,
}

/// Cache-only inputs selected once, with every capture guard already released.
pub(crate) struct CachedFsBinding {
    pub(crate) fs: FsContextSnapshot,
    pub(crate) credentials: FsCredentialsSnapshot,
    pub(crate) files: CachedFileTable,
}

impl FsBindingSnapshot {
    pub(super) fn from_scope(scope: &Scope) -> StarryResult<Option<Self>> {
        let owner = FS_CONTEXT.scope(scope);
        let mnt = MNT_NAMESPACE.scope(scope);
        match (owner.as_ref(), mnt.as_ref()) {
            (None, None) => Ok(None),
            (Some(owner), Some(mnt)) if Arc::ptr_eq(owner.namespace(), mnt.core()) => {
                Ok(Some(Self {
                    fs: owner.snapshot(),
                    mnt: mnt.clone(),
                }))
            }
            _ => Err(StarryError::BadState),
        }
    }
}

impl Thread {
    pub(crate) fn try_cached_fs_binding(&self) -> StarryResult<CachedFsBinding> {
        let update = self
            .proc_data
            .try_namespace_update()
            .ok_or(StarryError::WouldBlock)?;
        let selected = self.scope.try_read(|scope| {
            let owner = FS_CONTEXT.scope(scope);
            let mnt = MNT_NAMESPACE.scope(scope);
            let (Some(owner), Some(mnt)) = (owner.as_ref(), mnt.as_ref()) else {
                return Err(StarryError::BadState);
            };
            if !Arc::ptr_eq(owner.namespace(), mnt.core()) {
                return Err(StarryError::BadState);
            }
            let credentials = FsCredentialsSnapshot::try_capture_under(self, &update)?;
            let files = CachedFileTable::from_scope(
                scope,
                self.proc_data
                    .rlimit_current(linux_raw_sys::general::RLIMIT_NOFILE),
            );
            // This must remain the last fallible capture. A different process
            // sharing CLONE_FS may retire the copied cwd at any time, making
            // this result its last owner while our publication gate is held.
            let fs = owner.try_snapshot()?;
            Ok(CachedFsBinding {
                fs,
                credentials,
                files,
            })
        });
        drop(update);
        selected.ok_or(StarryError::WouldBlock)?
    }

    /// Selects both entries under target writer -> scope access -> path lock.
    /// Every lease ends before the owned snapshot reaches its caller.
    pub(crate) fn snapshot_fs_binding(&self) -> StarryResult<Option<FsBindingSnapshot>> {
        let update = self.proc_data.namespace_update();
        self.snapshot_fs_binding_under(&update)
    }

    pub(crate) fn snapshot_fs_binding_under(
        &self,
        update: &ProcessNamespaceUpdate<'_>,
    ) -> StarryResult<Option<FsBindingSnapshot>> {
        assert!(update.owns(&self.proc_data));
        self.scope.read(FsBindingSnapshot::from_scope)
    }

    /// Retires the complete binding before zombie publication. The returned
    /// owners must be released after the writer and pinned scope access end.
    pub(crate) fn retire_fs_binding(
        &self,
    ) -> StarryResult<(Option<FsContextOwner>, Option<Arc<MntNamespace>>)> {
        let update = self.proc_data.namespace_update();
        let retired = self.with_current_scope_mut(|scope| {
            {
                let owner = FS_CONTEXT.scope(scope);
                let mnt = MNT_NAMESPACE.scope(scope);
                match (owner.as_ref(), mnt.as_ref()) {
                    (None, None) => {}
                    (Some(owner), Some(mnt)) if Arc::ptr_eq(owner.namespace(), mnt.core()) => {}
                    _ => return Err(StarryError::BadState),
                }
            }
            let owner = FS_CONTEXT.scope_mut(scope).take();
            let mnt = MNT_NAMESPACE.scope_mut(scope).take();
            Ok((owner, mnt))
        });
        drop(update);
        retired
    }
}
