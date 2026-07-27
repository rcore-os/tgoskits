use alloc::{collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicU64, Ordering};

use ax_kspin::SpinNoIrq;

/// The initial root PID namespace, shared by all processes until
/// they call `unshare(CLONE_NEWPID)` or `clone(CLONE_NEWPID)`.
pub static ROOT_PID_NS: spin::LazyLock<Arc<SpinNoIrq<PidNamespace>>> =
    spin::LazyLock::new(|| Arc::new(SpinNoIrq::new(PidNamespace::new_root())));

static NEXT_PID_NS_ID: AtomicU64 = AtomicU64::new(1);

/// Per-process PID namespace.
///
/// Each PID namespace has a nesting `level` (0 for the root namespace,
/// incremented for each nested PID namespace) and isolates PID numbering
/// so that processes in different PID namespaces may have the same PID
/// value as seen from within their respective namespace.
pub struct PidNamespace {
    /// Globally unique namespace identifier (exposed via /proc/PID/ns/pid).
    pub id: u64,
    /// PID namespace nesting level.  Root is 0, first child is 1, etc.
    pub level: u32,
    /// Next local PID to allocate in this namespace (starts at 1).
    next_pid: u32,
    /// Map from global TID to namespace-local PID.
    pid_map: BTreeMap<u64, u32>,
    /// Global TID of the PID namespace init process (PID 1 in this ns).
    init_global_tid: Option<u64>,
}

impl PidNamespace {
    pub fn new_root() -> Self {
        Self {
            id: NEXT_PID_NS_ID.fetch_add(1, Ordering::Relaxed),
            level: 0,
            next_pid: 1,
            pid_map: BTreeMap::new(),
            init_global_tid: None,
        }
    }

    /// Create a fresh child PID namespace (level + 1, empty pid map,
    /// next_pid starts at 1).
    pub fn clone_ns(&self) -> Self {
        Self {
            id: NEXT_PID_NS_ID.fetch_add(1, Ordering::Relaxed),
            level: self.level + 1,
            next_pid: 1,
            pid_map: BTreeMap::new(),
            init_global_tid: None,
        }
    }

    /// Allocate a namespace-local PID for the given global TID.
    pub fn alloc_local_pid(&mut self, global_tid: u64) -> u32 {
        let local = self.next_pid;
        self.next_pid += 1;
        self.pid_map.insert(global_tid, local);
        local
    }

    /// Releases a namespace PID reserved for a task that never became visible.
    ///
    /// The `(global_tid, local_pid)` pair prevents a stale rollback token from
    /// deleting a later mapping. The allocation cursor is restored only when
    /// no subsequent reservation has consumed it.
    #[must_use]
    pub fn release_unpublished_local_pid(&mut self, global_tid: u64, local_pid: u32) -> bool {
        if self.pid_map.get(&global_tid) != Some(&local_pid) {
            return false;
        }
        self.pid_map.remove(&global_tid);
        if self.init_global_tid == Some(global_tid) {
            self.init_global_tid = None;
        }
        if self.next_pid == local_pid.saturating_add(1) {
            self.next_pid = local_pid;
        }
        true
    }

    /// Resolve a global TID to its namespace-local PID.
    /// In the root namespace (level 0), global and local PIDs are 1:1.
    pub fn local_pid(&self, global_tid: u64) -> Option<u32> {
        if self.level == 0 {
            return Some(global_tid as u32);
        }
        self.pid_map.get(&global_tid).copied()
    }

    /// Record the global TID of this namespace's init process (PID 1).
    pub fn set_init_global_tid(&mut self, tid: u64) {
        self.init_global_tid = Some(tid);
    }

    /// Returns the global TID of this namespace's init process.
    pub fn init_global_tid(&self) -> Option<u64> {
        self.init_global_tid
    }
}

#[cfg(test)]
mod tests {
    use super::PidNamespace;

    #[test]
    fn unpublished_pid_reservation_restores_namespace_state() {
        let mut namespace = PidNamespace::new_root().clone_ns();
        let local_pid = namespace.alloc_local_pid(42);
        namespace.set_init_global_tid(42);

        assert_eq!(local_pid, 1);
        assert_eq!(namespace.local_pid(42), Some(1));
        assert_eq!(namespace.init_global_tid(), Some(42));

        assert!(namespace.release_unpublished_local_pid(42, local_pid));
        assert_eq!(namespace.local_pid(42), None);
        assert_eq!(namespace.init_global_tid(), None);
        assert_eq!(namespace.alloc_local_pid(43), 1);
    }

    #[test]
    fn stale_pid_reservation_cannot_remove_a_new_mapping() {
        let mut namespace = PidNamespace::new_root().clone_ns();
        let first = namespace.alloc_local_pid(42);
        let second = namespace.alloc_local_pid(43);

        assert!(!namespace.release_unpublished_local_pid(42, second));
        assert_eq!(namespace.local_pid(42), Some(first));
        assert_eq!(namespace.local_pid(43), Some(second));
    }
}
