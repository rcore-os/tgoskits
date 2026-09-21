use alloc::sync::Arc;
use core::sync::atomic::AtomicU64;

use crate::sync::IrqMutex;

// Namespace ids double as the st_ino of /proc/<pid>/ns/ipc NsFds, and inode 0
// is never a valid identity on Linux, so the counter starts at 1 like the
// other namespace id counters.
static NEXT_IPC_NS_ID: AtomicU64 = AtomicU64::new(1);

/// The initial root IPC namespace, shared by all processes until
/// they call `unshare(CLONE_NEWIPC)` or `clone(CLONE_NEWIPC)`.
pub static ROOT_IPC_NS: ax_lazyinit::LazyLock<Arc<IrqMutex<IpcNamespace>>> =
    ax_lazyinit::LazyLock::new(|| Arc::new(IrqMutex::new(IpcNamespace::new_root())));

/// Per-process IPC namespace.
///
/// Isolates System V IPC objects (shared memory, semaphores, message
/// queues) and POSIX message queues so that processes in different
/// IPC namespaces cannot access each other's IPC resources.
pub struct IpcNamespace {
    pub ns_id: u64,
}

impl IpcNamespace {
    pub fn new_root() -> Self {
        Self {
            ns_id: NEXT_IPC_NS_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
        }
    }

    pub fn clone_ns(&self) -> Self {
        Self {
            ns_id: NEXT_IPC_NS_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
        }
    }
}
