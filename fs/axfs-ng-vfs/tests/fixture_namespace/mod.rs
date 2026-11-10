//! Test-only adapters retain each fixture's original storage implementation.
use axfs_ng_vfs::*;

static GATE: ax_sync::Mutex<()> = ax_sync::Mutex::new(());

pub(super) fn location(entry: DirEntry) -> Location {
    struct Root(DirEntry);
    impl FilesystemOps for Root {
        fn name(&self) -> &str {
            "fixture-mount"
        }
        fn root_dir(&self) -> DirEntry {
            self.0.clone()
        }
        fn stat(&self) -> VfsResult<StatFs> {
            Err(VfsError::Unsupported)
        }
    }
    let fs = Filesystem::new(alloc::sync::Arc::new(Root(entry)));
    Mountpoint::new_root(&fs).root_location()
}

pub(super) trait Backend: DirNodeOps {
    fn create_locked(
        &self,
        name: &str,
        kind: NodeType,
        mode: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<DirEntry>;
    fn symlink_locked(
        &self,
        name: &str,
        target: &str,
        mode: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<DirEntry>;
    fn link_locked(&self, name: &str, source: &DirEntry) -> VfsResult<DirEntry>;
    fn unlink_locked(&self, name: &str, directory: bool) -> VfsResult<()>;
}

pub(super) fn run<T: Backend>(
    backend: &T,
    command: &NamespaceMutation<'_>,
    retired: &MutationRetire,
) -> NamespaceOutcome<NamespaceEffect> {
    run_locked(backend, command, retired, GATE.lock())
}

pub(super) fn run_locked<'a, T: Backend>(
    backend: &'a T,
    command: &NamespaceMutation<'_>,
    retired: &MutationRetire,
    gate: ax_sync::MutexGuard<'a, ()>,
) -> NamespaceOutcome<NamespaceEffect> {
    if matches!(command, NamespaceMutation::Rename(_)) {
        return NamespaceOutcome::unchanged(Err(VfsError::OperationNotSupported));
    }
    let mut attempt = Attempt {
        backend,
        _gate: gate,
    };
    command.execute(&mut attempt, retired)
}

struct Attempt<'a, T> {
    _gate: ax_sync::MutexGuard<'a, ()>,
    backend: &'a T,
}

impl<T: Backend> Attempt<'_, T> {
    fn node(&self, entry: DirEntry, name: &str, retired: &MutationRetire) -> VfsResult<RenameNode> {
        retired.retire(entry.clone());
        let metadata = entry.metadata()?;
        let empty = if entry.is_dir() {
            !entry.as_dir()?.has_children()?
        } else {
            false
        };
        Ok(RenameNode {
            entry,
            metadata,
            canonical_name: name.into(),
            cacheable: self.backend.is_cacheable_child(name),
            empty,
        })
    }
    fn target(&self, name: &str, retired: &MutationRetire) -> VfsResult<Option<RenameNode>> {
        match self.backend.lookup(name) {
            Ok(entry) => self.node(entry, name, retired).map(Some),
            Err(VfsError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }
    fn selected(&self, entry: DirEntry, name: &str, retired: &MutationRetire) -> NamespaceEntry {
        retired.retire(entry.clone());
        NamespaceEntry {
            entry,
            canonical_name: name.into(),
            cacheable: self.backend.is_cacheable_child(name),
        }
    }
}

impl<T: Backend> LockedAttempt for Attempt<'_, T> {
    fn parents(&mut self) -> VfsResult<(Metadata, Metadata)> {
        let parent = self.backend.metadata()?;
        Ok((parent.clone(), parent))
    }
    fn observe(
        &mut self,
        command: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<MutationObservation> {
        match command {
            NamespaceMutation::Create(request) => Ok(MutationObservation::Create(
                self.target(request.name(), retired)?,
            )),
            NamespaceMutation::Link(request) => Ok(MutationObservation::Link {
                source: self.node(request.source().clone(), &request.source().name(), retired)?,
                target: self.target(request.name(), retired)?,
            }),
            NamespaceMutation::Unlink(request) => Ok(MutationObservation::Unlink(
                self.target(request.name(), retired)?
                    .ok_or(VfsError::NotFound)?,
            )),
            NamespaceMutation::Rename(_) => Err(VfsError::OperationNotSupported),
        }
    }
    fn is_ancestor(&mut self, _: &NamespaceNode, _: u64) -> VfsResult<bool> {
        Err(VfsError::OperationNotSupported)
    }
    fn commit(
        &mut self,
        authorized: &AuthorizedMutation<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        match authorized {
            AuthorizedMutation::Create(authorized) => {
                let request = authorized.request();
                let (uid, gid) = authorized.owner();
                let result = match request.attributes().content {
                    CreateContent::Node(kind, _) => self.backend.create_locked(
                        request.name(),
                        kind,
                        authorized.permission(),
                        uid,
                        gid,
                    ),
                    CreateContent::Symlink(target) => self.backend.symlink_locked(
                        request.name(),
                        target,
                        authorized.permission(),
                        uid,
                        gid,
                    ),
                };
                match result {
                    Ok(entry) => NamespaceOutcome::applied(
                        NamespaceEffect::Create(CreateEffect {
                            selected: self.selected(entry, request.name(), retired),
                            created: true,
                        }),
                        Ok(()),
                    ),
                    Err(error) => NamespaceOutcome::unchanged(Err(error)),
                }
            }
            AuthorizedMutation::Link(authorized) => {
                let name = authorized.request().name();
                match self.backend.link_locked(name, &authorized.source().entry) {
                    Ok(entry) => NamespaceOutcome::applied(
                        NamespaceEffect::Link(self.selected(entry, name, retired)),
                        Ok(()),
                    ),
                    Err(error) => NamespaceOutcome::unchanged(Err(error)),
                }
            }
            AuthorizedMutation::Unlink(authorized) => {
                match self.backend.unlink_locked(
                    authorized.request().name(),
                    authorized.request().directory(),
                ) {
                    Ok(()) => NamespaceOutcome::applied(
                        NamespaceEffect::Unlink(authorized.target().selected()),
                        Ok(()),
                    ),
                    Err(error) => NamespaceOutcome::unchanged(Err(error)),
                }
            }
            AuthorizedMutation::Rename(_) => {
                NamespaceOutcome::unchanged(Err(VfsError::OperationNotSupported))
            }
        }
    }
}
