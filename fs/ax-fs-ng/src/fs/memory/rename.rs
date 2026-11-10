//! A memory filesystem's gate-bearing rename attempt.

use axfs_ng_vfs::{
    AuthorizedRename, LockedAttempt, MutationRetire, NamespaceMutation, NamespaceNode,
    NamespaceOutcome, RenameNode, RenameObservation, RenameRequest,
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
    ) -> VfsResult<()> {
        let NamespaceMutation::Rename(request) = command;
        let destination = request.destination_parent().as_dir()?.downcast::<Self>()?;
        if !Arc::ptr_eq(&self.fs, &destination.fs) {
            return Err(VfsError::CrossesDevices);
        }
        if request.options().exchange() || request.options().whiteout() {
            return Err(VfsError::OperationNotSupported);
        }
        let mut attempt = MemoryAttempt {
            _gate: self.fs.mutation.lock(),
            source: self,
            destination: &destination,
        };
        request.execute(&mut attempt, retired).into_result()
    }
}

impl MemoryAttempt<'_> {
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
        authorized: &AuthorizedRename<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<axfs_ng_vfs::RenameEffect> {
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
