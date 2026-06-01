use alloc::sync::Arc;
use core::ffi::c_char;

use ax_kspin::SpinNoIrq;

/// The initial root mount namespace, shared by all processes until
/// they call `unshare(CLONE_NEWNS)` or `clone(CLONE_NEWNS)`.
pub static ROOT_MNT_NS: spin::LazyLock<Arc<SpinNoIrq<MntNamespace>>> =
    spin::LazyLock::new(|| Arc::new(SpinNoIrq::new(MntNamespace::new_root())));

const fn pad_mnt_root(root: &str) -> [c_char; 256] {
    let mut data: [c_char; 256] = [0; 256];
    unsafe {
        core::ptr::copy_nonoverlapping(root.as_ptr().cast(), data.as_mut_ptr(), root.len());
    }
    data
}

/// Per-process mount namespace.
///
/// Isolates the set of filesystem mount points seen by a process.
/// In the root namespace `root` starts as `"/"`.
pub struct MntNamespace {
    pub root: [c_char; 256],
}

impl MntNamespace {
    pub fn new_root() -> Self {
        Self {
            root: pad_mnt_root("/"),
        }
    }

    pub fn clone_ns(&self) -> Self {
        Self { root: self.root }
    }
}
