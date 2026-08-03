use alloc::sync::Arc;

use ax_cgroup::{CgroupNamespace, CgroupNode};
use ax_kspin::SpinNoIrq;

/// The initial cgroup namespace rooted at the global cgroup hierarchy.
pub static ROOT_CGROUP_NS: spin::LazyLock<Arc<SpinNoIrq<CgroupNamespace>>> =
    spin::LazyLock::new(|| Arc::new(SpinNoIrq::new(CgroupNamespace::new(ax_cgroup::root()))));

/// Create a new cgroup namespace rooted at the supplied membership.
pub fn new_cgroup_namespace(root: Arc<CgroupNode>) -> Arc<SpinNoIrq<CgroupNamespace>> {
    Arc::new(SpinNoIrq::new(CgroupNamespace::new(root)))
}
