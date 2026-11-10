use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{StarryError, StarryResult, sync::RawSpinLock};

/// The initial root user namespace, shared by all processes until
/// they call `unshare(CLONE_NEWUSER)` or `clone(CLONE_NEWUSER)`.
pub static ROOT_USER_NS: ax_lazyinit::LazyLock<Arc<RawSpinLock<UserNamespace>>> =
    ax_lazyinit::LazyLock::new(|| Arc::new(RawSpinLock::new(UserNamespace::new_root())));

static NEXT_USER_NS_ID: AtomicU64 = AtomicU64::new(1);

/// Per-process user namespace.
///
/// Isolates UID/GID mappings so that a process may have uid 0 inside
/// its namespace while mapping to an unprivileged UID in the parent.
/// `owner_uid` is the effective UID of the process that created the
/// namespace (0 for the root namespace).
///
/// When neither `uid_mapped` nor `gid_mapped` is true (child namespace
/// without configured mappings), all UIDs/GIDs are reported as `nobody`
/// (65534), matching Linux default behaviour for unconfigured user
/// namespaces.  Writing uid_map sets `uid_mapped`, writing gid_map sets
/// `gid_mapped`, so the two sides are independent — a half-configured
/// namespace correctly returns 65534 for the unmapped side.
pub struct UserNamespace {
    /// Globally unique namespace identifier (exposed via /proc/PID/ns/user).
    pub id: u64,
    lineage: UserNamespaceLineage,
    /// Whether this is the initial root user namespace or a child
    /// namespace that had both uid_map and gid_map configured.  Kept
    /// for compatibility; new code should use `uid_mapped` /
    /// `gid_mapped` for per-side overflow control.
    pub is_root: bool,
    /// Whether uid_map has been written — lifts UID-side overflow.
    pub uid_mapped: bool,
    /// Whether gid_map has been written — lifts GID-side overflow.
    pub gid_mapped: bool,
}

/// Immutable ancestry, independent of the mutable UID/GID mapping flags.
#[derive(Clone)]
pub(crate) struct UserNamespaceLineage {
    pub(crate) parent: Option<Arc<RawSpinLock<UserNamespace>>>,
    pub(crate) level: usize,
    /// Creator's effective UID in the parent namespace.
    pub(crate) owner_uid: u32,
}

impl UserNamespace {
    pub fn new_root() -> Self {
        Self {
            id: NEXT_USER_NS_ID.fetch_add(1, Ordering::Relaxed),
            lineage: UserNamespaceLineage {
                parent: None,
                level: 0,
                owner_uid: 0,
            },
            is_root: true,
            uid_mapped: true,
            gid_mapped: true,
        }
    }

    pub(crate) fn new_child(
        parent: Arc<RawSpinLock<Self>>,
        creator_euid: u32,
    ) -> StarryResult<Self> {
        let parent_level = parent.lock_irqsave().lineage.level;
        // Linux create_user_ns permits a level-32 parent to create level 33.
        // Reject before assigning an identity or retaining another parent link.
        if parent_level > 32 {
            return Err(StarryError::StorageFull);
        }
        Ok(Self {
            id: NEXT_USER_NS_ID.fetch_add(1, Ordering::Relaxed),
            lineage: UserNamespaceLineage {
                parent: Some(parent),
                level: parent_level + 1,
                owner_uid: creator_euid,
            },
            is_root: false,
            uid_mapped: false,
            gid_mapped: false,
        })
    }

    pub(crate) fn lineage(&self) -> UserNamespaceLineage {
        self.lineage.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn child_creation_enforces_linux_parent_depth_limit() {
        let mut parent = Arc::new(RawSpinLock::new(UserNamespace::new_root()));
        for _ in 0..32 {
            parent = Arc::new(RawSpinLock::new(
                UserNamespace::new_child(parent, 41).unwrap(),
            ));
        }

        let child = Arc::new(RawSpinLock::new(
            UserNamespace::new_child(parent.clone(), 99).unwrap(),
        ));
        let child_id = child.lock_irqsave().id;
        let error = UserNamespace::new_child(child.clone(), 100)
            .err()
            .expect("a level-33 parent must reject another child");
        assert_eq!(error.linux_errno(), syscalls::Errno::ENOSPC);

        let state = child.lock_irqsave();
        let lineage = state.lineage();
        assert_eq!(lineage.level, 33);
        assert_eq!(lineage.owner_uid, 99);
        assert!(Arc::ptr_eq(lineage.parent.as_ref().unwrap(), &parent));
        assert_eq!(state.id, child_id);
        assert!(!state.is_root && !state.uid_mapped && !state.gid_mapped);
    }
}
