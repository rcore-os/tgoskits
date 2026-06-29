use alloc::sync::Arc;
use core::{
    ffi::c_char,
    sync::atomic::{AtomicU64, Ordering},
};

use ax_kspin::SpinNoIrq;

/// The initial root mount namespace, shared by all processes until
/// they call `unshare(CLONE_NEWNS)` or `clone(CLONE_NEWNS)`.
pub static ROOT_MNT_NS: spin::LazyLock<Arc<SpinNoIrq<MntNamespace>>> =
    spin::LazyLock::new(|| Arc::new(SpinNoIrq::new(MntNamespace::new_root())));

static NEXT_MNT_NS_ID: AtomicU64 = AtomicU64::new(2);

const fn pad_mnt_root(root: &str) -> [c_char; 256] {
    let mut data: [c_char; 256] = [0; 256];
    unsafe {
        core::ptr::copy_nonoverlapping(root.as_ptr().cast(), data.as_mut_ptr(), root.len());
    }
    data
}

/// Mount namespace identity metadata.
///
/// The namespace-local mount tree is owned by `ax-fs-ng`'s `FsContext`;
/// this object supplies the stable identity shared by nsproxy and namespace
/// file descriptors.
pub struct MntNamespace {
    pub ns_id: u64,
    pub root: [c_char; 256],
}

impl MntNamespace {
    pub fn new_root() -> Self {
        Self {
            ns_id: 1,
            root: pad_mnt_root("/"),
        }
    }

    pub fn clone_ns(&self) -> Self {
        Self {
            ns_id: NEXT_MNT_NS_ID.fetch_add(1, Ordering::Relaxed),
            root: self.root,
        }
    }
}
