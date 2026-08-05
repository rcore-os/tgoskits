use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
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
    /// Parent namespace retained while this descendant namespace is live.
    parent: Option<Arc<SpinNoIrq<PidNamespace>>>,
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
            parent: None,
            next_pid: 1,
            pid_map: BTreeMap::new(),
            init_global_tid: None,
        }
    }

    /// Create a fresh child PID namespace (level + 1, empty pid map,
    /// next_pid starts at 1).
    pub fn new_child(parent: Arc<SpinNoIrq<Self>>) -> Self {
        let level = parent.lock().level + 1;
        Self {
            id: NEXT_PID_NS_ID.fetch_add(1, Ordering::Relaxed),
            level,
            parent: Some(parent),
            next_pid: 1,
            pid_map: BTreeMap::new(),
            init_global_tid: None,
        }
    }

    /// Allocate a namespace-local PID for the given global TID.
    pub fn alloc_local_pid(&mut self, global_tid: u64) -> u32 {
        if let Some(pid) = self.pid_map.get(&global_tid) {
            return *pid;
        }
        let local = self.next_pid;
        self.next_pid += 1;
        self.pid_map.insert(global_tid, local);
        local
    }

    /// Allocate a task ID in this namespace and every non-root ancestor.
    ///
    /// PID namespaces assign local IDs to both thread-group leaders and
    /// non-leader threads.
    pub fn alloc_pid_chain(namespace: &Arc<SpinNoIrq<Self>>, global_tid: u64) {
        let mut current = namespace.clone();
        loop {
            let parent = {
                let mut current_lock = current.lock();
                if current_lock.level == 0 {
                    return;
                }
                current_lock.alloc_local_pid(global_tid);
                current_lock.parent.clone()
            };
            let Some(parent) = parent else {
                return;
            };
            current = parent;
        }
    }

    /// Resolve a global TID to its namespace-local PID.
    /// In the root namespace (level 0), global and local PIDs are 1:1.
    pub fn local_pid(&self, global_tid: u64) -> Option<u32> {
        if self.level == 0 {
            return Some(global_tid as u32);
        }
        self.pid_map.get(&global_tid).copied()
    }

    /// Return the target PID as seen from `observer` and every descendant
    /// namespace through `target`.
    ///
    /// Returns [`None`] when `target` is not a descendant of `observer`.
    pub fn visible_pid_chain(
        observer: &Arc<SpinNoIrq<Self>>,
        target: &Arc<SpinNoIrq<Self>>,
        global_tid: u64,
    ) -> Option<Vec<u32>> {
        let mut current = target.clone();
        let mut chain = Vec::new();
        loop {
            let (pid, parent) = {
                let current_lock = current.lock();
                (
                    current_lock.local_pid(global_tid)?,
                    current_lock.parent.clone(),
                )
            };
            chain.push(pid);
            if Arc::ptr_eq(&current, observer) {
                chain.reverse();
                return Some(chain);
            }
            current = parent?;
        }
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
