use alloc::{collections::BTreeSet, sync::Arc};

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

struct MembershipState {
    pending_forks: BTreeSet<ProcessId>,
}

static STATE: LazyInit<SpinLock<MembershipState>> = LazyInit::new();
static PROVIDER: LazyInit<&'static dyn CgroupProvider> = LazyInit::new();

pub(crate) fn init() {
    STATE.init_once(SpinLock::new(MembershipState {
        pending_forks: BTreeSet::new(),
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

pub(crate) fn attach_initial_process(root: Arc<CgroupNode>, pid: ProcessId) -> CgroupResult<()> {
    let _state = state()?.lock_irqsave();
    root.add_member(pid);
    Ok(())
}

enum ForkState {
    Pending,
    Committed,
}

/// Rolls back a pending cgroup fork unless membership is committed.
pub struct CgroupForkGuard {
    cgroup: Arc<CgroupNode>,
    pid: ProcessId,
    state: ForkState,
}

impl CgroupForkGuard {
    /// Publish inherited membership before the child becomes runnable.
    pub fn commit(&mut self) {
        let mut state = STATE
            .get()
            // SAFE-EXPECT: a fork guard can only be created after membership initialization.
            .expect("cgroup membership must be initialized")
            .lock_irqsave();
        state.pending_forks.remove(&self.pid);
        self.cgroup.add_member(self.pid);
        self.state = ForkState::Committed;
    }
}

impl Drop for CgroupForkGuard {
    fn drop(&mut self) {
        if matches!(self.state, ForkState::Pending)
            && let Some(state) = STATE.get()
        {
            state.lock_irqsave().pending_forks.remove(&self.pid);
        }
    }
}

pub(crate) fn begin_fork(
    parent: Arc<CgroupNode>,
    child_pid: ProcessId,
) -> CgroupResult<CgroupForkGuard> {
    let mut state = state()?.lock_irqsave();
    if !state.pending_forks.insert(child_pid) {
        return Err(CgroupError::ResourceBusy);
    }
    Ok(CgroupForkGuard {
        cgroup: parent,
        pid: child_pid,
        state: ForkState::Pending,
    })
}

pub(crate) fn migrate_process(pid: ProcessId, target: Arc<CgroupNode>) -> CgroupResult<()> {
    let state = state()?.lock_irqsave();
    if state.pending_forks.contains(&pid) {
        return Err(CgroupError::ResourceBusy);
    }

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
    if !old.remove_member(pid) {
        return Err(CgroupError::NoSuchProcess);
    }
    target.add_member(pid);
    provider.set_membership(pid, target);
    Ok(())
}

pub(crate) fn exit_process(pid: ProcessId) -> CgroupResult<()> {
    let _state = state()?.lock_irqsave();
    let provider = provider()?;
    let cgroup = provider.membership(pid).ok_or(CgroupError::NoSuchProcess)?;
    cgroup.remove_member(pid);
    Ok(())
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

    #[test]
    fn migration_updates_node_lists_and_authoritative_handle() {
        let _guard = setup();
        let root = crate::root();
        let target = root.create_child("migration-target").unwrap();
        let pid = process_id(1001);
        root.add_member(pid);
        PROVIDER
            .memberships
            .lock()
            .unwrap()
            .insert(pid, root.clone());

        migrate_process(pid, target.clone()).unwrap();

        assert!(!root.has_member(pid));
        assert!(target.has_member(pid));
        assert!(Arc::ptr_eq(&PROVIDER.membership(pid).unwrap(), &target));
        exit_process(pid).unwrap();
    }

    #[test]
    fn same_target_migration_preserves_membership() {
        let _guard = setup();
        let root = crate::root();
        let pid = process_id(1002);
        root.add_member(pid);
        PROVIDER
            .memberships
            .lock()
            .unwrap()
            .insert(pid, root.clone());

        assert_eq!(migrate_process(pid, root.clone()), Ok(()));
        assert!(root.has_member(pid));
        exit_process(pid).unwrap();
    }

    #[test]
    fn migration_rejects_missing_and_zombie_processes() {
        let _guard = setup();
        let root = crate::root();
        let target = root.create_child("invalid-target").unwrap();

        assert_eq!(
            migrate_process(process_id(1003), target.clone()),
            Err(CgroupError::NoSuchProcess)
        );

        let zombie = process_id(1004);
        PROVIDER.memberships.lock().unwrap().insert(zombie, root);
        PROVIDER.zombies.lock().unwrap().insert(zombie);
        assert_eq!(
            migrate_process(zombie, target),
            Err(CgroupError::NoSuchProcess)
        );
    }

    #[test]
    fn fork_guard_rolls_back_or_commits_before_exit() {
        let _guard = setup();
        let root = crate::root();
        let pid = process_id(1005);
        PROVIDER
            .memberships
            .lock()
            .unwrap()
            .insert(pid, root.clone());

        drop(begin_fork(root.clone(), pid).unwrap());
        assert!(!root.has_member(pid));

        let mut guard = begin_fork(root.clone(), pid).unwrap();
        guard.commit();
        drop(guard);
        assert!(root.has_member(pid));

        assert_eq!(exit_process(pid), Ok(()));
        assert_eq!(exit_process(pid), Ok(()));
        assert!(!root.has_member(pid));
    }
}
