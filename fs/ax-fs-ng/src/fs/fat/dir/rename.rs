//! FAT rename observes, authorizes and publishes under one real filesystem guard.

use alloc::vec::Vec;

use axfs_ng_vfs::{
    AuthorizedRename, LockedAttempt, MutationRetire, NamespaceMutation, RenameNode,
    RenameObservation, RenameRequest,
};

use super::*;

struct FatAttempt<'a> {
    guard: FatFilesystemGuard<'a>,
    source: &'a FatDirNode,
    destination: &'a FatDirNode,
    source_holder: Option<Arc<FatInode>>,
}

impl FatDirNode {
    pub(super) fn run_namespace(
        &self,
        command: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<()> {
        let NamespaceMutation::Rename(request) = command;
        let destination = request.destination_parent().as_dir()?.downcast::<Self>()?;
        if !Arc::ptr_eq(&self.state.fs, &destination.state.fs) {
            return Err(VfsError::CrossesDevices);
        }
        if request.options().exchange() || request.options().whiteout() {
            return Err(VfsError::OperationNotSupported);
        }
        validate_rename_name(request.source_name())?;
        validate_rename_name(request.destination_name())?;
        let mut attempt = FatAttempt {
            guard: self.state.fs.lock_retiring(retired),
            source: self,
            destination: &destination,
            source_holder: None,
        };
        request.execute(&mut attempt, retired).into_result()
    }
}

impl FatAttempt<'_> {
    fn observe_node(
        &mut self,
        parent: &FatDirNode,
        name: &str,
        retired: &MutationRetire,
    ) -> VfsResult<(RenameNode, Arc<FatInode>)> {
        let raw = parent.find(name, &self.guard)?;
        let canonical_name = FatInode::canonical_name(&raw)?;
        let is_dir = raw.is_dir();
        let holder = FatInode::get(&parent.state, raw, &mut self.guard)?;
        let metadata = holder.metadata(&self.guard)?;
        let empty = if is_dir {
            let mut empty = true;
            for child in holder.directory(&self.guard)?.iter() {
                let child = child.map_err(into_vfs_err)?;
                if !matches!(child.file_name().as_str(), "." | "..") {
                    empty = false;
                    break;
                }
            }
            empty
        } else {
            false
        };
        let entry = parent.entry(holder.clone(), is_dir, &canonical_name);
        retired.retire(entry.clone());
        Ok((
            RenameNode {
                entry,
                metadata,
                canonical_name,
                cacheable: true,
                empty,
            },
            holder,
        ))
    }
}

impl LockedAttempt for FatAttempt<'_> {
    fn parents(&mut self) -> VfsResult<(Metadata, Metadata)> {
        if !self.source.state.name.borrow(&self.guard).trusted
            || !self.destination.state.name.borrow(&self.guard).trusted
        {
            return Err(VfsError::BadState);
        }
        Ok((
            self.source.state.metadata(&self.guard)?,
            self.destination.state.metadata(&self.guard)?,
        ))
    }

    fn observe(
        &mut self,
        request: &RenameRequest<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<RenameObservation> {
        let (source, holder) = self.observe_node(self.source, request.source_name(), retired)?;
        self.source_holder = Some(holder);
        let target = match self.observe_node(self.destination, request.destination_name(), retired)
        {
            Ok((target, _holder)) => Some(target),
            Err(VfsError::NotFound) => None,
            Err(error) => return Err(error),
        };
        Ok(RenameObservation { source, target })
    }

    fn is_ancestor(&mut self, ancestor: &NamespaceNode, directory: u64) -> VfsResult<bool> {
        let mut current = if self.source.state.inode == directory {
            self.source.state.clone()
        } else if self.destination.state.inode == directory {
            self.destination.state.clone()
        } else {
            return Err(VfsError::BadState);
        };
        let mut visited = Vec::new();
        loop {
            self.guard.retain(&current);
            if current.inode == ancestor.inode {
                return Ok(true);
            }
            if visited.contains(&current.inode) {
                return Err(VfsError::FilesystemLoop);
            }
            visited.push(current.inode);
            let parent = current.name.borrow(&self.guard).parent.clone();
            match parent {
                Some(parent) => current = parent,
                None => return Ok(false),
            }
        }
    }

    fn commit(
        &mut self,
        authorized: &AuthorizedRename<'_>,
        _retired: &MutationRetire,
    ) -> NamespaceOutcome<RenameEffect> {
        let request = authorized.request();
        if authorized.observation().target.is_some() {
            // No safe replacement of a different target/editor in starry-fatfs.
            return NamespaceOutcome::unchanged(Err(VfsError::OperationNotSupported));
        }
        let source = self.source_holder.as_ref().expect("observed FAT holder");
        if authorized.observation().source.entry.node_type() == NodeType::Directory
            && !Arc::ptr_eq(&self.source.state, &self.destination.state)
        {
            // The public fatfs API cannot update a moved directory's "..".
            return NamespaceOutcome::unchanged(Err(VfsError::OperationNotSupported));
        }
        let effect = authorized.effect();
        let parent = match self.source.state.directory(&self.guard) {
            Ok(parent) => parent,
            Err(error) => return NamespaceOutcome::unchanged(Err(error)),
        };
        let destination = match self.destination.state.directory(&self.guard) {
            Ok(parent) => parent,
            Err(error) => return NamespaceOutcome::unchanged(Err(error)),
        };
        if let Err(error) = source.close_handle(&mut self.guard) {
            return NamespaceOutcome::unchanged(Err(error));
        }
        if let Err(error) = parent.rename(
            request.source_name(),
            &destination,
            request.destination_name(),
        ) {
            source.invalidate_name(&mut self.guard);
            return NamespaceOutcome::uncertain(Some(effect), Err(into_vfs_err(error)));
        }
        match self
            .destination
            .find(request.destination_name(), &self.guard)
        {
            Ok(entry) => {
                source.move_handle(self.destination.state.clone(), entry, &mut self.guard);
                NamespaceOutcome::applied(effect, Ok(()))
            }
            Err(error) => {
                source.invalidate_name(&mut self.guard);
                NamespaceOutcome::applied(effect, Err(error))
            }
        }
    }
}
