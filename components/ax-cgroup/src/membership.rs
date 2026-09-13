//! Process-owned membership and task-lifecycle transactions.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

use ax_lazyinit::LazyInit;

use crate::{CgroupError, CgroupNode, CgroupResult, ProcessId, sync::CgroupMutex};

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

impl MembershipState {
    fn has_pending_task(&self, process_id: ProcessId) -> bool {
        self.pending_tasks
            .values()
            .any(|pending_process| *pending_process == process_id)
    }
}

static STATE: LazyInit<CgroupMutex<MembershipState>> = LazyInit::new();

pub(crate) fn init() {
    STATE.init_once(CgroupMutex::new(MembershipState {
        pending_tasks: BTreeMap::new(),
        tasks: BTreeMap::new(),
    }));
}

fn state() -> CgroupResult<&'static CgroupMutex<MembershipState>> {
    STATE.get().ok_or(CgroupError::NotInitialized)
}

/// Authoritative cgroup state owned by one stable process generation.
///
/// The consuming OS serializes this value with its process transaction lock.
/// The hierarchy ledger never calls back into a process or PID registry while
/// holding its non-sleeping lock.
pub struct ProcessMembership {
    state: ProcessMembershipState,
}

enum ProcessMembershipState {
    Active(Arc<CgroupNode>),
    Exited(Arc<CgroupNode>),
}

impl ProcessMembership {
    /// Create active membership in `node`.
    ///
    /// The task ledger and `cgroup.procs` publication are owned by the initial
    /// attach or fork guard, not by this constructor.
    pub fn new(node: Arc<CgroupNode>) -> Self {
        Self {
            state: ProcessMembershipState::Active(node),
        }
    }

    /// Return the current or final hierarchy node for observation.
    pub fn current(&self) -> Arc<CgroupNode> {
        match &self.state {
            ProcessMembershipState::Active(node) | ProcessMembershipState::Exited(node) => {
                Arc::clone(node)
            }
        }
    }

    /// Reserve a child task in this process's current cgroup.
    ///
    /// The caller must hold the process transaction lock so migration cannot
    /// change the authoritative node between selection and reservation.
    pub fn begin_task(
        &self,
        process_id: ProcessId,
        task_id: ProcessId,
        child_kind: CgroupChildKind,
    ) -> CgroupResult<CgroupForkGuard> {
        let ProcessMembershipState::Active(node) = &self.state else {
            return Err(CgroupError::NoSuchProcess);
        };
        begin_task_at(Arc::clone(node), process_id, task_id, child_kind)
    }

    /// Move every live task charge and the process membership to `target`.
    ///
    /// The caller must hold the process transaction lock through this method.
    pub fn migrate(&mut self, process_id: ProcessId, target: Arc<CgroupNode>) -> CgroupResult<()> {
        let ProcessMembershipState::Active(old) = &self.state else {
            return Err(CgroupError::NoSuchProcess);
        };
        migrate_process(process_id, old, &target)?;
        self.state = ProcessMembershipState::Active(target);
        Ok(())
    }

    /// Release one exact task charge and optionally the process membership.
    ///
    /// Repeated final cleanup is idempotent after the membership enters the
    /// exited state. The caller serializes this operation with clone and
    /// migration through the process transaction lock.
    pub fn exit_task(
        &mut self,
        process_id: ProcessId,
        task_id: ProcessId,
        exit_kind: CgroupTaskExit,
    ) -> CgroupResult<()> {
        let ProcessMembershipState::Active(node) = &self.state else {
            return Ok(());
        };
        exit_task(process_id, task_id, node, exit_kind)?;
        if matches!(exit_kind, CgroupTaskExit::LastProcessTask) {
            self.state = ProcessMembershipState::Exited(Arc::clone(node));
        }
        Ok(())
    }

    /// Rename one task generation after Linux de-threading during execve.
    pub fn rename_task(
        &mut self,
        process_id: ProcessId,
        old_task_id: ProcessId,
        new_task_id: ProcessId,
    ) -> CgroupResult<()> {
        let ProcessMembershipState::Active(node) = &self.state else {
            return Err(CgroupError::NoSuchProcess);
        };
        rename_task(process_id, old_task_id, new_task_id, node)
    }
}

/// Attach the initial task to the root without applying a configurable limit.
pub(crate) fn attach_initial_process(root: Arc<CgroupNode>, pid: ProcessId) -> CgroupResult<()> {
    let mut state = state()?.lock_irqsave();
    if state.pending_tasks.contains_key(&pid) || state.tasks.contains_key(&pid) {
        return Err(CgroupError::ResourceBusy);
    }

    let path = ancestry(&root);
    charge_path_unchecked(&path, 1);
    if !root.add_member(pid) {
        uncharge_path(&path, 1);
        return Err(CgroupError::ResourceBusy);
    }
    state.tasks.insert(
        pid,
        TaskMembership {
            process_id: pid,
            cgroup: root,
        },
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForkPublication {
    Prepared,
    Published,
    Committed,
}

/// Owns a reserved task charge until scheduler publication succeeds.
pub struct CgroupForkGuard {
    cgroup: Arc<CgroupNode>,
    task_id: ProcessId,
    task_process_id: ProcessId,
    child_kind: CgroupChildKind,
    charged_path: Vec<Arc<CgroupNode>>,
    publication: ForkPublication,
}

impl CgroupForkGuard {
    /// Return the cgroup selected for the reserved task.
    pub fn cgroup(&self) -> Arc<CgroupNode> {
        Arc::clone(&self.cgroup)
    }

    /// Publish ledger and process membership before PID visibility.
    ///
    /// Dropping the guard after this step still rolls publication back. The
    /// caller commits only after scheduler publication can no longer fail.
    pub fn publish(&mut self) -> CgroupResult<()> {
        if self.publication != ForkPublication::Prepared {
            return Err(CgroupError::ResourceBusy);
        }

        let mut state = state()?.lock_irqsave();
        if state.pending_tasks.get(&self.task_id) != Some(&self.task_process_id)
            || state.tasks.contains_key(&self.task_id)
        {
            return Err(CgroupError::ResourceBusy);
        }
        if matches!(self.child_kind, CgroupChildKind::Process)
            && !self.cgroup.add_member(self.task_id)
        {
            return Err(CgroupError::ResourceBusy);
        }
        state.tasks.insert(
            self.task_id,
            TaskMembership {
                process_id: self.task_process_id,
                cgroup: Arc::clone(&self.cgroup),
            },
        );
        self.publication = ForkPublication::Published;
        Ok(())
    }

    /// Commit membership after every fallible publication step has succeeded.
    pub fn commit(mut self) {
        assert_eq!(
            self.publication,
            ForkPublication::Published,
            "only published cgroup membership can be committed"
        );
        let removed = STATE
            .get()
            .expect("cgroup membership must be initialized")
            .lock_irqsave()
            .pending_tasks
            .remove(&self.task_id);
        assert_eq!(
            removed,
            Some(self.task_process_id),
            "committed task must retain its migration reservation"
        );
        self.publication = ForkPublication::Committed;
    }
}

impl Drop for CgroupForkGuard {
    fn drop(&mut self) {
        match self.publication {
            ForkPublication::Prepared => {
                if let Some(state) = STATE.get() {
                    state.lock_irqsave().pending_tasks.remove(&self.task_id);
                }
                uncharge_path(&self.charged_path, 1);
            }
            ForkPublication::Published => {
                if let Some(state) = STATE.get() {
                    let mut state = state.lock_irqsave();
                    let pending = state.pending_tasks.remove(&self.task_id);
                    debug_assert_eq!(
                        pending,
                        Some(self.task_process_id),
                        "published task must retain its migration reservation"
                    );
                    let removed = state.tasks.remove(&self.task_id);
                    debug_assert!(
                        removed.is_some_and(|membership| {
                            membership.process_id == self.task_process_id
                                && Arc::ptr_eq(&membership.cgroup, &self.cgroup)
                        }),
                        "published cgroup task must remain owned by its rollback guard"
                    );
                }
                if matches!(self.child_kind, CgroupChildKind::Process) {
                    let removed = self.cgroup.remove_member(self.task_id);
                    debug_assert!(removed, "published process member must be rolled back");
                }
                uncharge_path(&self.charged_path, 1);
            }
            ForkPublication::Committed => {}
        }
    }
}

/// Reserve a pids charge for a task that is not runnable yet.
pub(crate) fn begin_task_at(
    target: Arc<CgroupNode>,
    process_id: ProcessId,
    task_id: ProcessId,
    child_kind: CgroupChildKind,
) -> CgroupResult<CgroupForkGuard> {
    let mut state = state()?.lock_irqsave();
    if state.pending_tasks.contains_key(&task_id) || state.tasks.contains_key(&task_id) {
        return Err(CgroupError::ResourceBusy);
    }

    let charged_path = ancestry(&target);
    charge_path(&charged_path)?;
    let task_process_id = match child_kind {
        CgroupChildKind::Process => task_id,
        CgroupChildKind::Thread => process_id,
    };
    state.pending_tasks.insert(task_id, task_process_id);
    Ok(CgroupForkGuard {
        cgroup: target,
        task_id,
        task_process_id,
        child_kind,
        charged_path,
        publication: ForkPublication::Prepared,
    })
}

fn migrate_process(
    process_id: ProcessId,
    old: &Arc<CgroupNode>,
    target: &Arc<CgroupNode>,
) -> CgroupResult<()> {
    let mut state = state()?.lock_irqsave();
    if state.has_pending_task(process_id) {
        return Err(CgroupError::ResourceBusy);
    }
    if Arc::ptr_eq(old, target) {
        return old
            .has_member(process_id)
            .then_some(())
            .ok_or(CgroupError::NoSuchProcess);
    }

    let task_ids: Vec<_> = state
        .tasks
        .iter()
        .filter_map(|(task_id, membership)| {
            (membership.process_id == process_id).then_some(*task_id)
        })
        .collect();
    if task_ids.is_empty()
        || task_ids.iter().any(|task_id| {
            !state
                .tasks
                .get(task_id)
                .is_some_and(|membership| Arc::ptr_eq(&membership.cgroup, old))
        })
    {
        return Err(CgroupError::NoSuchProcess);
    }

    let old_path = ancestry(old);
    let target_path = ancestry(target);
    let (old_unique, target_unique) = unique_paths(&old_path, &target_path);
    let task_count = task_ids.len() as u64;
    charge_path_unchecked(target_unique, task_count);

    if !old.remove_member(process_id) {
        uncharge_path(target_unique, task_count);
        return Err(CgroupError::NoSuchProcess);
    }
    if !target.add_member(process_id) {
        let restored = old.add_member(process_id);
        debug_assert!(
            restored,
            "cgroup migration rollback must restore membership"
        );
        uncharge_path(target_unique, task_count);
        return Err(CgroupError::ResourceBusy);
    }

    for task_id in task_ids {
        state
            .tasks
            .get_mut(&task_id)
            .expect("cgroup task ledger changed while locked")
            .cgroup = Arc::clone(target);
    }
    uncharge_path(old_unique, task_count);
    Ok(())
}

fn exit_task(
    process_id: ProcessId,
    task_id: ProcessId,
    expected_cgroup: &Arc<CgroupNode>,
    exit_kind: CgroupTaskExit,
) -> CgroupResult<()> {
    let mut state = state()?.lock_irqsave();
    let Some(membership) = state.tasks.get(&task_id) else {
        return Ok(());
    };
    if membership.process_id != process_id || !Arc::ptr_eq(&membership.cgroup, expected_cgroup) {
        return Err(CgroupError::NoSuchProcess);
    }
    if matches!(exit_kind, CgroupTaskExit::LastProcessTask) {
        let has_other_task = state.tasks.iter().any(|(other_task_id, membership)| {
            *other_task_id != task_id && membership.process_id == process_id
        });
        if state.has_pending_task(process_id) || has_other_task {
            return Err(CgroupError::ResourceBusy);
        }
    }

    let membership = state
        .tasks
        .remove(&task_id)
        .expect("validated cgroup task disappeared while locked");
    if matches!(exit_kind, CgroupTaskExit::LastProcessTask) {
        let removed = membership.cgroup.remove_member(process_id);
        debug_assert!(removed, "final task must retain process membership");
    }
    uncharge_path(&ancestry(&membership.cgroup), 1);
    Ok(())
}

fn rename_task(
    process_id: ProcessId,
    old_task_id: ProcessId,
    new_task_id: ProcessId,
    expected_cgroup: &Arc<CgroupNode>,
) -> CgroupResult<()> {
    let mut state = state()?.lock_irqsave();
    let membership = state
        .tasks
        .get(&old_task_id)
        .ok_or(CgroupError::NoSuchProcess)?;
    if membership.process_id != process_id || !Arc::ptr_eq(&membership.cgroup, expected_cgroup) {
        return Err(CgroupError::NoSuchProcess);
    }
    if old_task_id != new_task_id
        && (state.pending_tasks.contains_key(&new_task_id)
            || state.tasks.contains_key(&new_task_id))
    {
        return Err(CgroupError::ResourceBusy);
    }
    let membership = state
        .tasks
        .remove(&old_task_id)
        .expect("validated cgroup task disappeared while locked");
    state.tasks.insert(new_task_id, membership);
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
    use std::sync::{LazyLock, Mutex, MutexGuard, Once};

    use super::*;

    static INIT: Once = Once::new();
    static TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn setup() -> MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().expect("cgroup test lock poisoned");
        INIT.call_once(crate::init);
        guard
    }

    fn process_id(id: u64) -> ProcessId {
        ProcessId::new(id).expect("test process generation must be non-zero")
    }

    fn commit_process(target: &Arc<CgroupNode>, pid: ProcessId) -> ProcessMembership {
        let mut guard =
            begin_task_at(Arc::clone(target), pid, pid, CgroupChildKind::Process).unwrap();
        guard.publish().unwrap();
        guard.commit();
        ProcessMembership::new(Arc::clone(target))
    }

    #[test]
    fn published_guard_rolls_back_until_scheduler_commit() {
        let _test = setup();
        let root = crate::root();
        root.write_subtree_control("+pids").unwrap();
        let target = root.create_child("published-rollback-target").unwrap();
        let pid = process_id(4001);

        let mut guard =
            begin_task_at(Arc::clone(&target), pid, pid, CgroupChildKind::Process).unwrap();
        guard.publish().unwrap();
        assert!(target.has_member(pid));
        assert_eq!(target.pids_current_text().unwrap(), "1\n");
        let pending_process = state()
            .unwrap()
            .lock_irqsave()
            .pending_tasks
            .get(&pid)
            .copied();
        assert_eq!(
            pending_process,
            Some(pid),
            "published membership must remain migration-pending until scheduler commit"
        );

        drop(guard);

        assert!(!target.has_member(pid));
        assert_eq!(target.pids_current_text().unwrap(), "0\n");
    }

    #[test]
    fn migration_moves_all_tasks_without_double_charging_common_ancestors() {
        let _test = setup();
        let root = crate::root();
        root.write_subtree_control("+pids").unwrap();
        let parent = root.create_child("owned-migration-parent").unwrap();
        parent.write_subtree_control("+pids").unwrap();
        let source = parent.create_child("source").unwrap();
        let target = parent.create_child("target").unwrap();
        let pid = process_id(4010);
        let tid = process_id(4011);
        let mut membership = commit_process(&source, pid);
        let mut thread = membership
            .begin_task(pid, tid, CgroupChildKind::Thread)
            .unwrap();
        thread.publish().unwrap();
        thread.commit();

        assert_eq!(source.pids_current_text().unwrap(), "2\n");
        assert_eq!(source.pids_peak_text().unwrap(), "2\n");
        assert_eq!(parent.pids_current_text().unwrap(), "2\n");
        assert_eq!(parent.pids_peak_text().unwrap(), "2\n");
        membership.migrate(pid, Arc::clone(&target)).unwrap();

        assert_eq!(source.pids_current_text().unwrap(), "0\n");
        assert_eq!(source.pids_peak_text().unwrap(), "2\n");
        assert_eq!(target.pids_current_text().unwrap(), "2\n");
        assert_eq!(target.pids_peak_text().unwrap(), "2\n");
        assert_eq!(parent.pids_current_text().unwrap(), "2\n");
        assert_eq!(parent.pids_peak_text().unwrap(), "2\n");
        membership
            .exit_task(pid, tid, CgroupTaskExit::Thread)
            .unwrap();
        membership
            .exit_task(pid, pid, CgroupTaskExit::LastProcessTask)
            .unwrap();
        assert_eq!(target.pids_current_text().unwrap(), "0\n");
        assert_eq!(target.pids_peak_text().unwrap(), "2\n");
        assert_eq!(parent.pids_current_text().unwrap(), "0\n");
        assert_eq!(parent.pids_peak_text().unwrap(), "2\n");
    }

    #[test]
    fn pending_thread_blocks_process_migration_and_releases_its_charge() {
        let _test = setup();
        let root = crate::root();
        root.write_subtree_control("+pids").unwrap();
        let source = root.create_child("owned-pending-source").unwrap();
        let target = root.create_child("owned-pending-target").unwrap();
        let pid = process_id(4020);
        let tid = process_id(4021);
        let mut membership = commit_process(&source, pid);
        let pending = membership
            .begin_task(pid, tid, CgroupChildKind::Thread)
            .unwrap();

        assert_eq!(
            membership.migrate(pid, Arc::clone(&source)),
            Err(CgroupError::ResourceBusy),
            "same-node migration must not bypass an in-flight task transaction"
        );
        assert_eq!(
            membership.migrate(pid, Arc::clone(&target)),
            Err(CgroupError::ResourceBusy)
        );
        assert_eq!(source.pids_current_text().unwrap(), "2\n");
        drop(pending);
        assert_eq!(source.pids_current_text().unwrap(), "1\n");

        membership.migrate(pid, Arc::clone(&target)).unwrap();
        membership
            .exit_task(pid, pid, CgroupTaskExit::LastProcessTask)
            .unwrap();
    }

    #[test]
    fn pending_thread_blocks_final_process_exit() {
        let _test = setup();
        let root = crate::root();
        root.write_subtree_control("+pids").unwrap();
        let target = root.create_child("owned-pending-exit-target").unwrap();
        let pid = process_id(4025);
        let tid = process_id(4026);
        let mut membership = commit_process(&target, pid);
        let pending = membership
            .begin_task(pid, tid, CgroupChildKind::Thread)
            .unwrap();

        assert_eq!(
            membership.exit_task(pid, pid, CgroupTaskExit::LastProcessTask),
            Err(CgroupError::ResourceBusy),
            "final exit must not overtake an in-flight task transaction"
        );
        assert!(target.has_member(pid));
        assert_eq!(target.pids_current_text().unwrap(), "2\n");

        drop(pending);
        membership
            .exit_task(pid, pid, CgroupTaskExit::LastProcessTask)
            .unwrap();
    }

    #[test]
    fn explicit_process_reservation_charges_only_its_target() {
        let _test = setup();
        let root = crate::root();
        root.write_subtree_control("+pids").unwrap();
        let source = root.create_child("owned-explicit-source").unwrap();
        let target = root.create_child("owned-explicit-target").unwrap();
        let parent = process_id(4030);
        let child = process_id(4031);
        let mut parent_membership = commit_process(&source, parent);

        let pending = crate::begin_process_at(Arc::clone(&target), child).unwrap();
        assert!(Arc::ptr_eq(&pending.cgroup(), &target));
        assert_eq!(source.pids_current_text().unwrap(), "1\n");
        assert_eq!(target.pids_current_text().unwrap(), "1\n");
        drop(pending);
        assert_eq!(target.pids_current_text().unwrap(), "0\n");

        parent_membership
            .exit_task(parent, parent, CgroupTaskExit::LastProcessTask)
            .unwrap();
    }

    #[test]
    fn pids_limit_denies_clone_but_not_organizational_migration() {
        let _test = setup();
        let root = crate::root();
        root.write_subtree_control("+pids").unwrap();
        let source = root.create_child("owned-limit-source").unwrap();
        let target = root.create_child("owned-limit-target").unwrap();
        let pid = process_id(4040);
        let tid = process_id(4041);
        let mut membership = commit_process(&source, pid);
        target.write_pids_max("0").unwrap();

        membership.migrate(pid, Arc::clone(&target)).unwrap();
        assert_eq!(target.pids_current_text().unwrap(), "1\n");
        assert!(matches!(
            membership.begin_task(pid, tid, CgroupChildKind::Thread),
            Err(CgroupError::LimitExceeded)
        ));
        assert_eq!(target.pids_events_text().unwrap(), "max 1\n");

        membership
            .exit_task(pid, pid, CgroupTaskExit::LastProcessTask)
            .unwrap();
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
        let mut membership = commit_process(&child, pid);
        parent.write_pids_max("1").unwrap();

        assert!(matches!(
            membership.begin_task(pid, tid, CgroupChildKind::Thread),
            Err(CgroupError::LimitExceeded)
        ));
        assert_eq!(parent.pids_current_text().unwrap(), "1\n");
        assert_eq!(parent.pids_peak_text().unwrap(), "1\n");
        assert_eq!(child.pids_current_text().unwrap(), "1\n");
        assert_eq!(child.pids_peak_text().unwrap(), "2\n");
        assert_eq!(parent.pids_events_text().unwrap(), "max 1\n");
        assert_eq!(child.pids_events_text().unwrap(), "max 0\n");

        membership
            .exit_task(pid, pid, CgroupTaskExit::LastProcessTask)
            .unwrap();
        assert_eq!(parent.pids_current_text().unwrap(), "0\n");
        assert_eq!(parent.pids_peak_text().unwrap(), "1\n");
        assert_eq!(child.pids_current_text().unwrap(), "0\n");
        assert_eq!(child.pids_peak_text().unwrap(), "2\n");
    }

    #[test]
    fn de_thread_rename_preserves_the_last_task_charge() {
        let _test = setup();
        let root = crate::root();
        root.write_subtree_control("+pids").unwrap();
        let target = root.create_child("owned-rename-target").unwrap();
        let pid = process_id(4050);
        let tid = process_id(4051);
        let mut membership = commit_process(&target, pid);
        let mut thread = membership
            .begin_task(pid, tid, CgroupChildKind::Thread)
            .unwrap();
        thread.publish().unwrap();
        thread.commit();

        membership
            .exit_task(pid, pid, CgroupTaskExit::Thread)
            .unwrap();
        membership.rename_task(pid, tid, pid).unwrap();
        assert_eq!(target.pids_current_text().unwrap(), "1\n");
        membership
            .exit_task(pid, pid, CgroupTaskExit::LastProcessTask)
            .unwrap();
        assert_eq!(target.pids_current_text().unwrap(), "0\n");
        assert!(!target.has_member(pid));
    }
}
