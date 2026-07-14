use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use ax_kspin::SpinNoIrq;

/// The initial root mount namespace, shared by all processes until
/// they call `unshare(CLONE_NEWNS)` or `clone(CLONE_NEWNS)`.
pub static ROOT_MNT_NS: spin::LazyLock<Arc<SpinNoIrq<MntNamespace>>> =
    spin::LazyLock::new(|| Arc::new(SpinNoIrq::new(MntNamespace::new_root())));

static MNT_NS_ID: AtomicU64 = AtomicU64::new(1);

/// Per-process mount namespace.
///
/// This is the namespace identity visible through `NsProxy`. The live mount
/// topology is held by the task-local `ax_fs::FsContext`, and syscall paths
/// update both objects together when entering a new mount namespace.
pub struct MntNamespace {
    id: u64,
}

impl MntNamespace {
    pub fn new_root() -> Self {
        Self {
            id: MNT_NS_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub fn clone_ns(&self) -> Self {
        Self {
            id: MNT_NS_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}
