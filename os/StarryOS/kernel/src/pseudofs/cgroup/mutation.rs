use alloc::sync::Arc;
use core::time::Duration;

use ax_cgroup::{
    CgroupFileKind, CgroupFsCreate, CgroupFsInit, CgroupFsTarget, CgroupMutationGuard,
};
use ax_runtime::hal::time::wall_time;
use axfs_ng_vfs::{
    AuthorizedCreate, AuthorizedMutation, AuthorizedUnlink, CreateContent, CreateEffect,
    CreateOwnerPolicy, DirEntry, LockedAttempt, Metadata, MutationObservation, MutationRetire,
    NamespaceEffect, NamespaceEntry, NamespaceMutation, NamespaceNode, NamespaceOutcome, NodeType,
    RenameNode, VfsError, VfsResult,
};

use super::{CgroupDir, cgroup_error_to_vfs_error, metadata_from_attributes, path_error};

impl CgroupDir {
    pub(super) fn mutate_names(
        &self,
        command: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        if matches!(
            command,
            NamespaceMutation::Link(_) | NamespaceMutation::Rename(_)
        ) {
            return NamespaceOutcome::unchanged(Err(VfsError::OperationNotPermitted));
        }
        let (parent, _) = command.parents();
        if !parent
            .downcast::<Self>()
            .is_ok_and(|owner| core::ptr::eq(owner.as_ref(), self))
        {
            return NamespaceOutcome::unchanged(Err(VfsError::BadState));
        }
        let now = wall_time();
        let mut attempt = CgroupAttempt {
            directory: self,
            parent,
            now,
            guard: self.cgroup.mutation_domain().lock(),
        };
        command.execute(&mut attempt, retired)
    }
}

struct CgroupAttempt<'a> {
    directory: &'a CgroupDir,
    parent: &'a DirEntry,
    now: Duration,
    guard: CgroupMutationGuard<'a>,
}

impl CgroupAttempt<'_> {
    fn observe_name(&self, name: &str) -> VfsResult<RenameNode> {
        let cgroup = &self.directory.cgroup;
        let (entry, metadata, empty) = if let Some(kind) = CgroupFileKind::from_name(name) {
            let target = CgroupFsTarget::File(kind);
            let attributes = self
                .guard
                .fs_attributes(cgroup, target)
                .map_err(cgroup_error_to_vfs_error)?;
            (
                self.directory.file_entry(self.parent, kind),
                metadata_from_attributes(target, attributes),
                false,
            )
        } else {
            let child = self.guard.lookup_child(cgroup, name).map_err(path_error)?;
            let target = CgroupFsTarget::Directory;
            let attributes = self
                .guard
                .fs_attributes(&child, target)
                .map_err(cgroup_error_to_vfs_error)?;
            let empty = !child.has_children();
            (
                self.directory.child_dir_entry(self.parent, name, child),
                metadata_from_attributes(target, attributes),
                empty,
            )
        };
        Ok(RenameNode {
            entry,
            metadata,
            canonical_name: name.into(),
            cacheable: false,
            empty,
        })
    }

    fn create(&self, authorized: &AuthorizedCreate<'_>) -> VfsResult<NamespaceEffect> {
        let request = authorized.request();
        let name = request.name();
        let content = request.attributes().content;
        if matches!(content, CreateContent::Node(..)) && CgroupFileKind::from_name(name).is_some() {
            return Err(VfsError::AlreadyExists);
        }
        if !matches!(content, CreateContent::Node(NodeType::Directory, _)) {
            return Err(VfsError::OperationNotPermitted);
        }
        let (uid, gid) = authorized.owner();
        let child = self
            .guard
            .create_child(
                &self.directory.cgroup,
                name,
                CgroupFsCreate {
                    directory: CgroupFsInit {
                        uid,
                        gid,
                        mode: authorized.permission().bits(),
                        now: self.now,
                    },
                    creator: authorized.creator_owner(),
                },
            )
            .map_err(cgroup_error_to_vfs_error)?;
        Ok(NamespaceEffect::Create(CreateEffect {
            selected: NamespaceEntry {
                entry: self.directory.child_dir_entry(self.parent, name, child),
                canonical_name: name.into(),
                cacheable: false,
            },
            created: true,
        }))
    }

    fn unlink(
        &self,
        authorized: &AuthorizedUnlink<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<NamespaceEffect> {
        let name = authorized.request().name();
        if CgroupFileKind::from_name(name).is_some() {
            return Err(VfsError::OperationNotPermitted);
        }
        let target = authorized.target();
        let cgroup = &self.directory.cgroup;
        let current = self.guard.lookup_child(cgroup, name).map_err(path_error)?;
        let selected = target.entry.downcast::<CgroupDir>()?;
        if target.canonical_name != name || !Arc::ptr_eq(&current, &selected.cgroup) {
            return Err(VfsError::BadState);
        }
        let removed = self
            .guard
            .remove_child(cgroup, name, self.now)
            .map_err(cgroup_error_to_vfs_error)?;
        retired.retire(removed);
        Ok(NamespaceEffect::Unlink(target.selected()))
    }
}

impl LockedAttempt for CgroupAttempt<'_> {
    fn parents(&mut self) -> VfsResult<(Metadata, Metadata)> {
        let cgroup = &self.directory.cgroup;
        // A dead parent fails at path/may_create observation, before parent DAC.
        self.guard.check_live(cgroup).map_err(path_error)?;
        let target = CgroupFsTarget::Directory;
        let attributes = self
            .guard
            .fs_attributes(cgroup, target)
            .map_err(cgroup_error_to_vfs_error)?;
        let metadata = metadata_from_attributes(target, attributes);
        Ok((metadata.clone(), metadata))
    }

    fn observe(
        &mut self,
        request: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<MutationObservation> {
        let name = match request {
            NamespaceMutation::Create(request) => request.name(),
            NamespaceMutation::Unlink(request) => request.name(),
            NamespaceMutation::Link(_) | NamespaceMutation::Rename(_) => {
                return Err(VfsError::OperationNotPermitted);
            }
        };
        let current = match self.observe_name(name) {
            Ok(current) => {
                retired.retire(current.entry.clone());
                Some(current)
            }
            Err(VfsError::NotFound) => None,
            Err(error) => return Err(error),
        };
        match request {
            NamespaceMutation::Create(_) => Ok(MutationObservation::Create {
                target: current,
                owner_policy: CreateOwnerPolicy::Kernfs {
                    parent_has_explicit_attrs: self
                        .guard
                        .directory_has_explicit_attrs(&self.directory.cgroup)
                        .map_err(cgroup_error_to_vfs_error)?,
                },
            }),
            NamespaceMutation::Unlink(_) => current
                .map(MutationObservation::Unlink)
                .ok_or(VfsError::NotFound),
            _ => Err(VfsError::OperationNotPermitted),
        }
    }

    fn is_ancestor(&mut self, _ancestor: &NamespaceNode, _directory: u64) -> VfsResult<bool> {
        Err(VfsError::OperationNotPermitted)
    }

    fn commit(
        &mut self,
        authorized: &AuthorizedMutation<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        let result = match authorized {
            AuthorizedMutation::Create(authorized) => self.create(authorized),
            AuthorizedMutation::Unlink(authorized) => self.unlink(authorized, retired),
            AuthorizedMutation::Link(_) | AuthorizedMutation::Rename(_) => {
                Err(VfsError::OperationNotPermitted)
            }
        };
        match result {
            Ok(effect) => NamespaceOutcome::applied(effect, Ok(())),
            Err(error) => NamespaceOutcome::unchanged(Err(error)),
        }
    }
}
