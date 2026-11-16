//! Serialize topology movement through exact shared-path revision.

use axfs_ng_vfs::MountPivotRetirement;

use super::*;

// Global because different namespaces may have owners sharing one FsState.
// Lock order: caller writer -> sequence -> VFS topology, then (after topology
// release) registry -> individual path state. Ordinary reads/updates skip it.
static PIVOT_SEQUENCE: Mutex<()> = Mutex::new(());

/// Complete ownership from a topology pivot and its ordered path revisions.
/// No lock, callback, implicit commit, rollback or filesystem synchronization.
#[derive(Debug, Default)]
pub struct FsPivotRetirement {
    _topology: Option<MountPivotRetirement>,
    _registry: FsRegistryRetirement,
}

#[derive(Debug, Default)]
struct FsRegistryRetirement {
    states: Vec<RegistryState>,
}

struct RegistryState {
    state: Arc<RawSpinLock<FsState>>,
    root: Option<PathHandle>,
    cwd: Option<PathHandle>,
}

impl core::fmt::Debug for RegistryState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RegistryState")
            .field("root", &self.root)
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl FsContextSnapshot {
    /// Pivots using this operation's frozen root and actual namespace, then
    /// revises every exact root/cwd in topology commit order, across namespaces.
    /// May sleep; keep the returned resources until after all caller writers.
    pub fn pivot_root(
        &self,
        new_root: &PathHandle,
        put_old: &PathHandle,
    ) -> (VfsResult<()>, FsPivotRetirement) {
        let mut retired = FsPivotRetirement::default();
        let result = {
            #[cfg(test)]
            tests::run(tests::Stage::BeforeSequence);
            let _sequence = PIVOT_SEQUENCE.lock();
            let (result, topology) = self.namespace.pivot_root(&self.root, new_root, put_old);
            retired._topology = Some(topology);
            drop(_sequence);
            if result.is_ok() {
                #[cfg(test)]
                tests::run(tests::Stage::AfterTopology);
                retired._registry = propagate_pivot_root(&self.root, new_root);
            }
            result
        };
        (result, retired)
    }
}

fn propagate_pivot_root(old: &PathHandle, new: &PathHandle) -> FsRegistryRetirement {
    let states = {
        let mut registry = FS_STATES.lock();
        registry.retain(|state| state.strong_count() != 0);
        registry
            .iter()
            .filter_map(|state| state.upgrade())
            .map(|state| RegistryState {
                state,
                root: None,
                cwd: None,
            })
            .collect()
    };
    let mut retired = FsRegistryRetirement { states };
    #[cfg(test)]
    tests::run(tests::Stage::RegistrySelected);
    for item in &mut retired.states {
        let mut state = item.state.lock_irqsave();
        if old.location().ptr_eq(state.root.location()) {
            item.root = Some(core::mem::replace(&mut state.root, new.clone()));
        }
        if old.location().ptr_eq(state.cwd.location()) {
            item.cwd = Some(core::mem::replace(&mut state.cwd, new.clone()));
        }
    }
    retired
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use core::cell::RefCell;
    use std::sync::mpsc;

    use axfs_ng_vfs::{Location, Mountpoint, MutationCredentials, NodePermission, NodeType};

    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum Stage {
        BeforeSequence,
        AfterTopology,
        RegistrySelected,
    }

    type Hook = (Stage, Box<dyn FnOnce()>);

    std::thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(super) fn run(stage: Stage) {
        let hook = HOOK.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|hook| hook.0 == stage) {
                slot.take()
            } else {
                None
            }
        });
        if let Some((_, hook)) = hook {
            hook();
        }
    }

    fn arm(stage: Stage, hook: impl FnOnce() + 'static) {
        HOOK.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some((stage, Box::new(hook)));
        });
    }

    fn directory(parent: &Location, name: &str) -> Location {
        parent
            .create(
                name,
                NodeType::Directory,
                NodePermission::from_bits_truncate(0o755),
                0,
                0,
                &MutationCredentials::root(),
            )
            .unwrap()
    }

    struct Tree {
        namespace: Arc<MountNamespace>,
        root: PathHandle,
        new: PathHandle,
        put_old: PathHandle,
        second: PathHandle,
        put_second: PathHandle,
    }

    fn tree() -> Tree {
        let anchor = Mountpoint::new_root(&crate::MemoryFs::new());
        let root = anchor
            .root_location()
            .mount(&crate::MemoryFs::new())
            .unwrap()
            .root_location();
        let new = directory(&root, "new")
            .mount(&crate::MemoryFs::new())
            .unwrap()
            .root_location();
        let second = directory(&root, "second")
            .mount(&crate::MemoryFs::new())
            .unwrap()
            .root_location();
        let put_old = directory(&new, "old");
        let put_second = directory(&second, "old");
        Tree {
            namespace: MountNamespace::from_tree(anchor).unwrap().into_namespace(),
            root: PathHandle::acquire(root).unwrap(),
            new: PathHandle::acquire(new).unwrap(),
            put_old: PathHandle::acquire(put_old).unwrap(),
            second: PathHandle::acquire(second).unwrap(),
            put_second: PathHandle::acquire(put_second).unwrap(),
        }
    }

    #[test]
    fn pivots_order_real_topology_and_exact_shared_path_revisions() {
        let _serial = OWNER_TESTS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tree = tree();
        let owner = FsContextOwner::new_root(tree.namespace.clone(), tree.root.clone()).unwrap();
        let frozen = owner.snapshot();
        let independent = frozen.fork_owner();
        let foreign_anchor = Mountpoint::new_root(&crate::MemoryFs::new());
        let foreign = MountNamespace::from_tree(foreign_anchor)
            .unwrap()
            .into_namespace();
        let across_namespace = FsContextOwner::new_root(foreign, tree.root.clone()).unwrap();
        let subdir = PathHandle::acquire(directory(tree.root.location(), "subdir")).unwrap();
        let chrooted = FsContextOwner::new_root(tree.namespace.clone(), subdir.clone()).unwrap();

        let (moved_tx, moved_rx) = mpsc::channel();
        let (scan_tx, scan_rx) = mpsc::channel();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                arm(Stage::AfterTopology, move || {
                    // Observe the real production gate at the exact boundary;
                    // no timeout or shared caller writer can mask its removal.
                    moved_tx.send(PIVOT_SEQUENCE.try_lock().is_none()).unwrap();
                    scan_rx.recv().unwrap();
                });
                frozen.pivot_root(&tree.new, &tree.put_old)
            });
            let held = moved_rx.recv().unwrap();
            let second = scope.spawn(|| {
                arm(Stage::BeforeSequence, move || {
                    attempt_tx.send(()).unwrap();
                });
                frozen.pivot_root(&tree.second, &tree.put_second)
            });
            attempt_rx.recv().unwrap();
            scan_tx.send(()).unwrap();
            let (result, first_retired) = first.join().unwrap();
            result.unwrap();
            let (result, second_retired) = second.join().unwrap();
            result.unwrap();
            assert!(
                held,
                "sequence gate must cover topology movement through registry revision"
            );
            assert!(
                owner
                    .snapshot()
                    .root()
                    .location()
                    .ptr_eq(tree.new.location())
            );
            assert!(
                owner
                    .snapshot()
                    .cwd()
                    .location()
                    .ptr_eq(tree.new.location())
            );
            assert!(
                independent
                    .snapshot()
                    .root()
                    .location()
                    .ptr_eq(tree.new.location())
            );
            assert!(
                across_namespace
                    .snapshot()
                    .root()
                    .location()
                    .ptr_eq(tree.new.location()),
                "Linux exact revisions do not filter by namespace"
            );
            assert!(
                chrooted
                    .snapshot()
                    .root()
                    .location()
                    .ptr_eq(subdir.location())
            );
            assert!(
                frozen.root().location().ptr_eq(tree.root.location()),
                "read snapshots never track later revisions"
            );
            assert!(
                tree.root
                    .location()
                    .mountpoint()
                    .location()
                    .unwrap()
                    .ptr_eq(tree.put_second.location())
            );
            assert!(
                tree.second
                    .location()
                    .mountpoint()
                    .location()
                    .unwrap()
                    .ptr_eq(tree.put_old.location())
            );
            drop((first_retired, second_retired));
        });
    }

    #[test]
    fn pivot_retains_selected_states_until_after_the_caller_releases_protection() {
        let _serial = OWNER_TESTS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tree = tree();
        let owner = FsContextOwner::new_root(tree.namespace.clone(), tree.root.clone()).unwrap();
        let snapshot = owner.snapshot();
        let observer = snapshot.fork_owner();
        let source = Arc::downgrade(&observer.state);
        arm(Stage::RegistrySelected, move || drop(observer));
        let caller = Mutex::new(());
        let writer = caller.lock();
        let (result, retired) = snapshot.pivot_root(&tree.new, &tree.put_old);
        result.unwrap();
        assert!(
            source.upgrade().is_some(),
            "scan owns the final state across caller protection"
        );
        drop(writer);
        assert!(source.upgrade().is_some());
        drop(retired);
        assert!(source.upgrade().is_none());

        let foreign_tree = super::tests::tree();
        let before = tree.namespace.walk_tree().len();
        let writer = caller.lock();
        let (result, rejected) = owner
            .snapshot()
            .pivot_root(&foreign_tree.new, &foreign_tree.put_old);
        assert_eq!(result, Err(VfsError::InvalidInput));
        assert_eq!(tree.namespace.walk_tree().len(), before);
        drop(writer);
        drop(rejected);
    }
}
