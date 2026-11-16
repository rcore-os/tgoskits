//! Merged observations borrowing the original operation and retirement owner.

use axfs_ng_vfs::{
    BackingQueryContext, DirEntry, DirectoryBatch, DirectoryCursor, DirectoryRecord, FileNode,
    Location, Metadata, NamespaceQuery, NamespaceRead, NamespaceReadResult, NodePermission,
    NodeType, Reference, VfsError, VfsResult,
};

use super::{
    DirentInfo, OPAQUE_MARKER_NAME, OverlayDir, OverlayFile, OverlayFs, OverlayInode,
    WHITEOUT_DEVICE,
};

pub(super) enum ReadFailure {
    Retry,
    Error(VfsError),
}

pub(super) type ReadResult<T> = Result<T, ReadFailure>;

impl From<VfsError> for ReadFailure {
    fn from(error: VfsError) -> Self {
        Self::Error(error)
    }
}

pub(super) fn snapshot<T>(result: ReadResult<T>) -> NamespaceReadResult<T> {
    match result {
        Ok(value) => NamespaceReadResult::Ready(Ok(value)),
        Err(ReadFailure::Retry) => NamespaceReadResult::Retry,
        Err(ReadFailure::Error(error)) => NamespaceReadResult::Ready(Err(error)),
    }
}

fn ready<T>(result: VfsResult<NamespaceReadResult<T>>) -> ReadResult<T> {
    match result? {
        NamespaceReadResult::Ready(value) => value.map_err(ReadFailure::Error),
        NamespaceReadResult::Retry => Err(ReadFailure::Retry),
    }
}

pub(super) struct LayerRead<'fs, 'op> {
    filesystem: &'fs OverlayFs,
    queries: BackingQueryContext<'op>,
}

impl<'fs, 'op> LayerRead<'fs, 'op> {
    pub(super) fn new(filesystem: &'fs OverlayFs, queries: BackingQueryContext<'op>) -> Self {
        Self {
            filesystem,
            queries,
        }
    }

    pub(super) fn metadata(&self, inode: &OverlayInode) -> ReadResult<Metadata> {
        let mut metadata = self.backing_metadata(&inode.current()?)?;
        metadata.inode = inode.number;
        metadata.device = self.filesystem.device;
        Ok(metadata)
    }

    pub(super) fn query(&self, query: NamespaceQuery<'_>) -> ReadResult<NamespaceRead> {
        match query {
            NamespaceQuery::Lookup { parent, name } => {
                let parent = parent.downcast::<OverlayDir>()?;
                match self.lookup(&parent, name) {
                    Ok(entry) => Ok(NamespaceRead::Entry(Some(entry))),
                    Err(ReadFailure::Error(VfsError::NotFound)) => Ok(NamespaceRead::Entry(None)),
                    Err(error) => Err(error),
                }
            }
            NamespaceQuery::Metadata(entry) => {
                let inode = self.filesystem.inode_for(entry)?;
                self.metadata(&inode).map(NamespaceRead::Metadata)
            }
            NamespaceQuery::Xattr { entry, name } => {
                let inode = self.filesystem.inode_for(entry)?;
                let current = inode.current()?;
                ready(self.queries.query(NamespaceQuery::Xattr {
                    entry: current.entry(),
                    name,
                }))
            }
            NamespaceQuery::Directory { entry, cursor } => {
                let directory = entry.downcast::<OverlayDir>()?;
                self.directory_batch(&directory, cursor)
                    .map(NamespaceRead::Directory)
            }
        }
    }

    pub(super) fn lookup(&self, parent: &OverlayDir, name: &str) -> ReadResult<DirEntry> {
        if name == OPAQUE_MARKER_NAME {
            return Err(VfsError::NotFound.into());
        }
        let upper_parent = parent.inode.upper();
        let upper = upper_parent
            .as_ref()
            .map(|dir| self.child(dir, name))
            .transpose()?
            .flatten();
        if upper
            .as_ref()
            .is_some_and(|(_, metadata)| Self::whiteout(metadata))
        {
            return Err(VfsError::NotFound.into());
        }
        let merge_lower = upper.as_ref().is_none_or(|(entry, _)| entry.is_dir())
            && match &upper_parent {
                Some(dir) => !self.opaque(dir)?,
                None => true,
            }
            && match &upper {
                Some((entry, _)) if entry.is_dir() => !self.opaque(entry)?,
                _ => true,
            };
        let lower = if merge_lower {
            self.lower_children(&parent.inode.lower, name, upper.is_some())?
        } else {
            alloc::vec::Vec::new()
        };
        let metadata = upper
            .as_ref()
            .map(|(_, metadata)| metadata)
            .or_else(|| lower.first().map(|(_, metadata)| metadata))
            .ok_or(VfsError::NotFound)?;
        let inode = self.filesystem.inodes.select(
            upper.as_ref().map(|(entry, _)| entry.clone()),
            lower.iter().map(|(entry, _)| entry.clone()).collect(),
            self.filesystem.upper_dir.is_some(),
            metadata,
            self.queries.retired(),
        )?;
        let parent = parent.this.upgrade().ok_or(VfsError::NotFound)?;
        let reference = Reference::new(Some(parent), name.into());
        if inode.kind == NodeType::Directory {
            Ok(OverlayDir::entry(
                self.filesystem.this.upgrade().ok_or(VfsError::BadState)?,
                inode,
                reference,
            ))
        } else {
            let file = alloc::sync::Arc::new(OverlayFile {
                fs: self.filesystem.this.upgrade().ok_or(VfsError::BadState)?,
                inode,
                this: crate::sync::RawSpinLock::new(None),
            });
            let entry = DirEntry::new_file(FileNode::new(file.clone()), file.inode.kind, reference);
            *file.this.lock_irqsave() = Some(entry.downgrade());
            Ok(entry)
        }
    }

    pub(super) fn names(
        &self,
        directory: &OverlayDir,
    ) -> ReadResult<alloc::collections::BTreeMap<alloc::string::String, DirentInfo>> {
        let mut names = alloc::collections::BTreeMap::new();
        let upper = directory.inode.upper();
        if !match &upper {
            Some(upper) => self.opaque(upper)?,
            None => false,
        } {
            let mut lowers = alloc::vec::Vec::new();
            for lower in &directory.inode.lower {
                lowers.push(lower);
                if self.opaque(lower)? {
                    break;
                }
            }
            for lower in lowers.into_iter().rev() {
                self.read_names(lower, &mut names)?;
            }
        }
        if let Some(upper) = upper {
            self.read_names(&upper, &mut names)?;
        }
        Ok(names)
    }

    fn directory_batch(
        &self,
        directory: &OverlayDir,
        cursor: DirectoryCursor,
    ) -> ReadResult<DirectoryBatch> {
        let offset = usize::try_from(cursor.offset()).map_err(|_| VfsError::InvalidInput)?;
        let mut records = alloc::vec::Vec::new();
        records
            .try_reserve_exact(DirectoryBatch::MAX_ENTRIES)
            .map_err(|_| VfsError::NoMemory)?;
        for (index, (name, entry)) in self
            .names(directory)?
            .into_iter()
            .enumerate()
            .skip(offset)
            .take(DirectoryBatch::MAX_ENTRIES)
        {
            records.push(DirectoryRecord {
                name: name.into_bytes(),
                inode: entry.ino,
                node_type: entry.node_type,
                next: DirectoryCursor::new(index as u64 + 1),
            });
        }
        DirectoryBatch::new(records).map_err(ReadFailure::Error)
    }

    fn read_names(
        &self,
        directory: &Location,
        names: &mut alloc::collections::BTreeMap<alloc::string::String, DirentInfo>,
    ) -> ReadResult<()> {
        self.visit_records(directory, |record| {
            let Ok(name) = core::str::from_utf8(&record.name) else {
                return Ok(());
            };
            if matches!(name, "." | "..") || name == OPAQUE_MARKER_NAME {
                return Ok(());
            }
            if record.node_type == NodeType::CharacterDevice
                && self
                    .child_snapshot(directory, name)?
                    .as_ref()
                    .is_some_and(|(_, metadata)| Self::whiteout(metadata))
            {
                names.remove(name);
            } else {
                names.insert(
                    name.into(),
                    DirentInfo {
                        ino: record.inode,
                        node_type: record.node_type,
                    },
                );
            }
            Ok(())
        })
    }

    pub(super) fn raw_names(
        &self,
        directory: &Location,
    ) -> ReadResult<alloc::vec::Vec<alloc::string::String>> {
        let mut names = alloc::vec::Vec::new();
        self.visit_records(directory, |record| {
            let name = core::str::from_utf8(&record.name).map_err(|_| VfsError::InvalidData)?;
            if !matches!(name, "." | "..") {
                names.push(name.into());
            }
            Ok(())
        })?;
        Ok(names)
    }

    fn visit_records(
        &self,
        directory: &Location,
        mut visit: impl FnMut(&DirectoryRecord) -> ReadResult<()>,
    ) -> ReadResult<()> {
        self.filesystem.credentials().check_permission(
            &self.backing_metadata(directory)?,
            NodePermission::OTHER_READ,
        )?;
        let mut cursor = DirectoryCursor::START;
        let mut visited = alloc::collections::BTreeSet::new();
        loop {
            if !visited.insert((
                cursor.offset(),
                cursor.continuation(),
                cursor.observed_change_attribute(),
            )) {
                return Err(VfsError::BadState.into());
            }
            let NamespaceRead::Directory(batch) =
                ready(self.queries.query(NamespaceQuery::Directory {
                    entry: directory.entry(),
                    cursor,
                }))?
            else {
                return Err(VfsError::BadState.into());
            };
            if batch.records().is_empty() {
                return Ok(());
            }
            for record in batch.records() {
                cursor = record.next;
                visit(record)?;
            }
        }
    }

    fn backing_metadata(&self, location: &Location) -> ReadResult<Metadata> {
        match ready(
            self.queries
                .query(NamespaceQuery::Metadata(location.entry())),
        )? {
            NamespaceRead::Metadata(metadata) => Ok(metadata),
            _ => Err(VfsError::BadState.into()),
        }
    }

    fn child(&self, parent: &Location, name: &str) -> ReadResult<Option<(Location, Metadata)>> {
        self.filesystem
            .credentials()
            .check_permission(&self.backing_metadata(parent)?, NodePermission::OTHER_EXEC)?;
        self.child_snapshot(parent, name)
    }

    pub(super) fn child_snapshot(
        &self,
        parent: &Location,
        name: &str,
    ) -> ReadResult<Option<(Location, Metadata)>> {
        let entry = match ready(self.queries.lookup_backing(parent, name)) {
            Ok(entry) => entry,
            Err(ReadFailure::Error(VfsError::NotFound)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let metadata = self.backing_metadata(&entry)?;
        Ok(Some((entry, metadata)))
    }

    pub(super) fn opaque(&self, directory: &Location) -> ReadResult<bool> {
        Ok(self
            .child_snapshot(directory, OPAQUE_MARKER_NAME)?
            .is_some())
    }

    pub(super) fn lower_children(
        &self,
        parents: &[Location],
        name: &str,
        directory: bool,
    ) -> ReadResult<alloc::vec::Vec<(Location, Metadata)>> {
        let mut result: alloc::vec::Vec<(Location, Metadata)> = alloc::vec::Vec::new();
        for parent in parents {
            if let Some((entry, metadata)) = self.child(parent, name)? {
                if Self::whiteout(&metadata) {
                    break;
                }
                if !entry.is_dir() {
                    if result.is_empty() && !directory {
                        result.push((entry, metadata));
                    }
                    break;
                }
                let opaque = self.opaque(&entry)?;
                result.push((entry, metadata));
                if opaque {
                    break;
                }
            }
            if self.opaque(parent)? {
                break;
            }
        }
        Ok(result)
    }

    fn whiteout(metadata: &Metadata) -> bool {
        metadata.node_type == NodeType::CharacterDevice && metadata.rdev == WHITEOUT_DEVICE
    }
}

impl OverlayFs {
    pub(super) fn read<T>(
        &self,
        mut operation: impl FnMut(&LayerRead<'_, '_>) -> ReadResult<T>,
    ) -> VfsResult<T> {
        axfs_ng_vfs::PreparedRead::acquire(self)?.run(|queries| {
            let _owner = self.namespace.lock();
            Ok(snapshot(operation(&LayerRead::new(self, queries))))
        })
    }

    pub(super) fn read_backing(
        &self,
        request: axfs_ng_vfs::BackingQueryRequest<'_, '_>,
    ) -> axfs_ng_vfs::NamespaceCompletion<NamespaceRead> {
        use axfs_ng_vfs::{NamespaceCompletion, NamespaceOutcome};
        if let Err(error) = request.admission().check_owner(self) {
            return NamespaceCompletion::new(NamespaceOutcome::unchanged(Err(error)), None);
        }
        let result = {
            let _owner = self.namespace.lock();
            LayerRead::new(self, request.backing_queries()).query(*request.query())
        };
        let outcome = match result {
            Ok(value) => NamespaceOutcome::unchanged_observed(value, Ok(())),
            Err(ReadFailure::Retry) => NamespaceOutcome::retry(),
            Err(ReadFailure::Error(error)) => NamespaceOutcome::unchanged(Err(error)),
        };
        NamespaceCompletion::new(outcome, None)
    }
}
