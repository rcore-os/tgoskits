use alloc::sync::Arc;

use ax_cgroup::{CgroupNamespace, CgroupNode};

use crate::sync::IrqMutex;

/// The initial cgroup namespace rooted at the global cgroup hierarchy.
pub static ROOT_CGROUP_NS: ax_lazyinit::LazyLock<Arc<IrqMutex<CgroupNamespace>>> =
    ax_lazyinit::LazyLock::new(|| Arc::new(IrqMutex::new(CgroupNamespace::new(ax_cgroup::root()))));

/// Create a new cgroup namespace rooted at the supplied membership.
pub fn new_cgroup_namespace(root: Arc<CgroupNode>) -> Arc<IrqMutex<CgroupNamespace>> {
    Arc::new(IrqMutex::new(CgroupNamespace::new(root)))
}
