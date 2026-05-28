use alloc::sync::Arc;
use core::sync::atomic::AtomicU64;

use ax_kspin::SpinNoIrq;

static NEXT_NET_NS_ID: AtomicU64 = AtomicU64::new(0);

/// The initial root network namespace, shared by all processes until
/// they call `unshare(CLONE_NEWNET)` or `clone(CLONE_NEWNET)`.
pub static ROOT_NET_NS: spin::LazyLock<Arc<SpinNoIrq<NetNamespace>>> =
    spin::LazyLock::new(|| Arc::new(SpinNoIrq::new(NetNamespace::new_root())));

/// Per-process network namespace.
///
/// Isolates network interfaces, routing tables, firewall rules, and
/// sockets so that processes in different network namespaces see
/// independent network stacks.
pub struct NetNamespace {
    pub ns_id: u64,
}

impl NetNamespace {
    pub fn new_root() -> Self {
        Self {
            ns_id: NEXT_NET_NS_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
        }
    }

    pub fn clone_ns(&self) -> Self {
        Self {
            ns_id: NEXT_NET_NS_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
        }
    }
}
