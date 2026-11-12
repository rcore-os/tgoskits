use alloc::sync::Arc;

use ax_cgroup::{CgroupNamespace, CgroupNode};

use crate::sync::RawSpinLock;

/// The initial cgroup namespace rooted at the global cgroup hierarchy.
pub static ROOT_CGROUP_NS: ax_lazyinit::LazyLock<Arc<RawSpinLock<CgroupNamespace>>> =
    ax_lazyinit::LazyLock::new(|| {
        // The global hierarchy root is permanently owned and cannot be removed.
        let namespace = CgroupNamespace::new(ax_cgroup::root())
            .expect("the permanent cgroup root must remain live");
        Arc::new(RawSpinLock::new(namespace))
    });

/// Create a new cgroup namespace rooted at the supplied membership.
pub fn new_cgroup_namespace(
    root: Arc<CgroupNode>,
) -> crate::StarryResult<Arc<RawSpinLock<CgroupNamespace>>> {
    let namespace = CgroupNamespace::new(root)?;
    Ok(Arc::new(RawSpinLock::new(namespace)))
}
