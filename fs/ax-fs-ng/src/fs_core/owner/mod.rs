//! Task ownership is separate from immutable operation snapshots.

mod pivot;
mod update;

use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};

use axfs_ng_vfs::{MountNamespace, NamespaceClone, PathHandle, VfsError, VfsResult};
pub use pivot::FsPivotRetirement;
pub use update::{FsSharingRequirement, FsUpdateRetirement, PreparedFsUpdate};

use crate::os::sync::{Mutex, RawSpinLock};

/// One task's share of root/cwd, paired with that task's mount namespace.
///
/// This value cannot be cloned. Moving it preserves one share; explicitly
/// sharing it reserves a share even before a child starts. Snapshots cannot
/// resurrect an owner after its final share has been released.
///
/// ```compile_fail,E0599
/// fn duplicate(owner: ax_fs_ng::FsContextOwner) {
///     let second = owner.clone();
/// }
/// ```
pub struct FsContextOwner {
    state: Arc<RawSpinLock<FsState>>,
    namespace: Arc<MountNamespace>,
}

impl core::fmt::Debug for FsContextOwner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FsContextOwner")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct FsState {
    root: PathHandle,
    cwd: PathHandle,
    owners: usize,
}

/// Frozen paths and namespace for one operation, without a task ownership share.
///
/// The source weak reference identifies an allocation for controlled updates;
/// it neither keeps its mutable state alive nor permits upgrading to an owner.
#[derive(Debug, Clone)]
pub struct FsContextSnapshot {
    root: PathHandle,
    cwd: PathHandle,
    source: Weak<RawSpinLock<FsState>>,
    namespace: Arc<MountNamespace>,
}

// Each shared paths allocation is registered once, independent of how many
// tasks or namespaces share it. Never take this sleepable lock in a scope
// lease, CPU pin, or FsState critical section.
static FS_STATES: Mutex<Vec<Weak<RawSpinLock<FsState>>>> = Mutex::new(Vec::new());

#[cfg(test)]
static OWNER_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl FsContextOwner {
    /// Creates independent root/cwd state. May perform directory I/O and
    /// allocate; call outside scope, pin and namespace writer protections.
    pub fn new_root(namespace: Arc<MountNamespace>, root: PathHandle) -> VfsResult<Self> {
        root.location().check_is_dir()?;
        Ok(Self::from_paths(namespace, root.clone(), root))
    }

    fn from_paths(namespace: Arc<MountNamespace>, root: PathHandle, cwd: PathHandle) -> Self {
        let state = Arc::new(RawSpinLock::new(FsState {
            root,
            cwd,
            owners: 1,
        }));
        {
            let mut registry = FS_STATES.lock();
            registry.retain(|state| state.strong_count() != 0);
            registry.push(Arc::downgrade(&state));
        }
        Self { state, namespace }
    }

    /// Borrows this task's exact namespace, independently of its shared paths.
    pub fn namespace(&self) -> &Arc<MountNamespace> {
        &self.namespace
    }

    /// Copies already owned paths under one short IRQ-safe lock. No allocation,
    /// backend call, topology acquisition, registry access, or owner increment.
    pub fn snapshot(&self) -> FsContextSnapshot {
        let state = self.state.lock_irqsave();
        FsContextSnapshot {
            root: state.root.clone(),
            cwd: state.cwd.clone(),
            source: Arc::downgrade(&self.state),
            namespace: self.namespace.clone(),
        }
    }

    /// Reserves a child share. Installed owners require their task's exclusive
    /// scope write access; the borrow ends before child allocation or startup.
    pub fn share_for_child(&mut self) -> VfsResult<Self> {
        let mut state = self.state.lock_irqsave();
        state.owners = state.owners.checked_add(1).ok_or(VfsError::ValueOverflow)?;
        Ok(Self {
            state: self.state.clone(),
            namespace: self.namespace.clone(),
        })
    }
}

impl Drop for FsContextOwner {
    fn drop(&mut self) {
        {
            let mut state = self.state.lock_irqsave();
            assert!(state.owners != 0, "filesystem owner share underflow");
            state.owners -= 1;
        }
        // Both Arc fields are released only after leaving the IRQ-safe lock.
    }
}

impl FsContextSnapshot {
    /// Borrows the selected root, retaining its exact mount use.
    pub fn root(&self) -> &PathHandle {
        &self.root
    }

    /// Borrows the selected working directory, retaining its exact mount use.
    pub fn cwd(&self) -> &PathHandle {
        &self.cwd
    }

    /// Borrows the namespace selected together with these paths.
    pub fn namespace(&self) -> &Arc<MountNamespace> {
        &self.namespace
    }

    /// Copies these frozen paths into a fresh, independently registered state.
    /// This never upgrades the weak source and does not re-resolve names.
    pub fn fork_owner(&self) -> FsContextOwner {
        FsContextOwner::from_paths(self.namespace.clone(), self.root.clone(), self.cwd.clone())
    }

    /// Transfers actual mounts through this clone's mapping. A result has no
    /// update provenance for the original owner and is only suitable for fork.
    /// Keep the clone and its retirement across caller protections on errors.
    pub fn remap(&self, clone: &mut NamespaceClone) -> VfsResult<Self> {
        if !Arc::ptr_eq(&self.namespace, clone.source_namespace()) {
            return Err(VfsError::BadState);
        }
        Ok(Self {
            root: clone.map_path(&self.root)?,
            cwd: clone.map_path(&self.cwd)?,
            namespace: clone.namespace().clone(),
            source: Weak::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use axfs_ng_vfs::{Mountpoint, MutationCredentials, NodePermission, NodeType};

    use super::*;

    fn owner() -> FsContextOwner {
        let mount = Mountpoint::new_root(&crate::MemoryFs::new());
        let namespace = MountNamespace::from_tree(mount.clone())
            .unwrap()
            .into_namespace();
        FsContextOwner::new_root(
            namespace,
            PathHandle::acquire(mount.root_location()).unwrap(),
        )
        .unwrap()
    }

    fn child(parent: &PathHandle, name: &str) -> PathHandle {
        let location = parent
            .location()
            .create(
                name,
                NodeType::Directory,
                NodePermission::from_bits_truncate(0o755),
                0,
                0,
                &MutationCredentials::root(),
            )
            .unwrap();
        parent.rebind(location).unwrap()
    }

    fn apply(
        owner: &mut FsContextOwner,
        command: VfsResult<PreparedFsUpdate>,
        preparation: FsUpdateRetirement,
    ) {
        let (result, retired) = owner.apply_update(command.unwrap());
        result.unwrap();
        drop((retired, preparation));
    }

    #[test]
    fn shared_paths_preserve_task_namespace_and_explicit_owner_shares() {
        let _serial = OWNER_TESTS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut parent = owner();
        let source = parent.snapshot();
        let dir = child(source.root(), "cwd");
        let mut child_owner = parent.share_for_child().unwrap();
        let read = source.clone();
        assert_eq!(
            parent.check_source(&read, FsSharingRequirement::SoleOwner),
            Err(VfsError::InvalidInput)
        );

        let (command, retired) = source.prepare_cwd(dir.clone());
        apply(&mut child_owner, command, retired);
        assert!(parent.snapshot().cwd().location().ptr_eq(dir.location()));
        assert!(
            source.cwd().location().ptr_eq(source.root().location()),
            "the operation snapshot is frozen"
        );

        let target = owner();
        let target_root = target.snapshot();
        let (root, retired) = target.namespace().snapshot_root();
        let command = child_owner
            .snapshot()
            .prepare_join(root.unwrap(), FsSharingRequirement::AnyOwner);
        let (result, update_retired) = child_owner.apply_update(command);
        result.unwrap();
        drop((update_retired, retired));
        let shared = parent.snapshot();
        assert!(
            shared
                .root()
                .location()
                .ptr_eq(target_root.root().location())
        );
        assert!(
            shared
                .cwd()
                .location()
                .ptr_eq(target_root.root().location())
        );
        assert!(Arc::ptr_eq(parent.namespace(), source.namespace()));
        assert!(Arc::ptr_eq(child_owner.namespace(), target.namespace()));
        assert_eq!(
            child_owner.check_source(&source, FsSharingRequirement::AnyOwner),
            Err(VfsError::BadState)
        );

        // A stale path observation remains a valid source for a root/cwd-only
        // update as long as the actual state and this task's namespace match.
        let (command, retired) = source.prepare_root(dir.clone());
        apply(&mut parent, command, retired);
        assert!(
            child_owner
                .snapshot()
                .root()
                .location()
                .ptr_eq(dir.location())
        );
        assert!(
            child_owner
                .snapshot()
                .cwd()
                .location()
                .ptr_eq(target_root.root().location())
        );

        let (root, retired) = parent.namespace().snapshot_root();
        let command = source.prepare_join(root.unwrap(), FsSharingRequirement::SoleOwner);
        let (result, rejected) = parent.apply_update(command);
        assert_eq!(result, Err(VfsError::InvalidInput));
        assert!(parent.snapshot().root().location().ptr_eq(dir.location()));
        drop((rejected, retired));
        drop(child_owner);
        parent
            .check_source(&read, FsSharingRequirement::SoleOwner)
            .unwrap();
        let (root, retired) = parent.namespace().snapshot_root();
        let command = source.prepare_join(root.unwrap(), FsSharingRequirement::SoleOwner);
        let (result, update_retired) = parent.apply_update(command);
        result.unwrap();
        assert!(
            parent
                .snapshot()
                .root()
                .location()
                .ptr_eq(source.root().location()),
            "same namespace join still resets both paths"
        );
        drop((update_retired, retired));

        parent.state.lock_irqsave().owners = usize::MAX;
        assert!(matches!(
            parent.share_for_child(),
            Err(VfsError::ValueOverflow)
        ));
        parent.state.lock_irqsave().owners = 1;
        parent
            .check_source(&read, FsSharingRequirement::SoleOwner)
            .unwrap();
    }

    #[test]
    fn snapshots_fork_frozen_paths_without_resurrecting_source_state() {
        let _serial = OWNER_TESTS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut original = owner();
        let source = original.snapshot();
        let cwd = child(source.root(), "removed");
        let (command, retired) = source.prepare_cwd(cwd.clone());
        apply(&mut original, command, retired);
        let frozen = original.snapshot();
        source
            .root()
            .location()
            .unlink("removed", true, &MutationCredentials::root())
            .unwrap();
        drop(original);
        assert!(
            frozen.source.upgrade().is_none(),
            "read snapshots must not own mutable FsState"
        );
        let independent = frozen.fork_owner();
        assert!(
            independent
                .snapshot()
                .cwd()
                .location()
                .ptr_eq(cwd.location()),
            "fork retains an unlinked cwd without resolving a display name"
        );
        assert_eq!(
            independent.check_source(&frozen, FsSharingRequirement::AnyOwner),
            Err(VfsError::BadState)
        );
        independent
            .check_source(&independent.snapshot(), FsSharingRequirement::SoleOwner)
            .unwrap();

        let mut cloned = frozen.namespace().clone_namespace();
        let mapped = frozen.remap(&mut cloned).unwrap();
        let mapped_owner = mapped.fork_owner();
        assert!(Arc::ptr_eq(mapped_owner.namespace(), cloned.namespace()));
        assert!(
            mapped
                .root()
                .location()
                .entry()
                .ptr_eq(frozen.root().location().entry())
        );
        assert!(!Arc::ptr_eq(
            mapped.root().location().mountpoint(),
            frozen.root().location().mountpoint()
        ));
        assert!(
            mapped
                .cwd()
                .location()
                .entry()
                .ptr_eq(cwd.location().entry())
        );
        assert_eq!(
            mapped_owner.check_source(&mapped, FsSharingRequirement::AnyOwner),
            Err(VfsError::BadState)
        );
        let other = owner();
        assert!(matches!(
            other.snapshot().remap(&mut cloned),
            Err(VfsError::BadState)
        ));
        let (created, retired) = cloned.into_parts();
        drop((mapped_owner, mapped, retired, created));
    }

    #[test]
    fn updates_return_rejected_inputs_and_replaced_last_path_owners() {
        let _serial = OWNER_TESTS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut current = owner();
        let source = current.snapshot();
        let file = source
            .root()
            .location()
            .create(
                "file",
                NodeType::RegularFile,
                NodePermission::default(),
                0,
                0,
                &MutationCredentials::root(),
            )
            .unwrap();
        let file_key = file.entry().downgrade();
        let file = source.root().rebind(file).unwrap();
        source
            .root()
            .location()
            .unlink("file", false, &MutationCredentials::root())
            .unwrap();
        let (result, retired) = source.prepare_cwd(file);
        assert!(matches!(result, Err(VfsError::NotADirectory)));
        assert!(file_key.upgrade().is_some());
        drop(retired);
        assert!(file_key.upgrade().is_none());

        let cwd = child(source.root(), "cwd");
        let key = cwd.location().entry().downgrade();
        let (command, retired) = source.prepare_cwd(cwd);
        apply(&mut current, command, retired);
        source
            .root()
            .location()
            .unlink("cwd", true, &MutationCredentials::root())
            .unwrap();
        let (command, preparation) = source.prepare_cwd(source.root().clone());
        let (result, retired) = current.apply_update(command.unwrap());
        result.unwrap();
        assert!(
            key.upgrade().is_some(),
            "replaced cwd survives the scope commit boundary"
        );
        drop((retired, preparation));
        assert!(key.upgrade().is_none());

        let rejected = child(source.root(), "rejected");
        let key = rejected.location().entry().downgrade();
        let (command, preparation) = source.prepare_root(rejected);
        source
            .root()
            .location()
            .unlink("rejected", true, &MutationCredentials::root())
            .unwrap();
        let mut unrelated = owner();
        let (result, retired) = unrelated.apply_update(command.unwrap());
        assert_eq!(result, Err(VfsError::BadState));
        assert!(
            key.upgrade().is_some(),
            "rejected command retains its complete input"
        );
        drop((retired, preparation));
        assert!(key.upgrade().is_none());
    }
}
