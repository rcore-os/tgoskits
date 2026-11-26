use alloc::sync::Arc;

use axfs_ng_vfs::{CreatedMountNamespace, MountNamespace};

use super::{ROOT_USER_NS, UserNamespace};
use crate::sync::RawSpinLock;

/// The sole initial adapter is bound to the boot Core and initial user namespace.
pub static ROOT_MNT_NS: ax_lazyinit::LazyLock<Arc<MntNamespace>> =
    ax_lazyinit::LazyLock::new(|| {
        Arc::new(MntNamespace {
            core: ax_fs_ng::ROOT_FS_CONTEXT
                .get()
                .expect("root filesystem must precede task scopes")
                .namespace()
                .clone(),
            creator: ROOT_USER_NS.clone(),
        })
    });

scope_local::scope_local! {
    /// Paired with FS_CONTEXT in the same task scope, including after exit.
    pub(crate) static MNT_NAMESPACE: Option<Arc<MntNamespace>> = Some(ROOT_MNT_NS.clone());
}

/// Immutable OS authorization binding for one actual VFS mount namespace.
/// A newly created Core can acquire a creator only by consuming its provenance.
pub struct MntNamespace {
    core: Arc<MountNamespace>,
    creator: Arc<RawSpinLock<UserNamespace>>,
}

impl MntNamespace {
    pub(crate) fn from_created(
        created: CreatedMountNamespace,
        creator: Arc<RawSpinLock<UserNamespace>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            core: created.into_namespace(),
            creator,
        })
    }

    pub(crate) fn core(&self) -> &Arc<MountNamespace> {
        &self.core
    }

    pub(crate) fn creator(&self) -> &Arc<RawSpinLock<UserNamespace>> {
        &self.creator
    }
}
