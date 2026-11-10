//! Ext4 keeps admission across retries, but authorizes and publishes per attempt.

use alloc::{sync::Arc, vec::Vec};

use axfs_ng_vfs::{
    AuthorizedCreate, AuthorizedMutation, AuthorizedRename, CreateContent, CreateEffect,
    LockedAttempt, Metadata, MutationObservation, MutationRetire, NamespaceEffect, NamespaceEntry,
    NamespaceMutation, NamespaceNode, NamespaceOutcome, NamespaceState, NodeOps, NodeType,
    RenameEffect, RenameNode, RenameObservation, VfsError, VfsResult,
};
use rsext4::{
    DeviceNumber, Ext4Error, Ext4Result, FileName, FilePermissions, InodeNumber, MutationContext,
    SpecialInodeKind,
};

use super::{
    super::{
        super::inode::{Inode, metadata_from_info},
        Ext4Filesystem, Ext4Guard, LocatedInode, ReapClaim, into_vfs_err,
        mutation::MutationAttempt,
    },
    NamespaceChange, NamespaceChangeGuard,
};

struct Ext4Attempt<'a> {
    state: Ext4Guard<'a>,
    _namespace: NamespaceChangeGuard,
    filesystem: &'a Arc<Ext4Filesystem>,
    source: &'a Inode,
    destination: &'a Inode,
    error: Option<Ext4Error>,
    reap: Option<ReapClaim>,
}

impl Ext4Filesystem {
    pub(crate) fn run_namespace(
        self: &Arc<Self>,
        command: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        let (source, destination) = match self.command_parents(command) {
            Ok(parents) => parents,
            Err(error) => return NamespaceOutcome::unchanged(Err(error)),
        };
        if self.lock().has_pending_reaps() {
            crate::file::prune_cached_files_retiring(retired);
        }
        let (outcome, reap) = {
            let _admission = match self.admission.enter() {
                Ok(admission) => admission,
                Err(error) => return NamespaceOutcome::unchanged(Err(into_vfs_err(error))),
            };
            loop {
                let completed = {
                    let scope = match command {
                        NamespaceMutation::Rename(_) => NamespaceChange::Topology,
                        NamespaceMutation::Unlink(request) if request.directory() => {
                            NamespaceChange::Topology
                        }
                        _ => NamespaceChange::Directory,
                    };
                    let namespace = match self.namespace.change(source.number(), scope) {
                        Ok(namespace) => namespace,
                        Err(error) => return NamespaceOutcome::unchanged(Err(error)),
                    };
                    let state = self.lock();
                    if state.staging {
                        None
                    } else {
                        let mut attempt = Ext4Attempt {
                            state,
                            _namespace: namespace,
                            filesystem: self,
                            source: &source,
                            destination: &destination,
                            error: None,
                            reap: None,
                        };
                        let outcome = command.execute(&mut attempt, retired);
                        Some((outcome, attempt.error.take(), attempt.reap.take()))
                    }
                };
                let Some((outcome, error, reap)) = completed else {
                    if let Err(error) = self.finish_mutation_attempt::<()>(MutationAttempt::Staging)
                    {
                        return NamespaceOutcome::unchanged(Err(into_vfs_err(error)));
                    }
                    continue;
                };
                // The attempt and all VFS publication guards have ended before
                // journal progress, abort persistence, or retry authorization.
                if let Some(error) = error {
                    let completion = self
                        .finish_mutation_attempt::<()>(MutationAttempt::Finished(Err(error)))
                        .map(|_| ())
                        .map_err(into_vfs_err);
                    if error.requires_journal_progress()
                        && matches!(
                            outcome.state(),
                            NamespaceState::Retry | NamespaceState::Unchanged
                        )
                    {
                        // The retry has not modified names. A failed preparation
                        // for the next attempt is not a replayable Retry result.
                        if let Err(error) = completion {
                            return NamespaceOutcome::unchanged(Err(error));
                        }
                        continue;
                    }
                    return outcome.with_completion(completion);
                }
                match outcome.state() {
                    NamespaceState::Applied => break (outcome, reap),
                    NamespaceState::Unchanged if outcome.observation().is_some() => {
                        break (outcome, reap);
                    }
                    _ => return outcome,
                }
            }
        };
        if matches!(outcome.observation(), Some(NamespaceEffect::Create(effect)) if !effect.created)
        {
            return outcome;
        }
        // Do not recursively enter operation admission while retaining its
        // outer guard: a concurrent lifecycle pause may be draining that guard.
        let completion = (|| {
            if let Some(claim) = reap {
                self.reap(claim)?;
            }
            if !self.background_writeback_enabled()
                || source.writeback_policy()?.syncs_directory()
                || destination.writeback_policy()?.syncs_directory()
            {
                self.sync_to_disk()?;
            }
            Ok(())
        })();
        outcome.with_completion(completion)
    }

    fn command_parents(
        self: &Arc<Self>,
        command: &NamespaceMutation<'_>,
    ) -> VfsResult<(Arc<Inode>, Arc<Inode>)> {
        let (source, destination) = command.parents();
        let source = source.as_dir()?.downcast::<Inode>()?;
        let destination = destination.as_dir()?.downcast::<Inode>()?;
        if !Arc::ptr_eq(self, source.fs()) || !Arc::ptr_eq(self, destination.fs()) {
            return Err(VfsError::CrossesDevices);
        }
        match command {
            NamespaceMutation::Create(request) => {
                FileName::new(request.name().as_bytes()).map_err(into_vfs_err)?;
            }
            NamespaceMutation::Link(request) => {
                FileName::new(request.name().as_bytes()).map_err(into_vfs_err)?;
            }
            NamespaceMutation::Unlink(request) => {
                FileName::new(request.name().as_bytes()).map_err(into_vfs_err)?;
            }
            NamespaceMutation::Rename(request) => {
                FileName::new(request.source_name().as_bytes()).map_err(into_vfs_err)?;
                FileName::new(request.destination_name().as_bytes()).map_err(into_vfs_err)?;
            }
        }
        Ok((source, destination))
    }
}

impl Ext4Attempt<'_> {
    fn failed(&mut self, error: Ext4Error) -> NamespaceOutcome<NamespaceEffect> {
        self.error = Some(error);
        if error.requires_journal_progress() {
            NamespaceOutcome::retry()
        } else {
            NamespaceOutcome::uncertain(None, Err(into_vfs_err(error)))
        }
    }

    fn created_entry(
        &mut self,
        info: rsext4::InodeInfo,
        name: &str,
        retired: &MutationRetire,
    ) -> NamespaceEntry {
        let lifetime = self.state.retain_inode(self.filesystem, info.number);
        let entry = self.source.create_entry(
            LocatedInode {
                lifetime,
                file_type: info.file_type(),
            },
            name,
        );
        retired.retire(entry.clone());
        NamespaceEntry {
            entry,
            canonical_name: name.into(),
            cacheable: true,
        }
    }

    fn create(
        &mut self,
        authorized: &AuthorizedCreate<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        let request = authorized.request();
        let name = FileName::new(request.name().as_bytes()).expect("validated name");
        let permissions =
            FilePermissions::new(authorized.permission().bits()).expect("permission bits");
        let (uid, gid) = authorized.owner();
        let context = MutationContext::new(uid, gid, 0, 0);
        let parent = self.source.number();
        self.state.dirty = true;
        let result = match request.attributes().content {
            CreateContent::Symlink(target) => {
                self.state
                    .ext4
                    .create_symlink(context, parent, name, target.as_bytes())
            }
            CreateContent::Node(kind, device) => match kind {
                NodeType::RegularFile => {
                    self.state
                        .ext4
                        .create_regular_file(context, parent, name, permissions)
                }
                NodeType::Directory => {
                    self.state
                        .ext4
                        .create_directory(context, parent, name, permissions)
                }
                NodeType::CharacterDevice | NodeType::BlockDevice => {
                    let device = match DeviceNumber::new(device.major(), device.minor()) {
                        Ok(device) => device,
                        Err(error) => return NamespaceOutcome::unchanged(Err(into_vfs_err(error))),
                    };
                    let kind = if kind == NodeType::CharacterDevice {
                        SpecialInodeKind::CharacterDevice(device)
                    } else {
                        SpecialInodeKind::BlockDevice(device)
                    };
                    self.state
                        .ext4
                        .create_special_inode(context, parent, name, permissions, kind)
                }
                NodeType::Fifo => self.state.ext4.create_special_inode(
                    context,
                    parent,
                    name,
                    permissions,
                    SpecialInodeKind::Fifo,
                ),
                NodeType::Socket => self.state.ext4.create_special_inode(
                    context,
                    parent,
                    name,
                    permissions,
                    SpecialInodeKind::Socket,
                ),
                _ => return NamespaceOutcome::unchanged(Err(VfsError::InvalidInput)),
            },
        };
        match result {
            Ok(info) => {
                let selected = self.created_entry(info, request.name(), retired);
                NamespaceOutcome::applied(
                    NamespaceEffect::Create(CreateEffect {
                        selected,
                        created: true,
                    }),
                    Ok(()),
                )
            }
            Err(error) => self.failed(error),
        }
    }

    fn core<T>(&mut self, result: Ext4Result<T>) -> VfsResult<T> {
        result.map_err(|error| {
            self.error = Some(error);
            into_vfs_err(error)
        })
    }

    fn observe_node(
        &mut self,
        parent: &Inode,
        name: &str,
        retired: &MutationRetire,
    ) -> VfsResult<RenameNode> {
        let name_raw = FileName::new(name.as_bytes()).map_err(into_vfs_err)?;
        let result = self.state.ext4.lookup_child(parent.number(), name_raw);
        let inode = self.core(result)?.ok_or(VfsError::NotFound)?;
        let number = inode.number;
        let node_type = super::super::super::util::directory_entry_type_to_vfs(inode.file_type());
        let file_type = inode.file_type();
        let metadata = metadata_from_info(number.as_u64(), self.filesystem.block_size(), inode);
        let empty = if node_type == NodeType::Directory {
            let result = self
                .state
                .ext4
                .read_directory(number, rsext4::DirectoryCursor::Start, 3);
            self.core(result)?
                .iter()
                .all(|entry| matches!(entry.name.as_slice(), b"." | b".."))
        } else {
            false
        };
        let lifetime = self.state.retain_inode(self.filesystem, number);
        let entry = parent.create_entry(
            LocatedInode {
                lifetime,
                file_type,
            },
            name,
        );
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

impl LockedAttempt for Ext4Attempt<'_> {
    fn parents(&mut self) -> VfsResult<(Metadata, Metadata)> {
        let result = self.state.ensure_linked_parent(self.source.number());
        self.core(result)?;
        let result = self.state.ensure_linked_parent(self.destination.number());
        self.core(result)?;
        let result = self.state.ext4.inode(self.source.number());
        let source = self.core(result)?;
        let result = self.state.ext4.inode(self.destination.number());
        let destination = self.core(result)?;
        Ok((
            metadata_from_info(
                self.source.number().as_u64(),
                self.filesystem.block_size(),
                source,
            ),
            metadata_from_info(
                self.destination.number().as_u64(),
                self.filesystem.block_size(),
                destination,
            ),
        ))
    }

    fn observe(
        &mut self,
        command: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<MutationObservation> {
        let request = match command {
            NamespaceMutation::Create(request) => {
                let target = match self.observe_node(self.source, request.name(), retired) {
                    Ok(target) => Some(target),
                    Err(VfsError::NotFound) => None,
                    Err(error) => return Err(error),
                };
                return Ok(MutationObservation::Create {
                    target,
                    owner_policy: axfs_ng_vfs::CreateOwnerPolicy::ParentSetgid,
                });
            }
            NamespaceMutation::Link(request) => {
                let source = request.source().downcast::<Inode>()?;
                if !Arc::ptr_eq(self.filesystem, source.fs()) {
                    return Err(VfsError::CrossesDevices);
                }
                let result = self.state.ext4.inode(source.number());
                let info = self.core(result)?;
                let metadata = metadata_from_info(
                    source.number().as_u64(),
                    self.filesystem.block_size(),
                    info,
                );
                let node = RenameNode {
                    entry: request.source().clone(),
                    metadata,
                    canonical_name: request.source().name(),
                    cacheable: true,
                    empty: false,
                };
                retired.retire(source);
                let target = match self.observe_node(self.source, request.name(), retired) {
                    Ok(target) => Some(target),
                    Err(VfsError::NotFound) => None,
                    Err(error) => return Err(error),
                };
                return Ok(MutationObservation::Link {
                    source: node,
                    target,
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
            let number =
                InodeNumber::new(directory.try_into().map_err(|_| VfsError::ValueOverflow)?)
                    .map_err(into_vfs_err)?;
            let result = self
                .state
                .ext4
                .lookup_child_number(number, FileName::new(b"..").expect("constant name"));
            let parent = self.core(result)?.ok_or(VfsError::NotFound)?.as_u64();
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
                let request = authorized.request();
                let source = authorized
                    .source()
                    .entry
                    .downcast::<Inode>()
                    .expect("observed ext4 source");
                self.state.dirty = true;
                let result = self.state.ext4.hard_link(
                    source.number(),
                    self.source.number(),
                    FileName::new(request.name().as_bytes()).expect("validated name"),
                );
                retired.retire(source);
                match result {
                    Ok(info) => {
                        let entry = self.created_entry(info, request.name(), retired);
                        NamespaceOutcome::applied(NamespaceEffect::Link(entry), Ok(()))
                    }
                    Err(error) => self.failed(error),
                }
            }
            AuthorizedMutation::Unlink(authorized) => {
                let request = authorized.request();
                let name = FileName::new(request.name().as_bytes()).expect("validated name");
                self.state.dirty = true;
                let result = if request.directory() {
                    self.state
                        .ext4
                        .remove_empty_directory(self.source.number(), name)
                } else {
                    self.state.ext4.unlink(self.source.number(), name)
                };
                match result {
                    Ok(outcome) => {
                        self.reap = outcome
                            .requires_reap()
                            .then(|| self.state.publish_zero_link(outcome.inode))
                            .flatten();
                        NamespaceOutcome::applied(
                            NamespaceEffect::Unlink(authorized.target().selected()),
                            Ok(()),
                        )
                    }
                    Err(error) => self.failed(error),
                }
            }
            AuthorizedMutation::Rename(authorized) => {
                self.rename(authorized).map(NamespaceEffect::Rename)
            }
        }
    }
}

impl Ext4Attempt<'_> {
    fn rename(&mut self, authorized: &AuthorizedRename<'_>) -> NamespaceOutcome<RenameEffect> {
        self.state.dirty = true;
        let request = authorized.request();
        let options = request.options();
        let options = match (options.no_replace(), options.exchange(), options.whiteout()) {
            (false, false, false) => rsext4::RenameOptions::REPLACE,
            (true, false, false) => rsext4::RenameOptions::NO_REPLACE,
            (false, true, false) => rsext4::RenameOptions::EXCHANGE,
            (false, false, true) => rsext4::RenameOptions::WHITEOUT,
            (true, false, true) => rsext4::RenameOptions::WHITEOUT_NO_REPLACE,
            _ => unreachable!("validated rename options"),
        };
        let effect = authorized.effect();
        let result = self.state.ext4.rename(
            self.source.number(),
            FileName::new(request.source_name().as_bytes()).expect("validated name"),
            self.destination.number(),
            FileName::new(request.destination_name().as_bytes()).expect("validated name"),
            options,
        );
        match result {
            Ok(outcome) => {
                self.reap = outcome
                    .replaced
                    .filter(|effect| effect.requires_reap())
                    .and_then(|effect| self.state.publish_zero_link(effect.inode));
                NamespaceOutcome::applied(effect, Ok(()))
            }
            Err(error) => {
                self.error = Some(error);
                if error.requires_journal_progress() {
                    NamespaceOutcome::retry()
                } else {
                    NamespaceOutcome::uncertain(Some(effect), Err(into_vfs_err(error)))
                }
            }
        }
    }
}
