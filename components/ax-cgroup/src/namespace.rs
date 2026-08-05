use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{CgroupNode, CgroupPin};

static NEXT_NAMESPACE_ID: AtomicU64 = AtomicU64::new(1);

/// A cgroup hierarchy view rooted at a stable cgroup node.
pub struct CgroupNamespace {
    id: u64,
    root: CgroupPin,
}

impl CgroupNamespace {
    /// Create a namespace rooted at the caller's current membership.
    pub fn new(root: Arc<CgroupNode>) -> Self {
        Self {
            id: NEXT_NAMESPACE_ID.fetch_add(1, Ordering::Relaxed),
            root: root.pin(),
        }
    }

    /// Return the namespace inode identity.
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Clone the stable hierarchy root pinned by this namespace.
    pub fn root(&self) -> Arc<CgroupNode> {
        self.root.node()
    }

    /// Create an independent ownership pin for a new cgroup2 mount.
    pub fn pin_root(&self) -> CgroupPin {
        self.root.node().pin()
    }
}
