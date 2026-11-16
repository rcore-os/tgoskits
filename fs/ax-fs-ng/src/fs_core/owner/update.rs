//! Prepared changes carry source identity and retire all moved owning values.

use axfs_ng_vfs::NamespaceRootSnapshot;

use super::*;

/// The sharing condition selected by the syscall's actual installation path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsSharingRequirement {
    AnyOwner,
    SoleOwner,
}

/// One prepared change; neither a state reference nor a reusable authorization.
#[derive(Debug)]
pub struct PreparedFsUpdate {
    source: Weak<RawSpinLock<FsState>>,
    namespace: Arc<MountNamespace>,
    sharing: FsSharingRequirement,
    change: FsChange,
}

#[derive(Debug)]
enum FsChange {
    Cwd(PathHandle),
    Root(PathHandle),
    Join {
        namespace: Arc<MountNamespace>,
        root: PathHandle,
        cwd: PathHandle,
    },
}

/// Owning resources from successful or rejected updates. Release only after
/// scope leases, CPU pins, state locks and outer namespace writers have ended.
#[derive(Debug, Default)]
pub struct FsUpdateRetirement {
    _update: Option<PreparedFsUpdate>,
    _input: Option<PathHandle>,
}

impl FsContextOwner {
    /// Checks actual source allocation, namespace, and the selected ownership
    /// condition. Path contents may have changed legitimately since snapshot.
    pub fn check_source(
        &self,
        source: &FsContextSnapshot,
        sharing: FsSharingRequirement,
    ) -> VfsResult<()> {
        if !Weak::ptr_eq(&Arc::downgrade(&self.state), &source.source)
            || !Arc::ptr_eq(&self.namespace, &source.namespace)
        {
            return Err(VfsError::BadState);
        }
        check_sharing(&self.state.lock_irqsave(), sharing)
    }

    /// Revalidates before any change, then moves fixed references under one
    /// short lock. Returns all rejected inputs or replaced resources for the
    /// caller to release outside its protections; no backend I/O or allocation.
    pub fn apply_update(
        &mut self,
        update: PreparedFsUpdate,
    ) -> (VfsResult<()>, FsUpdateRetirement) {
        let mut retired = FsUpdateRetirement {
            _update: Some(update),
            ..Default::default()
        };
        let update = retired._update.as_ref().expect("retained input");
        if !Weak::ptr_eq(&Arc::downgrade(&self.state), &update.source)
            || !Arc::ptr_eq(&self.namespace, &update.namespace)
        {
            return (Err(VfsError::BadState), retired);
        }
        let result = {
            let mut state = self.state.lock_irqsave();
            match check_sharing(&state, update.sharing) {
                Err(error) => Err(error),
                Ok(()) => {
                    // All checks precede the first move. Keep provenance in the
                    // retirement as well; consuming a command cannot drop it
                    // while the caller still holds its scope access gate.
                    let update = retired._update.as_mut().expect("retained input");
                    match &mut update.change {
                        FsChange::Cwd(cwd) => core::mem::swap(&mut state.cwd, cwd),
                        FsChange::Root(root) => core::mem::swap(&mut state.root, root),
                        FsChange::Join {
                            namespace,
                            root,
                            cwd,
                        } => {
                            core::mem::swap(&mut state.root, root);
                            core::mem::swap(&mut state.cwd, cwd);
                            core::mem::swap(&mut self.namespace, namespace);
                        }
                    }
                    Ok(())
                }
            }
        };
        (result, retired)
    }
}

impl FsContextSnapshot {
    /// Prepares a cwd-only change, preserving shared root and task namespace.
    /// Directory checks run before the scope commit; failure also returns input.
    pub fn prepare_cwd(
        &self,
        path: PathHandle,
    ) -> (VfsResult<PreparedFsUpdate>, FsUpdateRetirement) {
        self.prepare_directory(path, FsChange::Cwd)
    }

    /// Prepares a root-only change without resetting cwd or changing namespace.
    pub fn prepare_root(
        &self,
        path: PathHandle,
    ) -> (VfsResult<PreparedFsUpdate>, FsUpdateRetirement) {
        self.prepare_directory(path, FsChange::Root)
    }

    fn prepare_directory(
        &self,
        path: PathHandle,
        change: impl FnOnce(PathHandle) -> FsChange,
    ) -> (VfsResult<PreparedFsUpdate>, FsUpdateRetirement) {
        let result = path.location().check_is_dir();
        match result {
            Ok(()) => (
                Ok(self.prepare(change(path), FsSharingRequirement::AnyOwner)),
                Default::default(),
            ),
            Err(error) => (
                Err(error),
                FsUpdateRetirement {
                    _input: Some(path),
                    ..Default::default()
                },
            ),
        }
    }

    /// Consumes one actual target root observation. Even a same-namespace join
    /// resets both shared paths; only this owner's namespace binding changes.
    pub fn prepare_join(
        &self,
        target: NamespaceRootSnapshot,
        sharing: FsSharingRequirement,
    ) -> PreparedFsUpdate {
        let (namespace, root) = target.into_parts();
        let cwd = root.clone();
        self.prepare(
            FsChange::Join {
                namespace,
                root,
                cwd,
            },
            sharing,
        )
    }

    fn prepare(&self, change: FsChange, sharing: FsSharingRequirement) -> PreparedFsUpdate {
        PreparedFsUpdate {
            source: self.source.clone(),
            namespace: self.namespace.clone(),
            sharing,
            change,
        }
    }
}

fn check_sharing(state: &FsState, sharing: FsSharingRequirement) -> VfsResult<()> {
    if sharing == FsSharingRequirement::SoleOwner && state.owners != 1 {
        Err(VfsError::InvalidInput)
    } else {
        Ok(())
    }
}
