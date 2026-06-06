use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use ax_kspin::SpinNoIrq;

/// The initial root user namespace, shared by all processes until
/// they call `unshare(CLONE_NEWUSER)` or `clone(CLONE_NEWUSER)`.
pub static ROOT_USER_NS: spin::LazyLock<Arc<SpinNoIrq<UserNamespace>>> =
    spin::LazyLock::new(|| Arc::new(SpinNoIrq::new(UserNamespace::new_root())));

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
    /// Effective UID of the namespace creator (0 for root namespace).
    pub owner_uid: u32,
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
            owner_uid: 0,
            is_root: true,
            uid_mapped: true,
            gid_mapped: true,
        }
    }

    pub fn clone_ns(&self) -> Self {
        Self {
            id: NEXT_USER_NS_ID.fetch_add(1, Ordering::Relaxed),
            owner_uid: self.owner_uid,
            is_root: false,
            uid_mapped: false,
            gid_mapped: false,
        }
    }
}
