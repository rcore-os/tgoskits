//! Process membership and task-lifecycle transactions.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

use ax_lazyinit::LazyInit;
use ax_sync::SpinLock;

use crate::{CgroupError, CgroupNode, CgroupResult, ProcessId};

/// Process operations required by cgroup membership management.
pub trait CgroupProvider: Send + Sync {
    /// Return whether the process has already entered zombie state.
    fn is_zombie(&self, pid: ProcessId) -> bool;

    /// Snapshot the process's authoritative cgroup membership.
    fn membership(&self, pid: ProcessId) -> Option<Arc<CgroupNode>>;

    /// Replace the process's authoritative cgroup membership.
    fn set_membership(&self, pid: ProcessId, cgroup: Arc<CgroupNode>);
}

/// Whether a new task is also a new process in `cgroup.procs`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CgroupChildKind {
    Process,
    Thread,
}

/// Whether an exiting task ends its process membership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CgroupTaskExit {
    Thread,
    LastProcessTask,
}

struct MembershipState {
    pending_tasks: BTreeMap<ProcessId, ProcessId>,
    tasks: BTreeMap<ProcessId, TaskMembership>,
}

struct TaskMembership {
    process_id: ProcessId,
    cgroup: Arc<CgroupNode>,
}

static STATE: LazyInit<SpinLock<MembershipState>> = LazyInit::new();
static PROVIDER: LazyInit<&'static dyn CgroupProvider> = LazyInit::new();

pub(crate) fn init() {
    STATE.init_once(SpinLock::new(MembershipState {
        pending_tasks: BTreeMap::new(),
        tasks: BTreeMap::new(),
    }));
}

pub(crate) fn register_provider(provider: &'static dyn CgroupProvider) {
    PROVIDER.init_once(provider);
}

fn state() -> CgroupResult<&'static SpinLock<MembershipState>> {
    STATE.get().ok_or(CgroupError::NotInitialized)
}

fn provider() -> CgroupResult<&'static dyn CgroupProvider> {
    PROVIDER.get().copied().ok_or(CgroupError::NotInitialized)
}

/// Attach the initial task to the root without applying a configurable limit.
pub(crate) fn attach_initial_process(root: Arc<CgroupNode>, pid: ProcessId) -> CgroupResult<()> {
    let mut state = state()?.lock_irqsave();
    if state.tasks.contains_key(&pid) {
        return Err(CgroupError::ResourceBusy);
    }

    charge_path_unchecked(&ancestry(&root), 1);
    root.add_member(pid);
    state.tasks.insert(
        pid,
        TaskMembership {
            process_id: pid,
            cgroup: root,
        },
    );
    Ok(())
}

enum ForkState {
    Pending,
    Committed,
}

/// Rolls back a pending cgroup task reservation unless it is committed.
pub struct CgroupForkGuard {
    cgroup: Arc<CgroupNode>,
    task_id: ProcessId,
    task_process_id: ProcessId,
    child_kind: CgroupChildKind,
    charged_path: Vec<Arc<CgroupNode>>,
    state: ForkState,
}

impl CgroupForkGuard {
    /// Return the cgroup selected for the reserved task.
    pub fn cgroup(&self) -> Arc<CgroupNode> {
        Arc::clone(&self.cgroup)
    }

    /// Publish the reserved membership before the child becomes runnable.
    pub fn commit(&mut self) {
        if matches!(self.state, ForkState::Committed) {
            return;
        }

        let mut state = STATE
            .get()
            // SAFE-EXPECT: a fork guard can only be created after initialization.
            .expect("cgroup membership must be initialized")
            .lock_irqsave();
        state.pending_tasks.remove(&self.task_id);
        if matches!(self.child_kind, CgroupChildKind::Process) {
            self.cgroup.add_member(self.task_id);
        }
        state.tasks.insert(
            self.task_id,
            TaskMembership {
                process_id: self.task_process_id,
                cgroup: Arc::clone(&self.cgroup),
            },
        );
        self.state = ForkState::Committed;
    }
}

impl Drop for CgroupForkGuard {
    fn drop(&mut self) {
        if !matches!(self.state, ForkState::Pending) {
            return;
        }

        if let Some(state) = STATE.get() {
            state.lock_irqsave().pending_tasks.remove(&self.task_id);
        }
        uncharge_path(&self.charged_path, 1);
    }
}

/// Reserve a pids charge for a process or thread that is not runnable yet.
pub(crate) fn begin_task_at(
    target: Arc<CgroupNode>,
    process_id: ProcessId,
    task_id: ProcessId,
    child_kind: CgroupChildKind,
) -> CgroupResult<CgroupForkGuard> {
    let mut state = state()?.lock_irqsave();
    reserve_task(&mut state, target, process_id, task_id, child_kind)
}

/// Resolve the process's current cgroup and reserve a task charge atomically
/// with migration and other membership transactions.
pub(crate) fn begin_task(
    process_id: ProcessId,
    task_id: ProcessId,
    child_kind: CgroupChildKind,
) -> CgroupResult<CgroupForkGuard> {
    let mut state = state()?.lock_irqsave();
    let parent = provider()?
        .membership(process_id)
        .ok_or(CgroupError::NoSuchProcess)?;
    reserve_task(&mut state, parent, process_id, task_id, child_kind)
}

fn reserve_task(
    state: &mut MembershipState,
    target: Arc<CgroupNode>,
    process_id: ProcessId,
    task_id: ProcessId,
    child_kind: CgroupChildKind,
) -> CgroupResult<CgroupForkGuard> {
    if state.pending_tasks.contains_key(&task_id) || state.tasks.contains_key(&task_id) {
        return Err(CgroupError::ResourceBusy);
    }

    let charged_path = ancestry(&target);
    charge_path(&charged_path)?;
    state.pending_tasks.insert(task_id, process_id);
    let task_process_id = match child_kind {
        CgroupChildKind::Process => task_id,
        CgroupChildKind::Thread => process_id,
    };
    Ok(CgroupForkGuard {
        cgroup: target,
        task_id,
        task_process_id,
        child_kind,
        charged_path,
        state: ForkState::Pending,
    })
}

/// Move all live tasks of a process to another cgroup.
pub(crate) fn migrate_process(pid: ProcessId, target: Arc<CgroupNode>) -> CgroupResult<()> {
    let mut state = state()?.lock_irqsave();
    let provider = provider()?;
    if provider.is_zombie(pid) {
        return Err(CgroupError::NoSuchProcess);
    }

    let old = provider.membership(pid).ok_or(CgroupError::NoSuchProcess)?;
    if Arc::ptr_eq(&old, &target) {
        return old
            .has_member(pid)
            .then_some(())
            .ok_or(CgroupError::NoSuchProcess);
    }
    if state
        .pending_tasks
        .values()
        .any(|process_id| *process_id == pid)
    {
        return Err(CgroupError::ResourceBusy);
    }

    let task_ids: Vec<_> = state
        .tasks
        .iter()
        .filter_map(|(task_id, membership)| (membership.process_id == pid).then_some(*task_id))
        .collect();
    if task_ids.is_empty() {
        return Err(CgroupError::NoSuchProcess);
    }
    for task_id in &task_ids {
        if !state
            .tasks
            .get(task_id)
            .is_some_and(|membership| Arc::ptr_eq(&membership.cgroup, &old))
        {
            return Err(CgroupError::ResourceBusy);
        }
    }

    let old_path = ancestry(&old);
    let target_path = ancestry(&target);
    let (old_unique, target_unique) = unique_paths(&old_path, &target_path);
    let task_count = task_ids.len() as u64;
    charge_path_unchecked(target_unique, task_count);

    if !old.remove_member(pid) {
        uncharge_path(target_unique, task_count);
        return Err(CgroupError::NoSuchProcess);
    }

    target.add_member(pid);
    provider.set_membership(pid, Arc::clone(&target));
    for task_id in task_ids {
        state
            .tasks
            .get_mut(&task_id)
            .expect("cgroup task ledger changed while locked")
            .cgroup = Arc::clone(&target);
    }
    uncharge_path(old_unique, task_count);
    Ok(())
}

/// Release one task charge and, for the final task, process membership.
pub(crate) fn exit_task(
    process_pid: ProcessId,
    task_tid: ProcessId,
    exit_kind: CgroupTaskExit,
) -> CgroupResult<()> {
    let mut state = state()?.lock_irqsave();
    let Some(membership) = state.tasks.get(&task_tid) else {
        // Teardown is intentionally idempotent: a repeated exit notification
        // must not decrement the pids ledger twice.
        return Ok(());
    };
    if membership.process_id != process_pid {
        return Err(CgroupError::NoSuchProcess);
    }
    let membership = state
        .tasks
        .remove(&task_tid)
        .expect("validated cgroup task disappeared while locked");

    if matches!(exit_kind, CgroupTaskExit::LastProcessTask) {
        membership.cgroup.remove_member(process_pid);
    }
    uncharge_path(&ancestry(&membership.cgroup), 1);
    Ok(())
}

/// Rename a live task identity after `execve` de-threading.
pub(crate) fn rename_task(
    process_id: ProcessId,
    old_tid: ProcessId,
    new_tid: ProcessId,
) -> CgroupResult<()> {
    let mut state = state()?.lock_irqsave();
    let membership = state
        .tasks
        .get(&old_tid)
        .ok_or(CgroupError::NoSuchProcess)?;
    if membership.process_id != process_id {
        return Err(CgroupError::NoSuchProcess);
    }
    if old_tid != new_tid
        && (state.pending_tasks.contains_key(&new_tid) || state.tasks.contains_key(&new_tid))
    {
        return Err(CgroupError::ResourceBusy);
    }
    let membership = state
        .tasks
        .remove(&old_tid)
        .expect("validated cgroup task disappeared while locked");
    state.tasks.insert(new_tid, membership);
    Ok(())
}

fn ancestry(node: &Arc<CgroupNode>) -> Vec<Arc<CgroupNode>> {
    let mut nodes = Vec::new();
    let mut current = Some(Arc::clone(node));
    while let Some(node) = current {
        current = node.parent();
        nodes.push(node);
    }
    nodes
}

fn charge_path(path: &[Arc<CgroupNode>]) -> CgroupResult<()> {
    for (charged, node) in path.iter().enumerate() {
        if let Err(error) = node.try_charge_pids() {
            uncharge_path(&path[..charged], 1);
            record_max_event(&path[charged..]);
            return Err(error);
        }
    }
    Ok(())
}

fn record_max_event(failed_node_and_ancestors: &[Arc<CgroupNode>]) {
    for node in failed_node_and_ancestors {
        if node.parent().is_some() {
            node.record_pids_max_event();
        }
    }
}

fn charge_path_unchecked(path: &[Arc<CgroupNode>], count: u64) {
    for node in path {
        node.charge_pids_unchecked(count);
    }
}

fn uncharge_path(path: &[Arc<CgroupNode>], count: u64) {
    for node in path {
        node.uncharge_pids(count);
    }
}

fn unique_paths<'a>(
    old_path: &'a [Arc<CgroupNode>],
    target_path: &'a [Arc<CgroupNode>],
) -> (&'a [Arc<CgroupNode>], &'a [Arc<CgroupNode>]) {
    let mut old_unique = old_path.len();
    let mut target_unique = target_path.len();
    while old_unique > 0
        && target_unique > 0
        && Arc::ptr_eq(&old_path[old_unique - 1], &target_path[target_unique - 1])
    {
        old_unique -= 1;
        target_unique -= 1;
    }
    (&old_path[..old_unique], &target_path[..target_unique])
}

#[cfg(test)]
mod tests {
    use alloc::collections::{BTreeMap, BTreeSet};
    use std::sync::{LazyLock, Mutex, MutexGuard, Once};

    use super::*;

    struct MockProvider {
        memberships: Mutex<BTreeMap<ProcessId, Arc<CgroupNode>>>,
        zombies: Mutex<BTreeSet<ProcessId>>,
    }

    impl CgroupProvider for MockProvider {
        fn is_zombie(&self, pid: ProcessId) -> bool {
            self.zombies.lock().unwrap().contains(&pid)
        }

        fn membership(&self, pid: ProcessId) -> Option<Arc<CgroupNode>> {
            self.memberships.lock().unwrap().get(&pid).cloned()
        }

        fn set_membership(&self, pid: ProcessId, cgroup: Arc<CgroupNode>) {
            self.memberships.lock().unwrap().insert(pid, cgroup);
        }
    }

    static PROVIDER: MockProvider = MockProvider {
        memberships: Mutex::new(BTreeMap::new()),
        zombies: Mutex::new(BTreeSet::new()),
    };
    static INIT: Once = Once::new();
    static TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn process_id(id: u64) -> ProcessId {
        ProcessId::new(id).expect("test process generation must be non-zero")
    }

    fn setup() -> MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap();
        INIT.call_once(|| {
            crate::init();
            register_provider(&PROVIDER);
        });
        PROVIDER.memberships.lock().unwrap().clear();
        PROVIDER.zombies.lock().unwrap().clear();
        guard
    }

    fn commit_process(root: &Arc<CgroupNode>, pid: ProcessId) {
        let mut guard =
            begin_task_at(Arc::clone(root), pid, pid, CgroupChildKind::Process).unwrap();
        guard.commit();
        PROVIDER
            .memberships
            .lock()
            .unwrap()
            .insert(pid, Arc::clone(root));
    }

    #[test]
    fn migration_updates_all_task_mappings_and_node_lists() {
        let _guard = setup();
        let root = crate::root();
        let target = root.create_child("migration-target").unwrap();
        let pid = process_id(1001);
        commit_process(&root, pid);

        migrate_process(pid, Arc::clone(&target)).unwrap();

        assert!(!root.has_member(pid));
        assert!(target.has_member(pid));
        assert!(Arc::ptr_eq(&PROVIDER.membership(pid).unwrap(), &target));
        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn same_target_migration_preserves_membership() {
        let _guard = setup();
        let root = crate::root();
        let pid = process_id(1002);
        commit_process(&root, pid);

        assert_eq!(migrate_process(pid, Arc::clone(&root)), Ok(()));
        assert!(root.has_member(pid));

        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn migration_preserves_a_process_that_has_exceeded_its_target_limit() {
        let _guard = setup();
        let root = crate::root();
        let target = root.create_child("over-limit-target").unwrap();
        let pid = process_id(1003);
        commit_process(&root, pid);
        root.write_subtree_control("+pids").unwrap();
        target.write_pids_max("0").unwrap();

        migrate_process(pid, Arc::clone(&target)).unwrap();
        assert_eq!(target.pids_current_text().unwrap(), "1\n");
        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn migration_rejects_missing_and_zombie_processes() {
        let _guard = setup();
        let root = crate::root();
        let target = root.create_child("invalid-target").unwrap();

        assert_eq!(
            migrate_process(process_id(1004), Arc::clone(&target)),
            Err(CgroupError::NoSuchProcess)
        );

        let zombie = process_id(1005);
        commit_process(&root, zombie);
        PROVIDER.zombies.lock().unwrap().insert(zombie);
        assert_eq!(
            migrate_process(zombie, target),
            Err(CgroupError::NoSuchProcess)
        );
        exit_task(zombie, zombie, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn task_guard_rolls_back_or_commits_before_exit() {
        let _guard = setup();
        let root = crate::root();
        let pid = process_id(1006);

        drop(begin_task_at(Arc::clone(&root), pid, pid, CgroupChildKind::Process).unwrap());
        assert!(!root.has_member(pid));

        let mut guard =
            begin_task_at(Arc::clone(&root), pid, pid, CgroupChildKind::Process).unwrap();
        guard.commit();
        drop(guard);
        assert!(root.has_member(pid));

        assert_eq!(exit_task(pid, pid, CgroupTaskExit::LastProcessTask), Ok(()));
        assert_eq!(exit_task(pid, pid, CgroupTaskExit::LastProcessTask), Ok(()));
        assert!(!root.has_member(pid));
    }

    #[test]
    fn explicit_process_reservation_charges_only_the_target_path() {
        let _guard = setup();
        let root = crate::root();
        let source = root.create_child("explicit-process-source").unwrap();
        let target = root.create_child("explicit-process-target").unwrap();
        let parent = process_id(1032);
        let child = process_id(1033);
        root.write_subtree_control("+pids").unwrap();
        commit_process(&source, parent);
        target.write_pids_max("1").unwrap();

        let pending = crate::begin_process_at(Arc::clone(&target), child).unwrap();
        assert!(Arc::ptr_eq(&pending.cgroup(), &target));
        assert_eq!(source.pids_current_text().unwrap(), "1\n");
        assert_eq!(target.pids_current_text().unwrap(), "1\n");

        drop(pending);
        assert_eq!(source.pids_current_text().unwrap(), "1\n");
        assert_eq!(target.pids_current_text().unwrap(), "0\n");

        let mut pending = crate::begin_process_at(Arc::clone(&target), child).unwrap();
        pending.commit();
        assert!(target.has_member(child));
        assert_eq!(source.pids_current_text().unwrap(), "1\n");
        assert_eq!(target.pids_current_text().unwrap(), "1\n");

        exit_task(child, child, CgroupTaskExit::LastProcessTask).unwrap();
        assert!(!target.has_member(child));
        assert_eq!(source.pids_current_text().unwrap(), "1\n");
        assert_eq!(target.pids_current_text().unwrap(), "0\n");

        target.write_pids_max("0").unwrap();
        assert!(matches!(
            crate::begin_process_at(Arc::clone(&target), child),
            Err(CgroupError::LimitExceeded)
        ));
        assert_eq!(source.pids_current_text().unwrap(), "1\n");
        assert_eq!(target.pids_current_text().unwrap(), "0\n");
        assert_eq!(target.pids_events_text().unwrap(), "max 1\n");

        exit_task(parent, parent, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn thread_charge_does_not_create_a_process_member() {
        let _guard = setup();
        let root = crate::root();
        let cgroup = root.create_child("thread-accounting-target").unwrap();
        let pid = process_id(1007);
        let tid = process_id(1008);
        root.write_subtree_control("+pids").unwrap();
        commit_process(&cgroup, pid);

        let mut guard =
            begin_task_at(Arc::clone(&cgroup), pid, tid, CgroupChildKind::Thread).unwrap();
        guard.commit();

        assert!(cgroup.has_member(pid));
        assert!(!cgroup.has_member(tid));
        assert_eq!(cgroup.pids_current_text().unwrap(), "2\n");

        exit_task(pid, tid, CgroupTaskExit::Thread).unwrap();
        assert_eq!(cgroup.pids_current_text().unwrap(), "1\n");
        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn migration_moves_a_complete_thread_group_without_double_charging_ancestors() {
        let _guard = setup();
        let root = crate::root();
        let parent = root.create_child("thread-group-migration-parent").unwrap();
        root.write_subtree_control("+pids").unwrap();
        parent.write_subtree_control("+pids").unwrap();
        let source = parent.create_child("source").unwrap();
        let target = parent.create_child("target").unwrap();
        let pid = process_id(1009);
        let tid = process_id(1010);
        commit_process(&source, pid);

        let mut thread =
            begin_task_at(Arc::clone(&source), pid, tid, CgroupChildKind::Thread).unwrap();
        thread.commit();
        target.write_pids_max("0").unwrap();

        assert_eq!(source.pids_current_text().unwrap(), "2\n");
        assert_eq!(source.pids_peak_text().unwrap(), "2\n");
        assert_eq!(parent.pids_current_text().unwrap(), "2\n");
        assert_eq!(parent.pids_peak_text().unwrap(), "2\n");
        migrate_process(pid, Arc::clone(&target)).unwrap();

        assert!(!source.has_member(pid));
        assert!(target.has_member(pid));
        assert_eq!(source.pids_current_text().unwrap(), "0\n");
        assert_eq!(target.pids_current_text().unwrap(), "2\n");
        assert_eq!(target.pids_peak_text().unwrap(), "2\n");
        assert_eq!(parent.pids_current_text().unwrap(), "2\n");
        assert_eq!(parent.pids_peak_text().unwrap(), "2\n");

        exit_task(pid, tid, CgroupTaskExit::Thread).unwrap();
        assert_eq!(target.pids_current_text().unwrap(), "1\n");
        assert_eq!(parent.pids_current_text().unwrap(), "1\n");
        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();
        assert_eq!(target.pids_current_text().unwrap(), "0\n");
        assert_eq!(target.pids_peak_text().unwrap(), "2\n");
        assert_eq!(parent.pids_current_text().unwrap(), "0\n");
        assert_eq!(parent.pids_peak_text().unwrap(), "2\n");
    }

    #[test]
    fn migration_rejects_a_pending_thread_without_moving_the_process() {
        let _guard = setup();
        let root = crate::root();
        let source = root.create_child("pending-migration-source").unwrap();
        let target = root.create_child("pending-migration-target").unwrap();
        let pid = process_id(1011);
        let tid = process_id(1012);
        root.write_subtree_control("+pids").unwrap();
        commit_process(&source, pid);
        let pending = begin_task(pid, tid, CgroupChildKind::Thread).unwrap();

        assert_eq!(source.pids_current_text().unwrap(), "2\n");
        assert_eq!(
            migrate_process(pid, Arc::clone(&target)),
            Err(CgroupError::ResourceBusy)
        );
        assert!(source.has_member(pid));
        assert!(!target.has_member(pid));
        assert_eq!(source.pids_current_text().unwrap(), "2\n");
        assert_eq!(target.pids_current_text().unwrap(), "0\n");

        drop(pending);
        assert_eq!(source.pids_current_text().unwrap(), "1\n");
        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn migration_tracks_multiple_pending_tasks_by_process_owner() {
        let _guard = setup();
        let root = crate::root();
        let source = root.create_child("multiple-pending-source").unwrap();
        let target = root.create_child("multiple-pending-target").unwrap();
        let pid = process_id(1021);
        let tid1 = process_id(1022);
        let tid2 = process_id(1023);
        root.write_subtree_control("+pids").unwrap();
        commit_process(&source, pid);

        let pending1 = begin_task(pid, tid1, CgroupChildKind::Thread).unwrap();
        let pending2 = begin_task(pid, tid2, CgroupChildKind::Thread).unwrap();
        assert_eq!(
            migrate_process(pid, Arc::clone(&target)),
            Err(CgroupError::ResourceBusy)
        );

        drop(pending1);
        assert_eq!(
            migrate_process(pid, Arc::clone(&target)),
            Err(CgroupError::ResourceBusy)
        );
        drop(pending2);
        migrate_process(pid, Arc::clone(&target)).unwrap();
        assert_eq!(target.pids_current_text().unwrap(), "1\n");
        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn unrelated_pending_task_does_not_block_process_migration() {
        let _guard = setup();
        let root = crate::root();
        let source = root.create_child("unrelated-pending-source").unwrap();
        let target = root.create_child("unrelated-pending-target").unwrap();
        let first = process_id(1024);
        let second = process_id(1025);
        let pending_tid = process_id(1026);
        root.write_subtree_control("+pids").unwrap();
        commit_process(&source, first);
        commit_process(&source, second);

        let pending = begin_task(second, pending_tid, CgroupChildKind::Thread).unwrap();
        migrate_process(first, Arc::clone(&target)).unwrap();
        assert_eq!(target.pids_current_text().unwrap(), "1\n");

        drop(pending);
        exit_task(second, second, CgroupTaskExit::LastProcessTask).unwrap();
        exit_task(first, first, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn committed_task_migrates_without_provider_thread_publication() {
        let _guard = setup();
        let root = crate::root();
        let source = root.create_child("ledger-only-source").unwrap();
        let target = root.create_child("ledger-only-target").unwrap();
        let pid = process_id(1027);
        let tid = process_id(1028);
        root.write_subtree_control("+pids").unwrap();
        commit_process(&source, pid);

        let mut thread =
            begin_task_at(Arc::clone(&source), pid, tid, CgroupChildKind::Thread).unwrap();
        thread.commit();
        // The mock provider intentionally exposes only the process leader.
        migrate_process(pid, Arc::clone(&target)).unwrap();
        assert_eq!(target.pids_current_text().unwrap(), "2\n");

        exit_task(pid, tid, CgroupTaskExit::Thread).unwrap();
        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn leaf_limit_event_is_visible_in_ancestor_events() {
        let _guard = setup();
        let root = crate::root();
        let parent = root.create_child("hierarchical-event-parent").unwrap();
        root.write_subtree_control("+pids").unwrap();
        parent.write_subtree_control("+pids").unwrap();
        let child = parent.create_child("hierarchical-event-child").unwrap();
        let pid = process_id(1013);
        let tid = process_id(1014);
        commit_process(&child, pid);
        child.write_pids_max("1").unwrap();

        assert!(matches!(
            begin_task_at(Arc::clone(&child), pid, tid, CgroupChildKind::Thread),
            Err(CgroupError::LimitExceeded)
        ));
        assert_eq!(child.pids_events_text().unwrap(), "max 1\n");
        assert_eq!(parent.pids_events_text().unwrap(), "max 1\n");

        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn ancestor_rejection_rolls_back_current_but_retains_leaf_peak() {
        let _guard = setup();
        let root = crate::root();
        let parent = root.create_child("peak-rollback-parent").unwrap();
        root.write_subtree_control("+pids").unwrap();
        parent.write_subtree_control("+pids").unwrap();
        let child = parent.create_child("peak-rollback-child").unwrap();
        let pid = process_id(1034);
        let tid = process_id(1035);
        commit_process(&child, pid);
        parent.write_pids_max("1").unwrap();

        assert!(matches!(
            begin_task_at(Arc::clone(&child), pid, tid, CgroupChildKind::Thread),
            Err(CgroupError::LimitExceeded)
        ));
        assert_eq!(parent.pids_current_text().unwrap(), "1\n");
        assert_eq!(parent.pids_peak_text().unwrap(), "1\n");
        assert_eq!(child.pids_current_text().unwrap(), "1\n");
        assert_eq!(child.pids_peak_text().unwrap(), "2\n");
        assert_eq!(parent.pids_events_text().unwrap(), "max 1\n");
        assert_eq!(child.pids_events_text().unwrap(), "max 0\n");

        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();
        assert_eq!(parent.pids_current_text().unwrap(), "0\n");
        assert_eq!(parent.pids_peak_text().unwrap(), "1\n");
        assert_eq!(child.pids_current_text().unwrap(), "0\n");
        assert_eq!(child.pids_peak_text().unwrap(), "2\n");
    }

    #[test]
    fn de_thread_rename_preserves_the_last_task_charge() {
        let _guard = setup();
        let root = crate::root();
        let cgroup = root.create_child("de-thread-rename-target").unwrap();
        let pid = process_id(1015);
        let tid = process_id(1016);
        root.write_subtree_control("+pids").unwrap();
        commit_process(&cgroup, pid);
        let mut thread =
            begin_task_at(Arc::clone(&cgroup), pid, tid, CgroupChildKind::Thread).unwrap();
        thread.commit();

        exit_task(pid, pid, CgroupTaskExit::Thread).unwrap();
        assert!(cgroup.has_member(pid));
        assert_eq!(cgroup.pids_current_text().unwrap(), "1\n");
        assert_eq!(
            rename_task(process_id(1099), tid, pid),
            Err(CgroupError::NoSuchProcess)
        );
        assert_eq!(cgroup.pids_current_text().unwrap(), "1\n");
        rename_task(pid, tid, pid).unwrap();
        assert_eq!(
            exit_task(process_id(1099), pid, CgroupTaskExit::LastProcessTask),
            Err(CgroupError::NoSuchProcess)
        );
        assert_eq!(cgroup.pids_current_text().unwrap(), "1\n");
        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();

        assert!(!cgroup.has_member(pid));
        assert_eq!(cgroup.pids_current_text().unwrap(), "0\n");
    }

    #[test]
    fn rename_rejects_a_live_target_without_losing_the_source_task() {
        let _guard = setup();
        let root = crate::root();
        let cgroup = root.create_child("rename-collision-target").unwrap();
        let pid = process_id(1017);
        let tid = process_id(1018);
        commit_process(&cgroup, pid);
        let mut thread =
            begin_task_at(Arc::clone(&cgroup), pid, tid, CgroupChildKind::Thread).unwrap();
        thread.commit();

        assert_eq!(rename_task(pid, tid, pid), Err(CgroupError::ResourceBusy));
        exit_task(pid, tid, CgroupTaskExit::Thread).unwrap();
        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn task_reservation_uses_the_process_current_membership() {
        let _guard = setup();
        let root = crate::root();
        let source = root.create_child("reservation-source").unwrap();
        let target = root.create_child("reservation-target").unwrap();
        let pid = process_id(1019);
        let tid = process_id(1020);
        root.write_subtree_control("+pids").unwrap();
        commit_process(&source, pid);
        migrate_process(pid, Arc::clone(&target)).unwrap();

        let mut task = begin_task(pid, tid, CgroupChildKind::Thread).unwrap();
        assert!(Arc::ptr_eq(&task.cgroup(), &target));
        assert_eq!(source.pids_current_text().unwrap(), "0\n");
        assert_eq!(target.pids_current_text().unwrap(), "2\n");

        task.commit();
        exit_task(pid, tid, CgroupTaskExit::Thread).unwrap();
        exit_task(pid, pid, CgroupTaskExit::LastProcessTask).unwrap();
    }

    #[test]
    fn process_task_commits_under_the_child_process_owner() {
        let _guard = setup();
        let root = crate::root();
        let cgroup = root.create_child("process-owner-target").unwrap();
        let parent = process_id(1030);
        let child = process_id(1031);

        let mut process =
            begin_task_at(Arc::clone(&cgroup), parent, child, CgroupChildKind::Process).unwrap();
        process.commit();

        assert_eq!(
            exit_task(parent, child, CgroupTaskExit::LastProcessTask),
            Err(CgroupError::NoSuchProcess)
        );
        assert!(cgroup.has_member(child));
        exit_task(child, child, CgroupTaskExit::LastProcessTask).unwrap();
        assert!(!cgroup.has_member(child));
    }
}
