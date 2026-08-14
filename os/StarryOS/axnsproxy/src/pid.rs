use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use ax_lazyinit::LazyLock;
use ax_runtime::sync::IrqMutex;

use crate::{PidError, PidResult};

/// Shared ownership handle for one PID namespace generation.
pub type PidNamespaceRef = Arc<PidNamespace>;

/// Shared generation-specific PID identity, analogous to Linux `struct pid`.
pub type PidIdentityRef = Arc<PidIdentity>;

/// Shared ownership of one PID number while it identifies a process group or
/// session.
pub type JobControlIdRef = Arc<JobControlId>;

/// The initial root PID namespace, shared by all processes until
/// they call `unshare(CLONE_NEWPID)` or `clone(CLONE_NEWPID)`.
pub static ROOT_PID_NS: LazyLock<PidNamespaceRef> =
    LazyLock::new(|| Arc::new(PidNamespace::new_root()));

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
    state: IrqMutex<PidNamespaceState>,
}

struct PidNamespaceState {
    /// Next local PID to allocate in this namespace (starts at 1).
    next_pid: u32,
    /// Generation-specific PID entries, including unpublished reservations.
    pid_map: BTreeMap<u64, PidEntry>,
    /// Published namespace-local PIDs indexed back to their global identity.
    global_pid_map: BTreeMap<u32, u64>,
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
    task_retired: bool,
    job_control_refs: usize,
}

struct PidNamespaceIdentity {
    namespace: PidNamespaceRef,
    local_pid: u32,
}

/// Immutable PID numbers assigned to one task generation at every namespace
/// level where it is visible.
///
/// Live namespace indexes answer lookups from a number to a task. They are
/// removed when the task is reaped and may later identify another generation.
/// This snapshot instead travels with delayed identity users such as Unix
/// credentials and pidfds, matching Linux `struct pid` and its immutable
/// namespace-specific `upid` array.
pub struct PidIdentity {
    global_pid: u64,
    mappings: Arc<[PidNamespaceIdentity]>,
}

impl PidIdentity {
    /// Captures every published namespace PID for one task generation.
    pub fn capture(global_pid: u64, namespaces: &[PidNamespaceRef]) -> PidResult<PidIdentityRef> {
        let mut mappings = Vec::new();
        mappings
            .try_reserve_exact(namespaces.len())
            .map_err(|_| PidError::AllocationFailed)?;
        for namespace in namespaces {
            let local_pid = namespace
                .local_pid(global_pid)
                .ok_or(PidError::InvalidState)?;
            mappings.push(PidNamespaceIdentity {
                namespace: namespace.clone(),
                local_pid,
            });
        }
        if mappings.is_empty() {
            return Err(PidError::InvalidState);
        }
        Ok(Arc::new(Self {
            global_pid,
            mappings: mappings.into(),
        }))
    }

    /// Creates the identity used by the initial root process before kernel PID
    /// publication is available.
    pub fn new_root(global_pid: u64) -> PidIdentityRef {
        let local_pid = u32::try_from(global_pid).expect("root PID exceeds the userspace ABI");
        Arc::new(Self {
            global_pid,
            mappings: Arc::from([PidNamespaceIdentity {
                namespace: ROOT_PID_NS.clone(),
                local_pid,
            }]),
        })
    }

    /// Returns the kernel-global numeric identity.
    pub fn global_pid(&self) -> u64 {
        self.global_pid
    }

    /// Projects this generation into an observing PID namespace.
    ///
    /// The saved number remains valid after the live namespace index releases
    /// the task, while an unrelated namespace cannot observe the identity.
    pub fn visible_pid(&self, observer: &PidNamespaceRef) -> Option<u32> {
        self.mappings
            .iter()
            .find(|mapping| Arc::ptr_eq(&mapping.namespace, observer))
            .map(|mapping| mapping.local_pid)
    }
}

/// A process-group or session identifier retained at every visible PID
/// namespace level.
///
/// Linux stores PID, PGID, and SID references in the same refcounted
/// `struct pid`. A process may be reaped while its numeric identity remains a
/// live PGID or SID. This handle gives Starry the same ownership rule instead
/// of coupling job-control visibility to the former leader's task lifetime.
pub struct JobControlId {
    identity: PidIdentityRef,
}

impl JobControlId {
    /// Creates an identity in the root namespace, where IDs need no mapping.
    pub fn new_root(global_pid: u64) -> JobControlIdRef {
        Arc::new(Self {
            identity: PidIdentity::new_root(global_pid),
        })
    }

    /// Retains an already-published process PID as a PGID or SID.
    pub fn retain(identity: PidIdentityRef) -> PidResult<JobControlIdRef> {
        let mut retained: Vec<PidNamespaceRef> = Vec::new();
        for mapping in identity
            .mappings
            .iter()
            .filter(|mapping| mapping.namespace.level() > 0)
        {
            if let Err(error) = mapping
                .namespace
                .retain_job_control_pid(identity.global_pid)
            {
                for retained_namespace in retained.iter().rev() {
                    assert!(
                        retained_namespace.release_job_control_pid(identity.global_pid),
                        "job-control PID retention rollback lost its owner"
                    );
                }
                return Err(error);
            }
            retained.push(mapping.namespace.clone());
        }
        Ok(Arc::new(Self { identity }))
    }

    /// Returns the kernel-global numeric identity.
    pub fn global_pid(&self) -> u64 {
        self.identity.global_pid()
    }
}

impl Drop for JobControlId {
    fn drop(&mut self) {
        for mapping in self
            .identity
            .mappings
            .iter()
            .rev()
            .filter(|mapping| mapping.namespace.level() > 0)
        {
            assert!(
                mapping
                    .namespace
                    .release_job_control_pid(self.identity.global_pid),
                "job-control PID owner outlived its namespace mapping"
            );
        }
    }
}

impl PidNamespace {
    pub fn new_root() -> Self {
        Self {
            id: NEXT_PID_NS_ID.fetch_add(1, Ordering::Relaxed),
            level: 0,
            parent: None,
            state: IrqMutex::new(PidNamespaceState::new(PidNamespaceLifecycle::Root)),
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
            state: IrqMutex::new(PidNamespaceState::new(PidNamespaceLifecycle::AwaitingInit)),
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
    ) -> PidResult<u32> {
        self.state
            .lock()
            .reserve_local_pid(global_tid, kind, namespace_init)
    }

    /// Commits a reservation to Linux-visible task publication.
    pub fn publish_reserved_pid(&self, global_tid: u64, local_pid: u32) -> PidResult<()> {
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

    /// Resolves a published namespace-local PID to its global task identity.
    pub fn global_pid(&self, local_pid: u32) -> Option<u64> {
        if self.level == 0 {
            return Some(local_pid as u64);
        }
        self.state.lock().global_pid(local_pid)
    }

    /// Returns the target PID as seen from `observer` through the target's
    /// descendant namespace.
    ///
    /// Returns [`None`] when `target` is not a descendant of `observer` or a
    /// required namespace-local identity has not been published.
    pub fn visible_pid_chain(
        observer: &PidNamespaceRef,
        target: &PidNamespaceRef,
        global_tid: u64,
    ) -> Option<Vec<u32>> {
        let mut visible = Vec::new();
        for namespace in pid_namespace_lineage(target) {
            visible.push(namespace.local_pid(global_tid)?);
            if Arc::ptr_eq(&namespace, observer) {
                visible.reverse();
                return Some(visible);
            }
        }
        None
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

    /// Returns the published tasks that namespace shutdown must terminate.
    ///
    /// The last live thread of the namespace init may be a non-leader. Its
    /// thread PID remains published until its exit path returns, so treating it
    /// as a victim would make the reaper wait for its own post-exit cleanup.
    pub fn published_shutdown_victims(
        &self,
        init_global_tid: u64,
        reaper_global_tid: u64,
    ) -> Vec<u64> {
        self.state
            .lock()
            .published_shutdown_victims(init_global_tid, reaper_global_tid)
    }

    /// Returns whether namespace shutdown still owns a task or unpublished
    /// reservation that must retire before the reaper can publish exit.
    pub fn has_shutdown_victims(&self, init_global_tid: u64, reaper_global_tid: u64) -> bool {
        self.state
            .lock()
            .has_shutdown_victims(init_global_tid, reaper_global_tid)
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

    fn retain_job_control_pid(&self, global_pid: u64) -> PidResult<()> {
        self.state.lock().retain_job_control_pid(global_pid)
    }

    fn release_job_control_pid(&self, global_pid: u64) -> bool {
        self.state.lock().release_job_control_pid(global_pid)
    }
}

impl PidNamespaceState {
    fn new(lifecycle: PidNamespaceLifecycle) -> Self {
        Self {
            next_pid: 1,
            pid_map: BTreeMap::new(),
            global_pid_map: BTreeMap::new(),
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
    ) -> PidResult<u32> {
        if self.pid_map.contains_key(&global_tid) {
            return Err(PidError::AlreadyExists);
        }
        match (self.lifecycle, namespace_init) {
            (PidNamespaceLifecycle::AwaitingInit, true) => {
                if kind != PidReservationKind::Process {
                    return Err(PidError::InvalidInput);
                }
            }
            (PidNamespaceLifecycle::Active { .. }, false) => {}
            (PidNamespaceLifecycle::Root, _) => return Err(PidError::InvalidInput),
            (
                PidNamespaceLifecycle::AwaitingInit
                | PidNamespaceLifecycle::ShuttingDown { .. }
                | PidNamespaceLifecycle::Dead { .. },
                _,
            )
            | (PidNamespaceLifecycle::Active { .. }, true) => {
                return Err(PidError::NamespaceUnavailable);
            }
        }

        let local = self.next_pid;
        self.next_pid = self
            .next_pid
            .checked_add(1)
            .ok_or(PidError::AllocationFailed)?;
        self.pid_map.insert(
            global_tid,
            PidEntry {
                local_pid: local,
                kind,
                publication: PidPublication::Reserved,
                task_retired: false,
                job_control_refs: 0,
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
    fn publish_reserved_pid(&mut self, global_tid: u64, local_pid: u32) -> PidResult<()> {
        if !matches!(self.lifecycle, PidNamespaceLifecycle::Active { .. }) {
            return Err(PidError::NamespaceUnavailable);
        }
        if self.global_pid_map.contains_key(&local_pid) {
            return Err(PidError::InvalidState);
        }
        let entry = self
            .pid_map
            .get_mut(&global_tid)
            .filter(|entry| entry.local_pid == local_pid)
            .ok_or(PidError::InvalidState)?;
        if entry.publication != PidPublication::Reserved {
            return Err(PidError::InvalidState);
        }
        entry.publication = PidPublication::Published;
        let previous = self.global_pid_map.insert(local_pid, global_tid);
        debug_assert!(previous.is_none());
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
            .filter(|entry| {
                entry.local_pid == local_pid && !entry.task_retired && entry.job_control_refs == 0
            })
            .copied()
        else {
            return false;
        };
        self.pid_map.remove(&global_tid);
        if entry.publication == PidPublication::Published {
            assert_eq!(
                self.global_pid_map.remove(&local_pid),
                Some(global_tid),
                "published PID rollback lost its reverse namespace index"
            );
        }
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
        self.pid_map
            .get(&global_tid)
            .filter(|entry| entry.publication == PidPublication::Published)
            .map(|entry| entry.local_pid)
    }

    fn global_pid(&self, local_pid: u32) -> Option<u64> {
        self.global_pid_map.get(&local_pid).copied()
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
    #[cfg(test)]
    fn published_members_excluding(&self, init_global_tid: u64) -> Vec<u64> {
        self.pid_map
            .iter()
            .filter_map(|(global_tid, entry)| {
                (*global_tid != init_global_tid
                    && !entry.task_retired
                    && entry.publication == PidPublication::Published)
                    .then_some(*global_tid)
            })
            .collect()
    }

    fn published_shutdown_victims(&self, init_global_tid: u64, reaper_global_tid: u64) -> Vec<u64> {
        self.pid_map
            .iter()
            .filter_map(|(global_tid, entry)| {
                (*global_tid != init_global_tid
                    && *global_tid != reaper_global_tid
                    && !entry.task_retired
                    && entry.publication == PidPublication::Published)
                    .then_some(*global_tid)
            })
            .collect()
    }

    /// Returns whether any reservation outside the namespace init remains.
    fn has_members_excluding(&self, init_global_tid: u64) -> bool {
        self.pid_map
            .iter()
            .any(|(global_tid, entry)| *global_tid != init_global_tid && !entry.task_retired)
    }

    fn has_shutdown_victims(&self, init_global_tid: u64, reaper_global_tid: u64) -> bool {
        self.pid_map.iter().any(|(global_tid, entry)| {
            *global_tid != init_global_tid
                && *global_tid != reaper_global_tid
                && !entry.task_retired
        })
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
            entry.kind == kind
                && entry.publication == PidPublication::Published
                && !entry.task_retired
        });
        if matches {
            let entry = self
                .pid_map
                .get_mut(&global_tid)
                .expect("published PID disappeared before release");
            entry.task_retired = true;
            if entry.job_control_refs == 0 {
                self.remove_retired_identity(global_tid);
            }
        }
        matches
    }

    fn retain_job_control_pid(&mut self, global_pid: u64) -> PidResult<()> {
        let entry = self
            .pid_map
            .get_mut(&global_pid)
            .filter(|entry| {
                entry.kind == PidReservationKind::Process
                    && entry.publication == PidPublication::Published
                    && !entry.task_retired
            })
            .ok_or(PidError::NoSuchProcess)?;
        entry.job_control_refs = entry
            .job_control_refs
            .checked_add(1)
            .ok_or(PidError::AllocationFailed)?;
        Ok(())
    }

    fn release_job_control_pid(&mut self, global_pid: u64) -> bool {
        let Some(entry) = self
            .pid_map
            .get_mut(&global_pid)
            .filter(|entry| entry.job_control_refs != 0)
        else {
            return false;
        };
        entry.job_control_refs -= 1;
        if entry.job_control_refs == 0 && entry.task_retired {
            self.remove_retired_identity(global_pid);
        }
        true
    }

    fn remove_retired_identity(&mut self, global_pid: u64) {
        let entry = self
            .pid_map
            .remove(&global_pid)
            .expect("retired PID disappeared before final owner release");
        assert!(entry.task_retired);
        assert_eq!(entry.job_control_refs, 0);
        assert_eq!(
            self.global_pid_map.remove(&entry.local_pid),
            Some(global_pid),
            "PID namespace reverse index diverged from its identity owner"
        );
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

    use super::{
        PidError, PidNamespace, PidNamespaceLifecycle, PidNamespaceRef, PidNamespaceState,
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

    fn reserve_thread(namespace: &mut PidNamespaceState, global_tid: u64) -> u32 {
        namespace
            .reserve_local_pid(global_tid, PidReservationKind::Thread, false)
            .unwrap()
    }

    #[test]
    fn unpublished_pid_reservation_restores_namespace_state() {
        let mut namespace = new_child_state();
        let local_pid = reserve_process(&mut namespace, 42, true);

        assert_eq!(local_pid, 1);
        assert_eq!(namespace.local_pid(42), None);
        assert_eq!(namespace.global_pid(1), None);
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
        assert_eq!(namespace.global_pid(first), Some(42));
        assert_eq!(namespace.local_pid(43), None);
        assert_eq!(namespace.global_pid(second), None);
    }

    #[test]
    fn published_reservation_rollback_removes_both_indexes() {
        let mut namespace = new_child_state();
        let local_pid = reserve_process(&mut namespace, 42, true);
        namespace.publish_reserved_pid(42, local_pid).unwrap();

        assert!(namespace.rollback_pid_reservation(42, local_pid));
        assert_eq!(namespace.local_pid(42), None);
        assert_eq!(namespace.global_pid(local_pid), None);
        assert_eq!(reserve_process(&mut namespace, 43, true), local_pid);
        namespace.publish_reserved_pid(43, local_pid).unwrap();
    }

    #[test]
    fn published_pid_has_one_bidirectional_namespace_identity() {
        let mut namespace = new_child_state();
        let init = reserve_process(&mut namespace, 42, true);

        assert_eq!(namespace.local_pid(42), None);
        assert_eq!(namespace.global_pid(init), None);

        namespace.publish_reserved_pid(42, init).unwrap();
        assert_eq!(namespace.local_pid(42), Some(init));
        assert_eq!(namespace.global_pid(init), Some(42));

        assert!(namespace.release_process_pid(42));
        assert_eq!(namespace.local_pid(42), None);
        assert_eq!(namespace.global_pid(init), None);
    }

    #[test]
    fn reaped_process_keeps_its_job_control_number_until_the_group_releases_it() {
        let mut namespace = new_child_state();
        let init = reserve_process(&mut namespace, 42, true);
        namespace.publish_reserved_pid(42, init).unwrap();
        namespace.retain_job_control_pid(42).unwrap();

        assert!(namespace.release_process_pid(42));
        assert_eq!(namespace.local_pid(42), Some(init));
        assert_eq!(namespace.global_pid(init), Some(42));
        assert!(!namespace.has_members_excluding(0));

        assert!(namespace.release_job_control_pid(42));
        assert_eq!(namespace.local_pid(42), None);
        assert_eq!(namespace.global_pid(init), None);
    }

    #[test]
    fn pid_namespace_init_identity_cannot_be_replaced() {
        let mut namespace = new_child_state();
        let init_pid = reserve_process(&mut namespace, 42, true);

        assert_eq!(init_pid, 1);
        assert_eq!(
            namespace.reserve_local_pid(43, PidReservationKind::Process, true),
            Err(PidError::NamespaceUnavailable)
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
            Err(PidError::NamespaceUnavailable)
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
    fn shutdown_reaper_thread_is_not_its_own_victim() {
        let mut namespace = new_child_state();
        let init = reserve_process(&mut namespace, 42, true);
        namespace.publish_reserved_pid(42, init).unwrap();
        let reaper_thread = reserve_thread(&mut namespace, 43);
        namespace.publish_reserved_pid(43, reaper_thread).unwrap();
        let child = reserve_process(&mut namespace, 44, false);
        namespace.publish_reserved_pid(44, child).unwrap();

        assert!(namespace.begin_shutdown(42));
        assert_eq!(namespace.published_shutdown_victims(42, 43), [44]);
        assert!(namespace.has_shutdown_victims(42, 43));
        assert!(namespace.release_process_pid(44));
        assert!(
            !namespace.has_shutdown_victims(42, 43),
            "the final namespace-reaper thread must not wait for its own post-exit PID release"
        );
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
            Err(PidError::NamespaceUnavailable)
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
