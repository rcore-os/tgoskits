use alloc::sync::Arc;

use ax_kspin::SpinNoIrq;

/// The initial root user namespace, shared by all processes until
/// they call `unshare(CLONE_NEWUSER)` or `clone(CLONE_NEWUSER)`.
pub static ROOT_USER_NS: spin::LazyLock<Arc<SpinNoIrq<UserNamespace>>> =
    spin::LazyLock::new(|| Arc::new(SpinNoIrq::new(UserNamespace::new_root())));

/// Per-process user namespace.
///
/// Isolates UID/GID mappings so that a process may have uid 0 inside
/// its namespace while mapping to an unprivileged UID in the parent.
/// `owner_uid` is the effective UID of the process that created the
/// namespace (0 for the root namespace).
///
/// When `is_root` is false (child namespace without configured mappings),
/// all UIDs/GIDs are reported as `nobody` (65534), matching Linux default
/// behaviour for unconfigured user namespaces.
pub struct UserNamespace {
    /// Effective UID of the namespace creator (0 for root namespace).
    pub owner_uid: u32,
    /// Whether this is the initial root user namespace.  false for
    /// namespaces created via unshare / clone(CLONE_NEWUSER) that
    /// have not yet had uid_map / gid_map configured.
    pub is_root: bool,
}

impl UserNamespace {
    pub fn new_root() -> Self {
        Self {
            owner_uid: 0,
            is_root: true,
        }
    }

    pub fn clone_ns(&self) -> Self {
        Self {
            owner_uid: self.owner_uid,
            is_root: false,
        }
    }
}
