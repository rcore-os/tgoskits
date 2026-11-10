//! A memory filesystem's gate-bearing rename attempt.

use axfs_ng_vfs::{
    AuthorizedCreate, AuthorizedMutation, AuthorizedRename, CreateContent, CreateEffect,
    LockedAttempt, MutationObservation, MutationRetire, NamespaceEffect, NamespaceEntry,
    NamespaceMutation, NamespaceNode, NamespaceOutcome, RenameEffect, RenameNode,
    RenameObservation,
};

use super::*;

struct MemoryAttempt<'a> {
    // Unlock before destroying any owned attempt resources.
    _gate: crate::os::sync::MutexGuard<'a, ()>,
    source: &'a MemoryNode,
    destination: &'a MemoryNode,
}

impl MemoryNode {
    pub(super) fn run_namespace(
        &self,
        command: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        let destination = match command
            .parents()
            .1
            .as_dir()
            .and_then(|directory| directory.downcast::<Self>())
        {
            Ok(destination) => destination,
            Err(error) => return NamespaceOutcome::unchanged(Err(error)),
        };
        if !Arc::ptr_eq(&self.fs, &destination.fs) {
            return NamespaceOutcome::unchanged(Err(VfsError::CrossesDevices));
        }
        if let NamespaceMutation::Rename(request) = command
            && (request.options().exchange() || request.options().whiteout())
        {
            return NamespaceOutcome::unchanged(Err(VfsError::OperationNotSupported));
        }
        let mut attempt = MemoryAttempt {
            _gate: self.fs.mutation.lock(),
            source: self,
            destination: &destination,
        };
        command.execute(&mut attempt, retired)
    }
}

impl MemoryAttempt<'_> {
    fn create(
        &mut self,
        authorized: &AuthorizedCreate<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        let request = authorized.request();
        let content = request.attributes().content;
        let kind = content.node_type();
        let target_len = match content {
            CreateContent::Symlink(target) => target.len() as u64,
            _ => 0,
        };
        if let Err(error) = self.source.fs.resize_usage(0, target_len) {
            return NamespaceOutcome::unchanged(Err(error));
        }
        let (uid, gid) = authorized.owner();
        let inode = Inode::new(
            &self.source.fs,
            Some(self.source.inode.ino),
            kind,
            authorized.permission(),
            uid,
            gid,
            TMPFS_NESTED_DIR_ENTRIES_SUBCLASS,
        );
        retired.retire(inode.clone());
        match content {
            CreateContent::Symlink(target) => {
                let file = inode.as_file().expect("new symlink file");
                file.length.store(target_len, AtomicOrdering::Release);
                *file.symlink.lock() = Some(target.to_owned());
            }
            CreateContent::Node(_, device) => inode.metadata.lock_irqsave().rdev = device,
        }
        let entry = self
            .source
            .new_entry(request.name(), kind, inode.clone())
            .expect("validated directory");
        retired.retire(entry.clone());
        let directory = self.source.inode.as_dir().expect("locked directory");
        let cookie = directory.next_cookie.fetch_add(1, AtomicOrdering::Relaxed);
        let reference = InodeRef::new(self.source.fs.clone(), inode.ino, kind, cookie);
        let old = directory
            .entries
            .lock_irqsave()
            .insert(request.name().into(), reference);
        assert!(old.is_none(), "authorized vacant slot");
        retired.retire(old);
        NamespaceOutcome::applied(
            NamespaceEffect::Create(CreateEffect {
                selected: NamespaceEntry {
                    entry,
                    canonical_name: request.name().into(),
                    cacheable: true,
                },
                created: true,
            }),
            Ok(()),
        )
    }

    fn observe_node(
        &self,
        parent: &MemoryNode,
        name: &str,
        retired: &MutationRetire,
    ) -> VfsResult<RenameNode> {
        let inode = {
            let entries = parent.inode.as_dir()?.entries.lock_irqsave();
            entries.get(name).ok_or(VfsError::NotFound)?.get()
        };
        retired.retire(inode.clone());
        let metadata = inode.metadata.lock_irqsave().clone();
        let empty = match &inode.content {
            NodeContent::Dir(directory) => directory.entries.lock_irqsave().len() == 2,
            _ => false,
        };
        let entry = parent.new_entry(name, metadata.node_type, inode)?;
        retired.retire(entry.clone());
        Ok(RenameNode {
            entry,
            metadata,
            canonical_name: name.into(),
            cacheable: true,
            empty,
        })
    }
}

impl LockedAttempt for MemoryAttempt<'_> {
    fn parents(&mut self) -> VfsResult<(Metadata, Metadata)> {
        let source = self.source.inode.metadata.lock_irqsave().clone();
        let destination = self.destination.inode.metadata.lock_irqsave().clone();
        if !self
            .source
            .inode
            .as_dir()?
            .entries
            .lock_irqsave()
            .contains_key(".")
            || !self
                .destination
                .inode
                .as_dir()?
                .entries
                .lock_irqsave()
                .contains_key(".")
        {
            return Err(VfsError::NotFound);
        }
        Ok((source, destination))
    }

    fn observe(
        &mut self,
        command: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<MutationObservation> {
        let optional = |attempt: &Self, parent: &MemoryNode, name: &str| match attempt
            .observe_node(parent, name, retired)
        {
            Ok(node) => Ok(Some(node)),
            Err(VfsError::NotFound) => Ok(None),
            Err(error) => Err(error),
        };
        let request = match command {
            NamespaceMutation::Create(request) => {
                return Ok(MutationObservation::Create {
                    target: optional(self, self.source, request.name())?,
                    owner_policy: axfs_ng_vfs::CreateOwnerPolicy::ParentSetgid,
                });
            }
            NamespaceMutation::Link(request) => {
                let source = request.source().downcast::<MemoryNode>()?;
                if !Arc::ptr_eq(&source.fs, &self.source.fs) {
                    return Err(VfsError::CrossesDevices);
                }
                let metadata = source.inode.metadata.lock_irqsave().clone();
                let node = RenameNode {
                    entry: request.source().clone(),
                    metadata,
                    canonical_name: request.source().name(),
                    cacheable: true,
                    empty: false,
                };
                retired.retire(source);
                return Ok(MutationObservation::Link {
                    source: node,
                    target: optional(self, self.source, request.name())?,
                });
            }
            NamespaceMutation::Unlink(request) => {
                return Ok(MutationObservation::Unlink(self.observe_node(
                    self.source,
                    request.name(),
                    retired,
                )?));
            }
            NamespaceMutation::Rename(request) => request,
        };
        let source = self.observe_node(self.source, request.source_name(), retired)?;
        let target = match self.observe_node(self.destination, request.destination_name(), retired)
        {
            Ok(target) => Some(target),
            Err(VfsError::NotFound) => None,
            Err(error) => return Err(error),
        };
        Ok(MutationObservation::Rename(RenameObservation {
            source,
            target,
        }))
    }

    fn is_ancestor(&mut self, ancestor: &NamespaceNode, mut directory: u64) -> VfsResult<bool> {
        let mut visited = Vec::new();
        loop {
            if directory == ancestor.inode {
                return Ok(true);
            }
            if visited.contains(&directory) {
                return Err(VfsError::FilesystemLoop);
            }
            visited.push(directory);
            let inode = self.source.fs.get(directory);
            let parent = inode
                .as_dir()?
                .entries
                .lock_irqsave()
                .get("..")
                .ok_or(VfsError::NotFound)?
                .ino;
            if parent == directory {
                return Ok(false);
            }
            directory = parent;
        }
    }

    fn commit(
        &mut self,
        authorized: &AuthorizedMutation<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        match authorized {
            AuthorizedMutation::Create(authorized) => self.create(authorized, retired),
            AuthorizedMutation::Link(authorized) => {
                let source = authorized
                    .source()
                    .entry
                    .downcast::<MemoryNode>()
                    .expect("observed memory source");
                let directory = self.source.inode.as_dir().expect("locked directory");
                let kind = authorized.source().entry.node_type();
                let name = authorized.request().name();
                let cookie = directory.next_cookie.fetch_add(1, AtomicOrdering::Relaxed);
                let reference =
                    InodeRef::new(self.source.fs.clone(), source.inode.ino, kind, cookie);
                let entry = self
                    .source
                    .new_entry(name, kind, source.inode.clone())
                    .expect("locked directory");
                retired.retire(entry.clone());
                retired.retire(source);
                let old = directory
                    .entries
                    .lock_irqsave()
                    .insert(name.into(), reference);
                assert!(old.is_none(), "authorized vacant link slot");
                retired.retire(old);
                NamespaceOutcome::applied(
                    NamespaceEffect::Link(NamespaceEntry {
                        entry,
                        canonical_name: name.into(),
                        cacheable: true,
                    }),
                    Ok(()),
                )
            }
            AuthorizedMutation::Unlink(authorized) => {
                let directory = self.source.inode.as_dir().expect("locked directory");
                let removed = directory
                    .entries
                    .lock_irqsave()
                    .remove(authorized.request().name())
                    .expect("authorized occupied slot");
                let inode = removed.get();
                retired.retire(MemoryNode::clear_dir_entries(&inode));
                retired.retire(inode);
                retired.retire(removed);
                NamespaceOutcome::applied(
                    NamespaceEffect::Unlink(authorized.target().selected()),
                    Ok(()),
                )
            }
            AuthorizedMutation::Rename(authorized) => self
                .rename(authorized, retired)
                .map(NamespaceEffect::Rename),
        }
    }
}

impl MemoryAttempt<'_> {
    fn rename(
        &mut self,
        authorized: &AuthorizedRename<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<RenameEffect> {
        let request = authorized.request();
        let source_dir = self.source.inode.as_dir().expect("locked directory");
        let destination_dir = self.destination.inode.as_dir().expect("locked directory");
        let source = source_dir
            .entries
            .lock_irqsave()
            .remove(request.source_name())
            .expect("authorized source slot");
        let cookie = destination_dir
            .next_cookie
            .fetch_add(1, AtomicOrdering::Relaxed);
        let moved = InodeRef::new(source.fs.clone(), source.ino, source.node_type, cookie);
        let overwritten = destination_dir
            .entries
            .lock_irqsave()
            .insert(request.destination_name().into(), moved);
        if source.node_type == NodeType::Directory
            && self.source.inode.ino != self.destination.inode.ino
        {
            let inode = source.get();
            let parent = InodeRef::new(
                self.source.fs.clone(),
                self.destination.inode.ino,
                NodeType::Directory,
                2,
            );
            let old = inode
                .as_dir()
                .expect("authorized directory")
                .entries
                .lock_irqsave()
                .insert("..".into(), parent);
            retired.retire(old);
            retired.retire(inode);
        }
        if let Some(entry) = &overwritten {
            let inode = entry.get();
            if let NodeContent::Dir(dir) = &inode.content {
                let old = core::mem::take(&mut *dir.entries.lock_irqsave());
                retired.retire(old);
            }
            retired.retire(inode);
        }
        retired.retire(source);
        retired.retire(overwritten);
        NamespaceOutcome::applied(authorized.effect(), Ok(()))
    }
}
