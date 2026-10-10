use alloc::sync::Arc;

use ax_cgroup::{CgroupNamespace, CgroupNode};

use crate::sync::RawSpinLock;

/// The initial cgroup namespace rooted at the global cgroup hierarchy.
pub static ROOT_CGROUP_NS: ax_lazyinit::LazyLock<Arc<RawSpinLock<CgroupNamespace>>> =
    ax_lazyinit::LazyLock::new(|| {
        Arc::new(RawSpinLock::new(CgroupNamespace::new(ax_cgroup::root())))
    });

/// Create a new cgroup namespace rooted at the supplied membership.
pub fn new_cgroup_namespace(root: Arc<CgroupNode>) -> Arc<RawSpinLock<CgroupNamespace>> {
    Arc::new(RawSpinLock::new(CgroupNamespace::new(root)))
}
