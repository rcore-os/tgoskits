//! Directory mutation, lookup, and per-open directory-reader adaptation.

use alloc::{borrow::ToOwned, boxed::Box, sync::Arc};

use axfs_ng_vfs::{
    DirEntry, DirEntrySink, DirNode, DirNodeOps, DirectoryCursor as VfsDirectoryCursor,
    DirectoryReadState, FileExtentMap as VfsFileExtentMap, FileExtentTarget as VfsFileExtentTarget,
    FileNode, Reference, VfsError, VfsResult, WeakDirEntry,
};
use rsext4::{FileName, InodeFlags};

use super::{super::util::directory_entry_type_to_vfs, Inode, LocatedInode, into_vfs_err};

mod cursor;
use cursor::{
    core_to_vfs_directory_cursor, normalize_directory_cursor, vfs_to_core_directory_cursor,
};

const DIRECTORY_READ_BATCH_ENTRIES: usize = 128;

struct Ext4DirectoryReadState {
    reader: rsext4::DirectoryReader,
}

impl DirNodeOps for Inode {
    fn namespace_domain(&self) -> usize {
        Arc::as_ptr(self.fs()) as usize
    }

    fn negative_cache_generation(&self) -> VfsResult<Option<u64>> {
        // This mount owns exact-byte names and namespace mutation. Unsupported
        // casefold/encryption features are rejected by the core mount checks.
        self.fs().directory_cache_generation(self.number())
    }

    fn map_extents(
        &self,
        offset: u64,
        len: u64,
        target: VfsFileExtentTarget,
        extent_limit: usize,
    ) -> VfsResult<VfsFileExtentMap> {
        self.inspect_extents(offset, len, target, extent_limit)
    }

    fn read_dir(
        &self,
        cursor: VfsDirectoryCursor,
        sink: &mut dyn DirEntrySink,
    ) -> VfsResult<usize> {
        let mut reader = self
            .fs()
            .lock()
            .ext4
            .open_directory_reader(self.number())
            .map_err(into_vfs_err)?;
        self.read_dir_with_core_reader(&mut reader, cursor, sink)
    }

    fn open_directory_read_state(&self) -> VfsResult<Box<dyn DirectoryReadState>> {
        let reader = self
            .fs()
            .lock()
            .ext4
            .open_directory_reader(self.number())
            .map_err(into_vfs_err)?;
        Ok(Box::new(Ext4DirectoryReadState { reader }))
    }

    fn read_dir_with_state(
        &self,
        state: &mut dyn DirectoryReadState,
        cursor: VfsDirectoryCursor,
        sink: &mut dyn DirEntrySink,
    ) -> VfsResult<usize> {
        let state = state
            .as_any_mut()
            .downcast_mut::<Ext4DirectoryReadState>()
            .ok_or(VfsError::InvalidInput)?;
        if state.reader.directory() != self.number() {
            return Err(VfsError::InvalidInput);
        }
        self.read_dir_with_core_reader(&mut state.reader, cursor, sink)
    }

    fn directory_end_cursor(&self) -> VfsResult<VfsDirectoryCursor> {
        let cursor = self
            .fs()
            .lock()
            .ext4
            .directory_end_cursor(self.number())
            .map_err(into_vfs_err)?;
        Ok(core_to_vfs_directory_cursor(cursor, None))
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        if name == "." {
            return self
                .this
                .as_ref()
                .and_then(WeakDirEntry::upgrade)
                .ok_or(VfsError::NotFound);
        }
        if name == ".." {
            return self
                .this
                .as_ref()
                .and_then(WeakDirEntry::upgrade)
                .and_then(|entry| entry.parent())
                .ok_or(VfsError::NotFound);
        }
        self.lookup_entry(name)
    }

    fn prepare_unlink(&self) {
        let has_pending_reaps = self.fs().lock().has_pending_reaps();
        if has_pending_reaps {
            // Cache retirement can drop inodes and reenter admission/reap, so
            // neither filesystem state nor native admission may be held here.
            crate::file::prune_cached_files();
        }
    }

    fn mutate_namespace(
        &self,
        command: &axfs_ng_vfs::NamespaceMutation<'_>,
        retired: &axfs_ng_vfs::MutationRetire,
    ) -> axfs_ng_vfs::NamespaceOutcome<axfs_ng_vfs::NamespaceEffect> {
        self.fs().run_namespace(command, retired)
    }
}

impl Inode {
    pub(crate) fn create_entry(&self, located: LocatedInode, name: &str) -> DirEntry {
        let name = name.to_owned();
        let reference = Reference::new(
            self.this.as_ref().and_then(WeakDirEntry::upgrade),
            name.clone(),
        );
        if located.file_type == rsext4::DirectoryEntryType::Directory {
            DirEntry::new_dir(
                |this| DirNode::new(Inode::new(located.lifetime, Some(this))),
                reference,
            )
        } else {
            DirEntry::new_file(
                FileNode::new(Inode::new(located.lifetime, None)),
                directory_entry_type_to_vfs(located.file_type),
                reference,
            )
        }
    }

    fn lookup_entry(&self, name: &str) -> VfsResult<DirEntry> {
        let raw_name = FileName::new(name.as_bytes()).map_err(into_vfs_err)?;
        let located = self.lifetime.lookup(raw_name)?.ok_or(VfsError::NotFound)?;
        Ok(self.create_entry(located, name))
    }

    fn read_dir_with_core_reader(
        &self,
        reader: &mut rsext4::DirectoryReader,
        cursor: VfsDirectoryCursor,
        sink: &mut dyn DirEntrySink,
    ) -> VfsResult<usize> {
        let mut next_cursor = cursor;
        let mut count = 0usize;
        loop {
            let (entries, change_attribute) = {
                let mut state = self.fs().lock();
                let inode = state.ext4.inode(self.number()).map_err(into_vfs_err)?;
                next_cursor = normalize_directory_cursor(next_cursor, inode.change_attribute);
                let core_cursor = vfs_to_core_directory_cursor(
                    next_cursor,
                    inode.flags.contains(InodeFlags::DIRECTORY_INDEX),
                )?;
                let entries = state
                    .ext4
                    .read_directory_with_reader(reader, core_cursor, DIRECTORY_READ_BATCH_ENTRIES)
                    .map_err(into_vfs_err)?;
                (entries, inode.change_attribute)
            };
            if entries.is_empty() {
                return Ok(count);
            }
            for entry in entries {
                next_cursor =
                    core_to_vfs_directory_cursor(entry.next_cursor, Some(change_attribute));
                if !sink.accept(
                    &entry.name,
                    entry.inode.as_u64(),
                    directory_entry_type_to_vfs(entry.file_type),
                    next_cursor,
                ) {
                    return Ok(count);
                }
                count += 1;
            }
        }
    }
}
