//! Directory mutation, lookup, and per-open directory-reader adaptation.

use alloc::{borrow::ToOwned, boxed::Box, sync::Arc};

use axfs_ng_vfs::{
    DirEntry, DirEntrySink, DirNode, DirNodeOps, DirectoryCursor as VfsDirectoryCursor,
    DirectoryReadState, FileExtentMap as VfsFileExtentMap, FileExtentTarget as VfsFileExtentTarget,
    FileNode, NodeOps, NodePermission, NodeType, Reference, RenameOptions as VfsRenameOptions,
    VfsError, VfsResult, WeakDirEntry,
};
use rsext4::{
    DeviceNumber, FileName, FilePermissions, InodeFlags, MutationContext, SpecialInodeKind,
};

use super::{
    super::{fs::NamespaceChange, util::directory_entry_type_to_vfs},
    Inode, LocatedInode, into_vfs_err,
};

mod cursor;
use cursor::{
    core_to_vfs_directory_cursor, normalize_directory_cursor, vfs_to_core_directory_cursor,
};

const DIRECTORY_READ_BATCH_ENTRIES: usize = 128;

struct Ext4DirectoryReadState {
    reader: rsext4::DirectoryReader,
}

impl DirNodeOps for Inode {
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

    fn create(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<DirEntry> {
        let raw_name = FileName::new(name.as_bytes()).map_err(into_vfs_err)?;
        let permissions = FilePermissions::new(permission.bits()).map_err(into_vfs_err)?;
        let context = Self::mutation_context(uid, gid);
        match node_type {
            NodeType::Symlink => return Err(VfsError::InvalidInput),
            NodeType::Unknown => return Err(VfsError::InvalidData),
            _ => {}
        }
        let info = self
            .lifetime
            .mutate_namespace(NamespaceChange::Directory, |state| {
                let info = match node_type {
                    NodeType::RegularFile => state.ext4.create_regular_file(
                        context,
                        self.number(),
                        raw_name,
                        permissions,
                    ),
                    NodeType::Directory => {
                        state
                            .ext4
                            .create_directory(context, self.number(), raw_name, permissions)
                    }
                    NodeType::Symlink => return Err(rsext4::Ext4Error::invalid_input()),
                    NodeType::CharacterDevice => state.ext4.create_special_inode(
                        context,
                        self.number(),
                        raw_name,
                        permissions,
                        SpecialInodeKind::CharacterDevice(DeviceNumber::ZERO),
                    ),
                    NodeType::BlockDevice => state.ext4.create_special_inode(
                        context,
                        self.number(),
                        raw_name,
                        permissions,
                        SpecialInodeKind::BlockDevice(DeviceNumber::ZERO),
                    ),
                    NodeType::Fifo => state.ext4.create_special_inode(
                        context,
                        self.number(),
                        raw_name,
                        permissions,
                        SpecialInodeKind::Fifo,
                    ),
                    NodeType::Socket => state.ext4.create_special_inode(
                        context,
                        self.number(),
                        raw_name,
                        permissions,
                        SpecialInodeKind::Socket,
                    ),
                    NodeType::Unknown => return Err(rsext4::Ext4Error::invalid_input()),
                }?;
                Ok(LocatedInode {
                    lifetime: state.retain_inode(self.fs(), info.number),
                    file_type: info.file_type(),
                })
            })?;

        let entry = self.create_entry(info, name);
        self.finish_directory_change()?;
        Ok(entry)
    }

    fn create_symlink(
        &self,
        name: &str,
        target: &str,
        _permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<DirEntry> {
        let raw_name = FileName::new(name.as_bytes()).map_err(into_vfs_err)?;
        let info = self
            .lifetime
            .mutate_namespace(NamespaceChange::Directory, |state| {
                let info = state.ext4.create_symlink(
                    Self::mutation_context(uid, gid),
                    self.number(),
                    raw_name,
                    target.as_bytes(),
                )?;
                Ok(LocatedInode {
                    lifetime: state.retain_inode(self.fs(), info.number),
                    file_type: info.file_type(),
                })
            })?;

        let entry = self.create_entry(info, name);
        self.finish_directory_change()?;
        Ok(entry)
    }

    fn link(&self, name: &str, node: &DirEntry) -> VfsResult<DirEntry> {
        let target: Arc<Self> = node.downcast().map_err(|_| VfsError::InvalidInput)?;
        if !Arc::ptr_eq(self.fs(), target.fs()) {
            return Err(VfsError::CrossesDevices);
        }
        let raw_name = FileName::new(name.as_bytes()).map_err(into_vfs_err)?;
        let info = self
            .lifetime
            .mutate_namespace(NamespaceChange::Directory, |state| {
                let info = state
                    .ext4
                    .hard_link(target.number(), self.number(), raw_name)?;
                Ok(LocatedInode {
                    lifetime: state.retain_inode(self.fs(), info.number),
                    file_type: info.file_type(),
                })
            })?;
        let entry = self.create_entry(info, name);
        self.finish_directory_change()?;
        Ok(entry)
    }

    fn unlink(&self, name: &str, is_dir: bool) -> VfsResult<()> {
        let raw_name = FileName::new(name.as_bytes()).map_err(into_vfs_err)?;
        let scope = if is_dir {
            NamespaceChange::Topology
        } else {
            NamespaceChange::Directory
        };
        let reap_claim = self.lifetime.mutate_namespace(scope, |state| {
            let info = state
                .ext4
                .lookup_child(self.number(), raw_name)?
                .ok_or_else(rsext4::Ext4Error::not_found)?;
            let target_is_dir = info.is_directory();
            match (target_is_dir, is_dir) {
                (true, false) => return Err(rsext4::Ext4Error::is_dir()),
                (false, true) => return Err(rsext4::Ext4Error::not_dir()),
                _ => {}
            }
            let outcome = if target_is_dir {
                state.ext4.remove_empty_directory(self.number(), raw_name)
            } else {
                state.ext4.unlink(self.number(), raw_name)
            }?;
            if outcome.requires_reap() {
                Ok(state.publish_zero_link(outcome.inode))
            } else {
                Ok(None)
            }
        })?;
        if let Some(claim) = reap_claim {
            self.fs().reap(claim)?;
        }
        self.finish_directory_change()
    }

    fn rename(
        &self,
        src_name: &str,
        dst_dir: &DirNode,
        dst_name: &str,
        options: VfsRenameOptions,
    ) -> VfsResult<()> {
        let dst_dir: Arc<Self> = dst_dir.downcast().map_err(|_| VfsError::InvalidInput)?;
        if !Arc::ptr_eq(self.fs(), dst_dir.fs()) {
            return Err(VfsError::CrossesDevices);
        }
        let src_name = FileName::new(src_name.as_bytes()).map_err(into_vfs_err)?;
        let dst_name = FileName::new(dst_name.as_bytes()).map_err(into_vfs_err)?;
        let core_options = match (options.no_replace(), options.exchange(), options.whiteout()) {
            (false, false, false) => rsext4::RenameOptions::REPLACE,
            (true, false, false) => rsext4::RenameOptions::NO_REPLACE,
            (false, true, false) => rsext4::RenameOptions::EXCHANGE,
            (false, false, true) => rsext4::RenameOptions::WHITEOUT,
            (true, false, true) => rsext4::RenameOptions::WHITEOUT_NO_REPLACE,
            _ => return Err(VfsError::InvalidInput),
        };
        let reap_claim = self
            .lifetime
            .mutate_namespace(NamespaceChange::Topology, |state| {
                state.ensure_linked_parent(dst_dir.number())?;
                let outcome = state.ext4.rename(
                    self.number(),
                    src_name,
                    dst_dir.number(),
                    dst_name,
                    core_options,
                )?;
                Ok(
                    match outcome.replaced.filter(|outcome| outcome.requires_reap()) {
                        Some(outcome) => state.publish_zero_link(outcome.inode),
                        None => None,
                    },
                )
            })?;
        if let Some(claim) = reap_claim {
            self.fs().reap(claim)?;
        }
        if !self.fs().background_writeback_enabled()
            || self.writeback_policy()?.syncs_directory()
            || dst_dir.writeback_policy()?.syncs_directory()
        {
            self.fs().sync_to_disk()?;
        }
        Ok(())
    }
}

impl Inode {
    fn create_entry(&self, located: LocatedInode, name: &str) -> DirEntry {
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

    const fn mutation_context(uid: u32, gid: u32) -> MutationContext {
        MutationContext::new(uid, gid, 0, 0)
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
