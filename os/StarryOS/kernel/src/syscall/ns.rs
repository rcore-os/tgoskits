use ax_task::current;

use crate::task::AsThread;

/// Returns `true` when the calling task belongs to the root network
/// namespace (ns_id == 0).  Used by the network stack to decide whether
/// a real network device (eth0) is visible — processes in non-root
/// network namespaces only see the loopback interface.
pub fn in_root_net_ns() -> bool {
    let curr = current();
    let nsproxy = curr.as_thread().proc_data.nsproxy.lock();
    nsproxy.net_ns.lock().ns_id == 0
}
