use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{StarryError, StarryResult, sync::RawSpinLock};

/// The initial root user namespace, shared by all processes until
/// they call `unshare(CLONE_NEWUSER)` or `clone(CLONE_NEWUSER)`.
pub static ROOT_USER_NS: ax_lazyinit::LazyLock<Arc<RawSpinLock<UserNamespace>>> =
    ax_lazyinit::LazyLock::new(|| Arc::new(RawSpinLock::new(UserNamespace::new_root())));

static NEXT_USER_NS_ID: AtomicU64 = AtomicU64::new(1);
/// Highest parent level allowed by Linux's level-33 child limit.
const MAX_PARENT_LEVEL: u32 = 32;

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
    /// Immutable parent namespace. The root namespace has no parent.
    parent: Option<Arc<RawSpinLock<UserNamespace>>>,
    /// Depth in the user-namespace tree; the root namespace is level zero.
    level: u32,
    /// Effective UID of the namespace creator (0 for root namespace).
    owner_uid: u32,
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

impl UserNamespace {
    pub fn new_root() -> Self {
        Self {
            id: NEXT_USER_NS_ID.fetch_add(1, Ordering::Relaxed),
            parent: None,
            level: 0,
            owner_uid: 0,
            is_root: true,
            uid_mapped: true,
            gid_mapped: true,
        }
    }

    pub(crate) fn new_child(
        parent: Arc<RawSpinLock<UserNamespace>>,
        owner_uid: u32,
    ) -> StarryResult<Self> {
        let parent_level = parent.lock_irqsave().level;
        if parent_level > MAX_PARENT_LEVEL {
            return Err(StarryError::StorageFull);
        }

        Ok(Self {
            id: NEXT_USER_NS_ID.fetch_add(1, Ordering::Relaxed),
            parent: Some(parent),
            level: parent_level + 1,
            owner_uid,
            is_root: false,
            uid_mapped: false,
            gid_mapped: false,
        })
    }

    /// Return a strong snapshot of the parent namespace.
    pub(crate) fn parent(&self) -> Option<Arc<RawSpinLock<UserNamespace>>> {
        self.parent.clone()
    }

    /// Return this namespace's immutable tree depth.
    pub(crate) const fn level(&self) -> u32 {
        self.level
    }

    /// Return the creator's effective UID captured at construction.
    pub(crate) const fn owner_uid(&self) -> u32 {
        self.owner_uid
    }
}
