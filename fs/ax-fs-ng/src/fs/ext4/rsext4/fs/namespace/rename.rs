//! Ext4 keeps admission across retries, but authorizes and publishes per attempt.

use alloc::{sync::Arc, vec::Vec};

use axfs_ng_vfs::{
    AuthorizedRename, LockedAttempt, Metadata, MutationRetire, NamespaceMutation, NamespaceNode,
    NamespaceOutcome, NamespaceState, NodeOps, NodeType, RenameNode, RenameObservation,
    RenameRequest, VfsError, VfsResult,
};
use rsext4::{Ext4Error, Ext4Result, FileName, InodeNumber};

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
    ) -> VfsResult<()> {
        let NamespaceMutation::Rename(request) = command;
        let source = request.source_parent().as_dir()?.downcast::<Inode>()?;
        let destination = request.destination_parent().as_dir()?.downcast::<Inode>()?;
        if !Arc::ptr_eq(self, source.fs()) || !Arc::ptr_eq(self, destination.fs()) {
            return Err(VfsError::CrossesDevices);
        }
        FileName::new(request.source_name().as_bytes()).map_err(into_vfs_err)?;
        FileName::new(request.destination_name().as_bytes()).map_err(into_vfs_err)?;
        if self.lock().has_pending_reaps() {
            crate::file::prune_cached_files();
        }
        let reap = {
            let _admission = self.admission.enter().map_err(into_vfs_err)?;
            loop {
                let completed = {
                    let namespace = self
                        .namespace
                        .change(source.number(), NamespaceChange::Topology)?;
                    let mut state = self.lock();
                    if state.staging {
                        None
                    } else {
                        state.dirty = true;
                        let mut attempt = Ext4Attempt {
                            state,
                            _namespace: namespace,
                            filesystem: self,
                            source: &source,
                            destination: &destination,
                            error: None,
                            reap: None,
                        };
                        let outcome = request.execute(&mut attempt, retired);
                        Some((outcome, attempt.error.take(), attempt.reap.take()))
                    }
                };
                let Some((outcome, error, reap)) = completed else {
                    self.finish_mutation_attempt::<()>(MutationAttempt::Staging)
                        .map_err(into_vfs_err)?;
                    continue;
                };
                // The attempt and all VFS publication guards have ended before
                // journal progress, abort persistence, or retry authorization.
                if let Some(error) = error {
                    if error.requires_journal_progress()
                        && matches!(
                            outcome.state(),
                            NamespaceState::Retry | NamespaceState::Unchanged
                        )
                    {
                        self.finish_mutation_attempt::<()>(MutationAttempt::Finished(Err(error)))
                            .map_err(into_vfs_err)?;
                        continue;
                    }
                    self.finish_mutation_attempt::<()>(MutationAttempt::Finished(Err(error)))
                        .map_err(into_vfs_err)?;
                }
                outcome.into_result()?;
                break reap;
            }
        };
        // Do not recursively enter operation admission while retaining its
        // outer guard: a concurrent lifecycle pause may be draining that guard.
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
    }
}

impl Ext4Attempt<'_> {
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
        request: &RenameRequest<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<RenameObservation> {
        let source = self.observe_node(self.source, request.source_name(), retired)?;
        let target = match self.observe_node(self.destination, request.destination_name(), retired)
        {
            Ok(target) => Some(target),
            Err(VfsError::NotFound) => None,
            Err(error) => return Err(error),
        };
        Ok(RenameObservation { source, target })
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
        authorized: &AuthorizedRename<'_>,
        _retired: &MutationRetire,
    ) -> NamespaceOutcome<axfs_ng_vfs::RenameEffect> {
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
