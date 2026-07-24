use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use ax_kspin::SpinNoIrq;

/// The initial root cgroup namespace, shared by all processes until
/// they call `unshare(CLONE_NEWCGROUP)` or `clone(CLONE_NEWCGROUP)`.
pub static ROOT_CGROUP_NS: spin::LazyLock<Arc<SpinNoIrq<CgroupNamespace>>> =
    spin::LazyLock::new(|| Arc::new(SpinNoIrq::new(CgroupNamespace::new_root())));

static NEXT_CGROUP_NS_ID: AtomicU64 = AtomicU64::new(1);

/// Per-process cgroup namespace.
///
/// Cgroup namespaces virtualize the view of the cgroup hierarchy:
/// processes in a new cgroup namespace see their current cgroup as the
/// root of the hierarchy in `/proc/<pid>/cgroup`.  This type carries
/// only the namespace identity; the cgroup hierarchy itself is managed
/// elsewhere.  The ID is exposed via `/proc/<pid>/ns/cgroup` as
/// `cgroup:[<id>]`.
pub struct CgroupNamespace {
    id: u64,
}

impl CgroupNamespace {
    pub fn new_root() -> Self {
        Self {
            id: NEXT_CGROUP_NS_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub fn clone_ns(&self) -> Self {
        Self {
            id: NEXT_CGROUP_NS_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}
