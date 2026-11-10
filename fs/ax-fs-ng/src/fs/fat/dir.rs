use alloc::{borrow::ToOwned, sync::Arc};
use core::any::Any;

use axfs_ng_vfs::{
    DirEntry, DirEntrySink, DirNode, DirNodeOps, DirectoryCursor, FilesystemOps, Metadata,
    MetadataUpdate, NamespaceOutcome, NodeFlags, NodeIdentity, NodeOps, NodeType, Reference,
    RenameEffect, VfsError, VfsResult, WeakDirEntry,
};

use super::{ff, file::FatFileNode, fs::FatFilesystemGuard, state::FatInode, util::into_vfs_err};

mod rename;

pub struct FatDirNode {
    state: Arc<FatInode>,
    this: WeakDirEntry,
}

impl FatDirNode {
    pub(super) fn from_state(state: Arc<FatInode>, this: WeakDirEntry) -> DirNode {
        DirNode::new(Arc::new(Self { state, this }))
    }

    fn find(&self, name: &str, guard: &FatFilesystemGuard<'_>) -> VfsResult<ff::DirEntry<'static>> {
        for entry in self.state.directory(guard)?.iter() {
            let entry = entry.map_err(into_vfs_err)?;
            if entry.eq_name(name) {
                FatInode::canonical_name(&entry)?;
                return Ok(entry);
            }
        }
        Err(VfsError::NotFound)
    }

    fn entry(&self, state: Arc<FatInode>, is_dir: bool, name: &str) -> DirEntry {
        let reference = Reference::new(self.this.upgrade(), name.to_owned());
        if is_dir {
            DirEntry::new_dir(|this| Self::from_state(state, this), reference)
        } else {
            DirEntry::new_file(
                FatFileNode::from_state(state),
                NodeType::RegularFile,
                reference,
            )
        }
    }

    fn object(
        &self,
        entry: ff::DirEntry<'static>,
        guard: &mut FatFilesystemGuard<'_>,
    ) -> VfsResult<DirEntry> {
        let canonical = FatInode::canonical_name(&entry)?;
        let is_dir = entry.is_dir();
        let state = FatInode::get(&self.state, entry, guard)?;
        Ok(self.entry(state, is_dir, &canonical))
    }
}

fn validate_rename_name(name: &str) -> VfsResult<()> {
    // starry-fatfs validates this byte limit and character set in write_entry,
    // after rename_internal has already deleted the source slots. Validate the
    // complete component before touching a shared editor or namespace state.
    const MAX_FAT_NAME_BYTES: usize = 255;
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.len() > MAX_FAT_NAME_BYTES
        || !name.chars().all(|character| {
            matches!(
                character,
                'a'..='z' | 'A'..='Z' | '0'..='9' | '\u{80}'..='\u{ffff}'
                    | '$' | '%' | '\'' | '-' | '_' | '@' | '~' | '`' | '!' | '(' | ')'
                    | '{' | '}' | '.' | ' ' | '+' | ',' | ';' | '=' | '[' | ']'
                    | '^' | '#' | '&'
            )
        })
    {
        return Err(VfsError::InvalidInput);
    }
    Ok(())
}

impl NodeOps for FatDirNode {
    fn inode(&self) -> u64 {
        self.state.inode
    }
    fn identity(&self) -> Option<NodeIdentity> {
        Some(self.state.identity())
    }
    fn metadata(&self) -> VfsResult<Metadata> {
        self.state.metadata(&self.state.fs.lock())
    }
    fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
        Ok(())
    }
    fn filesystem(&self) -> &dyn FilesystemOps {
        &*self.state.fs
    }
    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        Ok(())
    }
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
    fn flags(&self) -> NodeFlags {
        NodeFlags::BLOCKING
    }
}

impl DirNodeOps for FatDirNode {
    fn namespace_domain(&self) -> usize {
        Arc::as_ptr(&self.state.fs) as usize
    }

    fn canonical_name(&self, _name: &str, entry: &DirEntry) -> alloc::string::String {
        entry.name()
    }

    fn read_dir(&self, cursor: DirectoryCursor, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let mut guard = self.state.fs.lock();
        let dir = self.state.directory(&guard)?;
        let mut count = 0;
        for entry in dir.iter().skip(cursor.offset() as usize) {
            let entry = entry.map_err(into_vfs_err)?;
            let name = FatInode::canonical_name(&entry)?;
            let node_type = if entry.is_dir() {
                NodeType::Directory
            } else {
                NodeType::RegularFile
            };
            let inode = match name.as_str() {
                "." => self.inode(),
                ".." => self
                    .state
                    .name
                    .borrow(&guard)
                    .parent
                    .as_ref()
                    .map_or(self.inode(), |parent| parent.inode),
                _ => FatInode::get(&self.state, entry, &mut guard)?.inode,
            };
            if !sink.accept(
                name.as_bytes(),
                inode,
                node_type,
                DirectoryCursor::new(cursor.offset() + count + 1),
            ) {
                break;
            }
            count += 1;
        }
        Ok(count as usize)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        match name {
            "." => return self.this.upgrade().ok_or(VfsError::NotFound),
            ".." => {
                return self
                    .this
                    .upgrade()
                    .and_then(|entry| entry.parent())
                    .ok_or(VfsError::NotFound);
            }
            _ => {}
        }
        #[cfg(all(test, feature = "host-test"))]
        if let Some(probe) = self
            .this
            .upgrade()
            .and_then(|entry| entry.as_dir().ok()?.unlink_probe())
        {
            probe.reader_entering_backend(self.state.fs.lookup_gate_is_held());
        }
        let mut guard = self.state.fs.lock();
        self.object(self.find(name, &guard)?, &mut guard)
    }

    fn prepare_unlink(&self) {
        crate::file::prune_cached_files();
    }

    fn mutate_namespace(
        &self,
        command: &axfs_ng_vfs::NamespaceMutation<'_>,
        retired: &axfs_ng_vfs::MutationRetire,
    ) -> NamespaceOutcome<axfs_ng_vfs::NamespaceEffect> {
        self.run_namespace(command, retired)
    }
}
