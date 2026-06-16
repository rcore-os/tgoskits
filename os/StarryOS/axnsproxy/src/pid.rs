use alloc::{collections::BTreeMap, sync::Arc};

use ax_kspin::SpinNoIrq;

/// The initial root PID namespace, shared by all processes until
/// they call `unshare(CLONE_NEWPID)` or `clone(CLONE_NEWPID)`.
pub static ROOT_PID_NS: spin::LazyLock<Arc<SpinNoIrq<PidNamespace>>> =
    spin::LazyLock::new(|| Arc::new(SpinNoIrq::new(PidNamespace::new_root())));

/// Per-process PID namespace.
///
/// Each PID namespace has a nesting `level` (0 for the root namespace,
/// incremented for each nested PID namespace) and isolates PID numbering
/// so that processes in different PID namespaces may have the same PID
/// value as seen from within their respective namespace.
pub struct PidNamespace {
    /// PID namespace nesting level.  Root is 0, first child is 1, etc.
    pub level: u32,
    /// Next local PID to allocate in this namespace (starts at 1).
    next_pid: u32,
    /// Map from global TID to namespace-local PID.
    pid_map: BTreeMap<u64, u32>,
}

impl PidNamespace {
    pub fn new_root() -> Self {
        Self {
            level: 0,
            next_pid: 1,
            pid_map: BTreeMap::new(),
        }
    }

    /// Create a fresh child PID namespace (level + 1, empty pid map,
    /// next_pid starts at 1).
    pub fn clone_ns(&self) -> Self {
        Self {
            level: self.level + 1,
            next_pid: 1,
            pid_map: BTreeMap::new(),
        }
    }

    /// Allocate a namespace-local PID for the given global TID.
    pub fn alloc_local_pid(&mut self, global_tid: u64) -> u32 {
        let local = self.next_pid;
        self.next_pid += 1;
        self.pid_map.insert(global_tid, local);
        local
    }

    /// Resolve a global TID to its namespace-local PID.
    /// In the root namespace (level 0), global and local PIDs are 1:1.
    pub fn local_pid(&self, global_tid: u64) -> Option<u32> {
        if self.level == 0 {
            return Some(global_tid as u32);
        }
        self.pid_map.get(&global_tid).copied()
    }
}
