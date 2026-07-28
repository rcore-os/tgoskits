use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;

/// Shared ownership handle for one PID namespace generation.
pub type PidNamespaceRef = Arc<PidNamespace>;

/// The initial root PID namespace, shared by all processes until
/// they call `unshare(CLONE_NEWPID)` or `clone(CLONE_NEWPID)`.
pub static ROOT_PID_NS: spin::LazyLock<PidNamespaceRef> =
    spin::LazyLock::new(|| Arc::new(PidNamespace::new_root()));

static NEXT_PID_NS_ID: AtomicU64 = AtomicU64::new(1);

/// Immutable identity and synchronized allocation state for one PID namespace.
///
/// Each PID namespace has a nesting `level` (0 for the root namespace,
/// incremented for each nested PID namespace) and isolates PID numbering
/// so that processes in different PID namespaces may have the same PID
/// value as seen from within their respective namespace.
pub struct PidNamespace {
    /// Globally unique namespace identifier (exposed via /proc/PID/ns/pid).
    id: u64,
    /// PID namespace nesting level.  Root is 0, first child is 1, etc.
    level: u32,
    /// Immediate enclosing namespace. The hierarchy only owns upward, so
    /// namespace handles cannot form a reference cycle.
    parent: Option<PidNamespaceRef>,
    state: SpinNoIrq<PidNamespaceState>,
}

struct PidNamespaceState {
    /// Next local PID to allocate in this namespace (starts at 1).
    next_pid: u32,
    /// Generation-specific PID entries, including unpublished reservations.
    pid_map: BTreeMap<u64, PidEntry>,
    lifecycle: PidNamespaceLifecycle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PidNamespaceLifecycle {
    Root,
    AwaitingInit,
    Active { init_global_tid: u64 },
    ShuttingDown { init_global_tid: u64 },
    Dead { init_global_tid: u64 },
}

/// Linux identity represented by one PID namespace reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PidReservationKind {
    Process,
    Thread,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PidPublication {
    Reserved,
    Published,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PidEntry {
    local_pid: u32,
    kind: PidReservationKind,
    publication: PidPublication,
}

impl PidNamespace {
    pub fn new_root() -> Self {
        Self {
            id: NEXT_PID_NS_ID.fetch_add(1, Ordering::Relaxed),
            level: 0,
            parent: None,
            state: SpinNoIrq::new(PidNamespaceState::new(PidNamespaceLifecycle::Root)),
        }
    }

    /// Creates a fresh child PID namespace with explicit ancestor ownership.
    pub fn new_child(parent: PidNamespaceRef) -> Self {
        let level = parent
            .level
            .checked_add(1)
            .expect("PID namespace nesting level overflow");
        Self {
            id: NEXT_PID_NS_ID.fetch_add(1, Ordering::Relaxed),
            level,
            parent: Some(parent),
            state: SpinNoIrq::new(PidNamespaceState::new(PidNamespaceLifecycle::AwaitingInit)),
        }
    }

    /// Returns the stable identifier exposed by `/proc/PID/ns/pid`.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Returns this namespace's immutable nesting level.
    pub fn level(&self) -> u32 {
        self.level
    }

    /// Reserves a namespace-local PID while a task remains unpublished.
    pub fn reserve_local_pid(
        &self,
        global_tid: u64,
        kind: PidReservationKind,
        namespace_init: bool,
    ) -> AxResult<u32> {
        self.state
            .lock()
            .reserve_local_pid(global_tid, kind, namespace_init)
    }

    /// Commits a reservation to Linux-visible task publication.
    pub fn publish_reserved_pid(&self, global_tid: u64, local_pid: u32) -> AxResult<()> {
        self.state
            .lock()
            .publish_reserved_pid(global_tid, local_pid)
    }

    /// Rolls back an exact clone reservation before publication commits.
    #[must_use]
    pub fn rollback_pid_reservation(&self, global_tid: u64, local_pid: u32) -> bool {
        self.state
            .lock()
            .rollback_pid_reservation(global_tid, local_pid)
    }

    /// Resolves a global TID to its namespace-local PID.
    pub fn local_pid(&self, global_tid: u64) -> Option<u32> {
        if self.level == 0 {
            return Some(global_tid as u32);
        }
        self.state.lock().local_pid(global_tid)
    }

    /// Returns the global TID of this namespace's immutable init process.
    pub fn init_global_tid(&self) -> Option<u64> {
        self.state.lock().init_global_tid()
    }

    /// Atomically disables new PID publication for the namespace init.
    #[must_use]
    pub fn begin_shutdown(&self, init_global_tid: u64) -> bool {
        self.state.lock().begin_shutdown(init_global_tid)
    }

    /// Returns whether the namespace init has disabled task publication.
    pub fn is_shutting_down(&self) -> bool {
        self.state.lock().is_shutting_down()
    }

    /// Returns published task IDs other than the namespace init thread.
    pub fn published_members_excluding(&self, init_global_tid: u64) -> Vec<u64> {
        self.state
            .lock()
            .published_members_excluding(init_global_tid)
    }

    /// Returns whether any reservation outside the namespace init remains.
    pub fn has_members_excluding(&self, init_global_tid: u64) -> bool {
        self.state.lock().has_members_excluding(init_global_tid)
    }

    /// Releases one namespace-local thread ID after scheduler-visible exit.
    #[must_use]
    pub fn release_thread_pid(&self, global_tid: u64) -> bool {
        self.state.lock().release_thread_pid(global_tid)
    }

    /// Releases one namespace-local process ID after its zombie is reaped.
    #[must_use]
    pub fn release_process_pid(&self, global_pid: u64) -> bool {
        self.state.lock().release_process_pid(global_pid)
    }
}

impl PidNamespaceState {
    fn new(lifecycle: PidNamespaceLifecycle) -> Self {
        Self {
            next_pid: 1,
            pid_map: BTreeMap::new(),
            lifecycle,
        }
    }

    /// Reserves a namespace-local PID while a task remains unpublished.
    ///
    /// A namespace accepts ordinary reservations only after its immutable init
    /// identity has been installed and before shutdown begins.
    fn reserve_local_pid(
        &mut self,
        global_tid: u64,
        kind: PidReservationKind,
        namespace_init: bool,
    ) -> AxResult<u32> {
        if self.pid_map.contains_key(&global_tid) {
            return Err(AxError::AlreadyExists);
        }
        match (self.lifecycle, namespace_init) {
            (PidNamespaceLifecycle::AwaitingInit, true) => {
                if kind != PidReservationKind::Process {
                    return Err(AxError::InvalidInput);
                }
            }
            (PidNamespaceLifecycle::Active { .. }, false) => {}
            (PidNamespaceLifecycle::Root, _) => return Err(AxError::InvalidInput),
            (
                PidNamespaceLifecycle::AwaitingInit
                | PidNamespaceLifecycle::ShuttingDown { .. }
                | PidNamespaceLifecycle::Dead { .. },
                _,
            )
            | (PidNamespaceLifecycle::Active { .. }, true) => return Err(AxError::NoMemory),
        }

        let local = self.next_pid;
        self.next_pid = self.next_pid.checked_add(1).ok_or(AxError::NoMemory)?;
        self.pid_map.insert(
            global_tid,
            PidEntry {
                local_pid: local,
                kind,
                publication: PidPublication::Reserved,
            },
        );
        if namespace_init {
            debug_assert_eq!(local, 1);
            self.lifecycle = PidNamespaceLifecycle::Active {
                init_global_tid: global_tid,
            };
        }
        Ok(local)
    }

    /// Commits a reservation to Linux-visible task publication.
    ///
    /// This is the post-registry shutdown check corresponding to Linux's
    /// `copy_process()` check of `PIDNS_ADDING`.
    fn publish_reserved_pid(&mut self, global_tid: u64, local_pid: u32) -> AxResult<()> {
        if !matches!(self.lifecycle, PidNamespaceLifecycle::Active { .. }) {
            return Err(AxError::NoMemory);
        }
        let entry = self
            .pid_map
            .get_mut(&global_tid)
            .filter(|entry| entry.local_pid == local_pid)
            .ok_or(AxError::BadState)?;
        if entry.publication != PidPublication::Reserved {
            return Err(AxError::BadState);
        }
        entry.publication = PidPublication::Published;
        Ok(())
    }

    /// Releases a namespace PID reserved for a task that never became visible.
    ///
    /// The `(global_tid, local_pid)` pair prevents a stale rollback token from
    /// deleting a later mapping. The allocation cursor is restored only when
    /// no subsequent reservation has consumed it.
    #[must_use]
    fn rollback_pid_reservation(&mut self, global_tid: u64, local_pid: u32) -> bool {
        let Some(entry) = self
            .pid_map
            .get(&global_tid)
            .filter(|entry| entry.local_pid == local_pid)
            .copied()
        else {
            return false;
        };
        self.pid_map.remove(&global_tid);
        if matches!(
            self.lifecycle,
            PidNamespaceLifecycle::Active { init_global_tid }
                if init_global_tid == global_tid
        ) && entry.kind == PidReservationKind::Process
        {
            self.lifecycle = PidNamespaceLifecycle::AwaitingInit;
        }
        if self.next_pid == local_pid.saturating_add(1) {
            self.next_pid = local_pid;
        }
        true
    }

    /// Resolve a global TID to its namespace-local PID.
    /// In the root namespace (level 0), global and local PIDs are 1:1.
    fn local_pid(&self, global_tid: u64) -> Option<u32> {
        self.pid_map.get(&global_tid).map(|entry| entry.local_pid)
    }

    /// Returns the global TID of this namespace's init process.
    fn init_global_tid(&self) -> Option<u64> {
        match self.lifecycle {
            PidNamespaceLifecycle::Root | PidNamespaceLifecycle::AwaitingInit => None,
            PidNamespaceLifecycle::Active { init_global_tid }
            | PidNamespaceLifecycle::ShuttingDown { init_global_tid }
            | PidNamespaceLifecycle::Dead { init_global_tid } => Some(init_global_tid),
        }
    }

    /// Atomically disables new PID publication for the namespace init.
    #[must_use]
    fn begin_shutdown(&mut self, init_global_tid: u64) -> bool {
        let PidNamespaceLifecycle::Active {
            init_global_tid: registered,
        } = self.lifecycle
        else {
            return false;
        };
        if registered != init_global_tid {
            return false;
        }
        self.lifecycle = PidNamespaceLifecycle::ShuttingDown { init_global_tid };
        true
    }

    /// Returns whether the namespace init has disabled task publication.
    fn is_shutting_down(&self) -> bool {
        matches!(self.lifecycle, PidNamespaceLifecycle::ShuttingDown { .. })
    }

    /// Returns published task IDs other than the namespace init thread.
    fn published_members_excluding(&self, init_global_tid: u64) -> Vec<u64> {
        self.pid_map
            .iter()
            .filter_map(|(global_tid, entry)| {
                (*global_tid != init_global_tid && entry.publication == PidPublication::Published)
                    .then_some(*global_tid)
            })
            .collect()
    }

    /// Returns whether any reservation outside the namespace init remains.
    fn has_members_excluding(&self, init_global_tid: u64) -> bool {
        self.pid_map
            .keys()
            .any(|global_tid| *global_tid != init_global_tid)
    }

    /// Releases one namespace-local thread ID after scheduler-visible exit.
    #[must_use]
    fn release_thread_pid(&mut self, global_tid: u64) -> bool {
        self.release_published_pid(global_tid, PidReservationKind::Thread)
    }

    /// Releases one namespace-local process ID after its zombie is reaped.
    #[must_use]
    fn release_process_pid(&mut self, global_pid: u64) -> bool {
        let released = self.release_published_pid(global_pid, PidReservationKind::Process);
        if released
            && matches!(
                self.lifecycle,
                PidNamespaceLifecycle::ShuttingDown { init_global_tid }
                    if init_global_tid == global_pid
            )
        {
            debug_assert!(!self.has_members_excluding(global_pid));
            self.lifecycle = PidNamespaceLifecycle::Dead {
                init_global_tid: global_pid,
            };
        }
        released
    }

    fn release_published_pid(&mut self, global_tid: u64, kind: PidReservationKind) -> bool {
        let matches = self.pid_map.get(&global_tid).is_some_and(|entry| {
            entry.kind == kind && entry.publication == PidPublication::Published
        });
        if matches {
            self.pid_map.remove(&global_tid);
        }
        matches
    }
}

/// Returns namespace identities from the active namespace through the root.
///
/// Linux assigns one `upid` at every level. Keeping the same explicit
/// hierarchy lets clone reserve all affected namespaces and lets ancestor
/// shutdown include tasks running in nested descendants.
pub fn pid_namespace_lineage(innermost: &PidNamespaceRef) -> Vec<PidNamespaceRef> {
    let mut lineage = Vec::new();
    let mut current = Some(innermost.clone());
    while let Some(namespace) = current {
        current = namespace.parent.clone();
        lineage.push(namespace);
    }
    lineage
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use ax_errno::AxError;

    use super::{
        PidNamespace, PidNamespaceLifecycle, PidNamespaceRef, PidNamespaceState,
        PidReservationKind, pid_namespace_lineage,
    };

    fn new_namespace(parent: PidNamespaceRef) -> PidNamespaceRef {
        Arc::new(PidNamespace::new_child(parent))
    }

    fn new_child_state() -> PidNamespaceState {
        PidNamespaceState::new(PidNamespaceLifecycle::AwaitingInit)
    }

    fn reserve_process(
        namespace: &mut PidNamespaceState,
        global_pid: u64,
        namespace_init: bool,
    ) -> u32 {
        namespace
            .reserve_local_pid(global_pid, PidReservationKind::Process, namespace_init)
            .unwrap()
    }

    #[test]
    fn unpublished_pid_reservation_restores_namespace_state() {
        let mut namespace = new_child_state();
        let local_pid = reserve_process(&mut namespace, 42, true);

        assert_eq!(local_pid, 1);
        assert_eq!(namespace.local_pid(42), Some(1));
        assert_eq!(namespace.init_global_tid(), Some(42));

        assert!(namespace.rollback_pid_reservation(42, local_pid));
        assert_eq!(namespace.local_pid(42), None);
        assert_eq!(namespace.init_global_tid(), None);
        assert_eq!(reserve_process(&mut namespace, 43, true), 1);
    }

    #[test]
    fn stale_pid_reservation_cannot_remove_a_new_mapping() {
        let mut namespace = new_child_state();
        let first = reserve_process(&mut namespace, 42, true);
        namespace.publish_reserved_pid(42, first).unwrap();
        let second = reserve_process(&mut namespace, 43, false);

        assert!(!namespace.rollback_pid_reservation(42, second));
        assert_eq!(namespace.local_pid(42), Some(first));
        assert_eq!(namespace.local_pid(43), Some(second));
    }

    #[test]
    fn pid_namespace_init_identity_cannot_be_replaced() {
        let mut namespace = new_child_state();
        let init_pid = reserve_process(&mut namespace, 42, true);

        assert_eq!(init_pid, 1);
        assert_eq!(
            namespace.reserve_local_pid(43, PidReservationKind::Process, true),
            Err(AxError::NoMemory)
        );
        assert_eq!(namespace.init_global_tid(), Some(42));
    }

    #[test]
    fn shutdown_rejects_publication_until_existing_members_retire() {
        let mut namespace = new_child_state();
        let init = reserve_process(&mut namespace, 42, true);
        namespace.publish_reserved_pid(42, init).unwrap();
        let child = reserve_process(&mut namespace, 43, false);
        namespace.publish_reserved_pid(43, child).unwrap();

        assert!(namespace.begin_shutdown(42));
        assert_eq!(
            namespace.reserve_local_pid(44, PidReservationKind::Process, false),
            Err(AxError::NoMemory)
        );
        assert_eq!(namespace.published_members_excluding(42), [43]);
        assert!(namespace.has_members_excluding(42));

        assert!(namespace.release_process_pid(43));
        assert!(!namespace.has_members_excluding(42));
        assert!(namespace.release_process_pid(42));
        assert!(namespace.is_shutting_down() == false);
        assert_eq!(namespace.init_global_tid(), Some(42));
    }

    #[test]
    fn shutdown_revokes_a_reserved_but_unpublished_clone() {
        let mut namespace = new_child_state();
        let init = reserve_process(&mut namespace, 42, true);
        namespace.publish_reserved_pid(42, init).unwrap();
        let pending = reserve_process(&mut namespace, 43, false);

        assert!(namespace.begin_shutdown(42));
        assert_eq!(
            namespace.publish_reserved_pid(43, pending),
            Err(AxError::NoMemory)
        );
        assert!(namespace.has_members_excluding(42));
        assert!(namespace.rollback_pid_reservation(43, pending));
        assert!(!namespace.has_members_excluding(42));
    }

    #[test]
    fn ancestor_state_tracks_a_nested_namespace_process() {
        let mut parent = new_child_state();
        let parent_init = reserve_process(&mut parent, 40, true);
        parent.publish_reserved_pid(40, parent_init).unwrap();
        let mut child = new_child_state();

        let child_pid = reserve_process(&mut child, 42, true);
        let parent_pid = reserve_process(&mut parent, 42, false);
        child.publish_reserved_pid(42, child_pid).unwrap();
        parent.publish_reserved_pid(42, parent_pid).unwrap();

        assert!(parent.begin_shutdown(40));
        assert_eq!(parent.published_members_excluding(40), [42]);
        assert!(parent.has_members_excluding(40));
        assert!(parent.release_process_pid(42));
        assert!(!parent.has_members_excluding(40));
    }

    #[test]
    fn nested_namespace_retains_every_ancestor_identity() {
        let root = Arc::new(PidNamespace::new_root());
        let child = new_namespace(root.clone());
        let grandchild = new_namespace(child.clone());

        let lineage = pid_namespace_lineage(&grandchild);

        assert_eq!(lineage.len(), 3);
        assert!(Arc::ptr_eq(&lineage[0], &grandchild));
        assert!(Arc::ptr_eq(&lineage[1], &child));
        assert!(Arc::ptr_eq(&lineage[2], &root));
    }
}
