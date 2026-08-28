use alloc::{borrow::ToOwned, boxed::Box, sync::Arc, vec::Vec};
use core::any::Any;

use axfs_ng_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, DirectoryCursor as VfsDirectoryCursor,
    DirectoryReadState, FileExtent as VfsFileExtent, FileExtentMap as VfsFileExtentMap,
    FileExtentState as VfsFileExtentState, FileExtentTarget as VfsFileExtentTarget, FileNode,
    FileNodeOps, FileRangeOperation as VfsRangeOperation, FilesystemOps, FsIoEvents, FsPollable,
    Metadata, MetadataUpdate, NodeFlags, NodeOps, NodePermission, NodeType,
    PreallocationMode as VfsPreallocationMode, Reference, RenameOptions as VfsRenameOptions,
    VfsError, VfsResult, WeakDirEntry, XattrOps, XattrSetMode as VfsXattrSetMode,
};
use rsext4::{
    DeviceNumber, DirectoryCursor, Ext4Timestamp, FileName, FilePermissions, InodeFlags,
    InodeMetadataUpdate, InodeNumber, MutationContext, PreallocationOptions, RangeOperation,
    SpecialInodeKind, XattrNamespace, XattrSetMode, ZeroRangeOptions,
};

use super::{
    Ext4Filesystem,
    util::{directory_entry_type_to_vfs, into_vfs_err},
};
use crate::highlevel::forget_cached_file_key;

pub struct Inode {
    fs: Arc<Ext4Filesystem>,
    ino: InodeNumber,
    this: Option<WeakDirEntry>,
}

struct Ext4DirectoryReadState {
    reader: rsext4::DirectoryReader,
}

impl Inode {
    pub(crate) fn new(
        fs: Arc<Ext4Filesystem>,
        ino: InodeNumber,
        this: Option<WeakDirEntry>,
    ) -> Arc<Self> {
        // NOTE: callers MUST call state.inc_ref(ino) before or after
        // creating the Inode Arc.  We cannot lock here because many
        // callers already hold the Ext4State lock (lookup_locked,
        // create, link) and the outer sleepable mutex is not recursive.
        Arc::new(Self { fs, ino, this })
    }

    fn create_entry(&self, info: rsext4::InodeInfo, name: &str) -> DirEntry {
        let name = name.to_owned();
        let reference = Reference::new(
            self.this.as_ref().and_then(WeakDirEntry::upgrade),
            name.clone(),
        );
        if info.is_directory() {
            DirEntry::new_dir(
                |this| DirNode::new(Inode::new(self.fs.clone(), info.number, Some(this))),
                reference,
            )
        } else {
            DirEntry::new_file(
                FileNode::new(Inode::new(self.fs.clone(), info.number, None)),
                directory_entry_type_to_vfs(info.file_type()),
                reference,
            )
        }
    }

    const fn mutation_context(uid: u32, gid: u32) -> MutationContext {
        MutationContext::new(uid, gid, 0, 0)
    }

    fn timestamp(value: core::time::Duration) -> VfsResult<Ext4Timestamp> {
        let seconds = i64::try_from(value.as_secs())
            .map_err(|_| into_vfs_err(rsext4::Ext4Error::overflow()))?;
        Ok(Ext4Timestamp::new(seconds, value.subsec_nanos()))
    }

    fn lookup_locked(&self, name: &str) -> VfsResult<DirEntry> {
        let raw_name = FileName::new(name.as_bytes()).map_err(into_vfs_err)?;
        let mut state = self.fs.lock();
        let info = state
            .ext4
            .lookup_child(self.ino, raw_name)
            .map_err(into_vfs_err)?
            .ok_or(VfsError::NotFound)?;
        state.inc_ref(info.number);
        Ok(self.create_entry(info, name))
    }

    fn inspect_extents(
        &self,
        offset: u64,
        len: u64,
        target: VfsFileExtentTarget,
        extent_limit: usize,
    ) -> VfsResult<VfsFileExtentMap> {
        let mut state = self.fs.lock();
        let mappings = state
            .ext4
            .inode_extents(
                self.ino,
                offset,
                len,
                match target {
                    VfsFileExtentTarget::Data => rsext4::FileExtentTarget::Data,
                    VfsFileExtentTarget::ExtendedAttributes => {
                        rsext4::FileExtentTarget::ExtendedAttributes
                    }
                },
                extent_limit,
            )
            .map_err(into_vfs_err)?;
        Ok(VfsFileExtentMap {
            mapped_extents: mappings.mapped_extents,
            complete: mappings.complete,
            extents: mappings
                .extents
                .into_iter()
                .map(|extent| VfsFileExtent {
                    logical_start: extent.logical_start,
                    physical_start: extent.physical_start,
                    length: extent.length,
                    state: match extent.state {
                        rsext4::FileExtentState::Initialized => VfsFileExtentState::Initialized,
                        rsext4::FileExtentState::Unwritten => VfsFileExtentState::Unwritten,
                        rsext4::FileExtentState::Inline => VfsFileExtentState::Inline,
                    },
                    merged: extent.merged,
                })
                .collect(),
        })
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
                let mut state = self.fs.lock();
                let inode = state.ext4.inode(self.ino).map_err(into_vfs_err)?;
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

    fn user_xattr_name(name: &[u8]) -> VfsResult<&[u8]> {
        const PREFIX: &[u8] = b"user.";
        let component = name
            .strip_prefix(PREFIX)
            .ok_or(VfsError::OperationNotSupported)?;
        if component.is_empty() {
            return Err(VfsError::InvalidInput);
        }
        Ok(component)
    }

    fn xattr_error(error: rsext4::Ext4Error) -> VfsError {
        if error.kind() == rsext4::Ext4ErrorKind::NotFound {
            VfsError::DataMissing
        } else {
            into_vfs_err(error)
        }
    }
}

impl Drop for Inode {
    fn drop(&mut self) {
        let claim = self.fs.lock().release_ref(self.ino);
        if let Some(claim) = claim
            && let Err(error) = self.fs.reap(claim)
        {
            log::error!("failed to reap zero-link ext4 inode: {error:?}");
        }
    }
}

impl NodeOps for Inode {
    fn inode(&self) -> u64 {
        self.ino.as_u64()
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        let mut state = self.fs.lock();
        let inode = state.ext4.inode(self.ino).map_err(into_vfs_err)?;
        let node_type = directory_entry_type_to_vfs(inode.file_type());
        let block_size = state.ext4.statfs().block_size;
        Ok(Metadata {
            inode: self.ino.as_u64(),
            device: 0,
            nlink: inode.links as _,
            mode: NodePermission::from_bits_truncate(inode.mode),
            node_type,
            uid: inode.uid,
            gid: inode.gid,
            size: inode.size,
            block_size,
            blocks: inode.blocks,
            rdev: inode
                .device_number
                .map(|device| DeviceId::new(device.major(), device.minor()))
                .unwrap_or_default(),
            atime: core::time::Duration::from_secs(u64::from(inode.atime)),
            mtime: core::time::Duration::from_secs(u64::from(inode.mtime)),
            ctime: core::time::Duration::from_secs(u64::from(inode.ctime)),
        })
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        {
            let mut state = self.fs.lock();
            let metadata = InodeMetadataUpdate {
                permissions: update
                    .mode
                    .map(|mode| FilePermissions::new(mode.bits()))
                    .transpose()
                    .map_err(into_vfs_err)?,
                owner: update.owner,
                device_number: update
                    .rdev
                    .map(|device| DeviceNumber::new(device.major(), device.minor()))
                    .transpose()
                    .map_err(into_vfs_err)?,
                atime: update.atime.map(Self::timestamp).transpose()?,
                mtime: update.mtime.map(Self::timestamp).transpose()?,
                ..Default::default()
            };
            state
                .ext4
                .update_inode_metadata(self.ino, metadata)
                .map_err(into_vfs_err)?;
        }
        self.fs.sync_to_disk()
    }

    fn len(&self) -> VfsResult<u64> {
        let mut state = self.fs.lock();
        state
            .ext4
            .inode(self.ino)
            .map(|inode| inode.size)
            .map_err(into_vfs_err)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        &*self.fs
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        self.fs.sync_to_disk()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::BLOCKING
    }

    fn xattr_ops(&self) -> Option<&dyn XattrOps> {
        Some(self)
    }
}

impl XattrOps for Inode {
    fn get_xattr(&self, name: &[u8]) -> VfsResult<Vec<u8>> {
        let name = Self::user_xattr_name(name)?;
        let mut state = self.fs.lock();
        state
            .ext4
            .get_xattr(self.ino, XattrNamespace::User, name)
            .map_err(Self::xattr_error)
    }

    fn list_xattrs(&self) -> VfsResult<Vec<Vec<u8>>> {
        let mut state = self.fs.lock();
        let names = state
            .ext4
            .list_xattrs(self.ino)
            .map_err(Self::xattr_error)?;
        Ok(names
            .into_iter()
            .filter(|name| name.namespace == XattrNamespace::User)
            .map(|name| {
                let mut full_name = b"user.".to_vec();
                full_name.extend_from_slice(&name.name);
                full_name
            })
            .collect())
    }

    fn set_xattr(&self, name: &[u8], value: &[u8], mode: VfsXattrSetMode) -> VfsResult<()> {
        let name = Self::user_xattr_name(name)?;
        let mode = match mode {
            VfsXattrSetMode::Upsert => XattrSetMode::Upsert,
            VfsXattrSetMode::Create => XattrSetMode::Create,
            VfsXattrSetMode::Replace => XattrSetMode::Replace,
        };
        let mut state = self.fs.lock();
        state
            .ext4
            .set_xattr(self.ino, XattrNamespace::User, name, value, mode)
            .map_err(Self::xattr_error)
    }

    fn remove_xattr(&self, name: &[u8]) -> VfsResult<()> {
        let name = Self::user_xattr_name(name)?;
        let mut state = self.fs.lock();
        state
            .ext4
            .remove_xattr(self.ino, XattrNamespace::User, name)
            .map_err(Self::xattr_error)
    }
}

impl FileNodeOps for Inode {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let mut state = self.fs.lock();
        state
            .ext4
            .read_inode(self.ino, offset, buf)
            .map_err(into_vfs_err)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        let mut state = self.fs.lock();
        // Use inode-number-based write so open-unlinked regular files remain
        // writable after their directory entry has been removed.
        state
            .ext4
            .write_inode(self.ino, offset, buf)
            .map_err(into_vfs_err)?;
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let mut state = self.fs.lock();
        let length = state.ext4.inode(self.ino).map_err(into_vfs_err)?.size;
        state
            .ext4
            .write_inode(self.ino, length, buf)
            .map_err(into_vfs_err)?;
        let end = length
            .checked_add(buf.len() as u64)
            .ok_or_else(|| into_vfs_err(rsext4::Ext4Error::overflow()))?;
        Ok((buf.len(), end))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        let mut state = self.fs.lock();
        // An open-unlinked regular file stays alive by inode number, not by a
        // directory entry.
        state
            .ext4
            .truncate_inode(self.ino, len)
            .map_err(into_vfs_err)
    }

    fn operate_range(&self, offset: u64, len: u64, operation: VfsRangeOperation) -> VfsResult<()> {
        let operation = match operation {
            VfsRangeOperation::Allocate(mode) => RangeOperation::Allocate(match mode {
                VfsPreallocationMode::ExtendSize => PreallocationOptions::EXTEND_SIZE,
                VfsPreallocationMode::KeepSize => PreallocationOptions::KEEP_SIZE,
            }),
            VfsRangeOperation::PunchHole => RangeOperation::PunchHole,
            VfsRangeOperation::ZeroRange(mode) => RangeOperation::Zero(match mode {
                VfsPreallocationMode::ExtendSize => ZeroRangeOptions::EXTEND_SIZE,
                VfsPreallocationMode::KeepSize => ZeroRangeOptions::KEEP_SIZE,
            }),
            VfsRangeOperation::CollapseRange => RangeOperation::Collapse,
            VfsRangeOperation::InsertRange => RangeOperation::Insert,
        };
        let mut state = self.fs.lock();
        state
            .ext4
            .operate_inode_range(self.ino, offset, len, operation)
            .map_err(into_vfs_err)
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
}

impl FsPollable for Inode {
    fn poll(&self) -> FsIoEvents {
        FsIoEvents::IN | FsIoEvents::OUT
    }

    fn register(&self, _context: &mut core::task::Context<'_>, _events: FsIoEvents) {}
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
            .fs
            .lock()
            .ext4
            .open_directory_reader(self.ino)
            .map_err(into_vfs_err)?;
        self.read_dir_with_core_reader(&mut reader, cursor, sink)
    }

    fn open_directory_read_state(&self) -> VfsResult<Box<dyn DirectoryReadState>> {
        let reader = self
            .fs
            .lock()
            .ext4
            .open_directory_reader(self.ino)
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
        if state.reader.directory() != self.ino {
            return Err(VfsError::InvalidInput);
        }
        self.read_dir_with_core_reader(&mut state.reader, cursor, sink)
    }

    fn directory_end_cursor(&self) -> VfsResult<VfsDirectoryCursor> {
        let cursor = self
            .fs
            .lock()
            .ext4
            .directory_end_cursor(self.ino)
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
        self.lookup_locked(name)
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
        let info = {
            let mut state = self.fs.lock();
            let info = match node_type {
                NodeType::RegularFile => {
                    state
                        .ext4
                        .create_regular_file(context, self.ino, raw_name, permissions)
                }
                NodeType::Directory => {
                    state
                        .ext4
                        .create_directory(context, self.ino, raw_name, permissions)
                }
                NodeType::Symlink => return Err(VfsError::InvalidInput),
                NodeType::CharacterDevice => state.ext4.create_special_inode(
                    context,
                    self.ino,
                    raw_name,
                    permissions,
                    SpecialInodeKind::CharacterDevice(DeviceNumber::ZERO),
                ),
                NodeType::BlockDevice => state.ext4.create_special_inode(
                    context,
                    self.ino,
                    raw_name,
                    permissions,
                    SpecialInodeKind::BlockDevice(DeviceNumber::ZERO),
                ),
                NodeType::Fifo => state.ext4.create_special_inode(
                    context,
                    self.ino,
                    raw_name,
                    permissions,
                    SpecialInodeKind::Fifo,
                ),
                NodeType::Socket => state.ext4.create_special_inode(
                    context,
                    self.ino,
                    raw_name,
                    permissions,
                    SpecialInodeKind::Socket,
                ),
                NodeType::Unknown => return Err(VfsError::InvalidData),
            }
            .map_err(into_vfs_err)?;
            state.inc_ref(info.number);
            info
        };

        let entry = self.create_entry(info, name);
        self.fs.sync_to_disk()?;
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
        let info = {
            let mut state = self.fs.lock();
            let info = state
                .ext4
                .create_symlink(
                    Self::mutation_context(uid, gid),
                    self.ino,
                    raw_name,
                    target.as_bytes(),
                )
                .map_err(into_vfs_err)?;
            state.inc_ref(info.number);
            info
        };

        let entry = self.create_entry(info, name);
        self.fs.sync_to_disk()?;
        Ok(entry)
    }

    fn link(&self, name: &str, node: &DirEntry) -> VfsResult<DirEntry> {
        let target: Arc<Self> = node.downcast().map_err(|_| VfsError::InvalidInput)?;
        if !Arc::ptr_eq(&self.fs, &target.fs) {
            return Err(VfsError::CrossesDevices);
        }
        let raw_name = FileName::new(name.as_bytes()).map_err(into_vfs_err)?;
        let info = {
            let mut state = self.fs.lock();
            let info = state
                .ext4
                .hard_link(target.ino, self.ino, raw_name)
                .map_err(into_vfs_err)?;
            state.inc_ref(info.number);
            info
        };
        let entry = self.create_entry(info, name);
        self.fs.sync_to_disk()?;
        Ok(entry)
    }

    fn unlink(&self, name: &str, is_dir: bool) -> VfsResult<()> {
        let raw_name = FileName::new(name.as_bytes()).map_err(into_vfs_err)?;
        let (zero_link_inode, reap_claim) = {
            let mut state = self.fs.lock();
            let info = state
                .ext4
                .lookup_child(self.ino, raw_name)
                .map_err(into_vfs_err)?
                .ok_or(VfsError::NotFound)?;
            let target_is_dir = info.is_directory();
            match (target_is_dir, is_dir) {
                (true, false) => return Err(VfsError::IsADirectory),
                (false, true) => return Err(VfsError::NotADirectory),
                _ => {}
            }
            let outcome = if target_is_dir {
                state.ext4.remove_empty_directory(self.ino, raw_name)
            } else {
                state.ext4.unlink(self.ino, raw_name)
            }
            .map_err(into_vfs_err)?;
            if outcome.requires_reap() {
                let claim = state.publish_zero_link(outcome.inode);
                (Some(outcome.inode), claim)
            } else {
                (None, None)
            }
        };
        if let Some(claim) = reap_claim {
            self.fs.reap(claim)?;
        }
        if let Some(ino) = zero_link_inode {
            forget_cached_file_key(&*self.fs, ino.as_u64());
        }
        self.fs.sync_to_disk()
    }

    fn rename(
        &self,
        src_name: &str,
        dst_dir: &DirNode,
        dst_name: &str,
        options: VfsRenameOptions,
    ) -> VfsResult<()> {
        let dst_dir: Arc<Self> = dst_dir.downcast().map_err(|_| VfsError::InvalidInput)?;
        if !Arc::ptr_eq(&self.fs, &dst_dir.fs) {
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
        let (zero_link_inode, reap_claim) = {
            let mut state = self.fs.lock();
            let outcome = state
                .ext4
                .rename(self.ino, src_name, dst_dir.ino, dst_name, core_options)
                .map_err(into_vfs_err)?;
            match outcome.replaced.filter(|outcome| outcome.requires_reap()) {
                Some(outcome) => {
                    let claim = state.publish_zero_link(outcome.inode);
                    (Some(outcome.inode), claim)
                }
                None => (None, None),
            }
        };
        if let Some(claim) = reap_claim {
            self.fs.reap(claim)?;
        }
        if let Some(ino) = zero_link_inode {
            forget_cached_file_key(&*self.fs, ino.as_u64());
        }
        self.fs.sync_to_disk()
    }
}

const DIRECTORY_READ_BATCH_ENTRIES: usize = 128;

// Linux reserves this value for HTree EOF. ext4's directory hash finalization
// rewrites the only colliding major hash, 0xffff_fffe, to 0xffff_fffc.
const HTREE_EOF_COOKIE: u64 = i64::MAX as u64;

fn vfs_to_core_directory_cursor(
    cursor: VfsDirectoryCursor,
    indexed: bool,
) -> VfsResult<DirectoryCursor> {
    if cursor.offset() == HTREE_EOF_COOKIE {
        return Ok(DirectoryCursor::End);
    }
    if cursor.offset() == 0 && cursor.continuation() == 0 {
        return Ok(DirectoryCursor::Start);
    }
    if !indexed {
        return Ok(DirectoryCursor::Linear {
            offset: cursor.offset(),
        });
    }
    let collision = u32::try_from(cursor.continuation()).map_err(|_| VfsError::InvalidInput)?;
    Ok(DirectoryCursor::HTree {
        major: ((cursor.offset() >> 32) as u32) << 1,
        minor: cursor.offset() as u32,
        collision,
    })
}

fn core_to_vfs_directory_cursor(
    cursor: DirectoryCursor,
    change_attribute: Option<u64>,
) -> VfsDirectoryCursor {
    let (offset, continuation) = match cursor {
        DirectoryCursor::Start => (0, 0),
        DirectoryCursor::Linear { offset } => (offset, 0),
        DirectoryCursor::HTree {
            major,
            minor,
            collision,
        } => (
            (u64::from(major >> 1) << 32) | u64::from(minor),
            u64::from(collision),
        ),
        DirectoryCursor::End => (HTREE_EOF_COOKIE, 0),
    };
    match change_attribute {
        Some(change_attribute) => VfsDirectoryCursor::with_observed_change_attribute(
            offset,
            continuation,
            change_attribute,
        ),
        None => VfsDirectoryCursor::with_continuation(offset, continuation),
    }
}

fn normalize_directory_cursor(
    cursor: VfsDirectoryCursor,
    change_attribute: u64,
) -> VfsDirectoryCursor {
    let continuation = match cursor.observed_change_attribute() {
        Some(observed) if observed != change_attribute => 0,
        _ => cursor.continuation(),
    };
    VfsDirectoryCursor::with_observed_change_attribute(
        cursor.offset(),
        continuation,
        change_attribute,
    )
}

#[cfg(test)]
mod directory_cursor_tests {
    use super::*;

    #[test]
    fn linux_64_bit_htree_cookie_round_trips_private_collision_state() {
        let core = DirectoryCursor::HTree {
            major: 0x89ab_cdec,
            minor: 0x1357_2468,
            collision: 7,
        };
        let vfs = core_to_vfs_directory_cursor(core, Some(41));

        assert_eq!(vfs.offset(), 0x44d5_e6f6_1357_2468);
        assert_eq!(vfs.continuation(), 7);
        assert_eq!(vfs.observed_change_attribute(), Some(41));
        assert_eq!(vfs_to_core_directory_cursor(vfs, true), Ok(core));
    }

    #[test]
    fn external_seek_cookie_resets_private_collision_state() {
        let cookie = core_to_vfs_directory_cursor(
            DirectoryCursor::HTree {
                major: 0x1234_5678,
                minor: 0x9abc_def0,
                collision: 11,
            },
            Some(7),
        );
        let external_seek = VfsDirectoryCursor::new(cookie.offset());

        assert_eq!(
            vfs_to_core_directory_cursor(external_seek, true),
            Ok(DirectoryCursor::HTree {
                major: 0x1234_5678,
                minor: 0x9abc_def0,
                collision: 0,
            })
        );
    }

    #[test]
    fn htree_eof_cookie_maps_to_core_end() {
        let cursor = VfsDirectoryCursor::new(HTREE_EOF_COOKIE);
        assert_eq!(
            vfs_to_core_directory_cursor(cursor, true),
            Ok(DirectoryCursor::End)
        );
        assert_eq!(
            core_to_vfs_directory_cursor(DirectoryCursor::End, None),
            cursor
        );
    }

    #[test]
    fn largest_linux_directory_hash_does_not_collide_with_eof() {
        let core = DirectoryCursor::HTree {
            major: 0xffff_fffc,
            minor: u32::MAX,
            collision: 0,
        };
        let vfs = core_to_vfs_directory_cursor(core, None);

        assert_eq!(vfs.offset(), 0x7fff_fffe_ffff_ffff);
        assert_ne!(vfs.offset(), HTREE_EOF_COOKIE);
        assert_eq!(vfs_to_core_directory_cursor(vfs, true), Ok(core));
    }

    #[test]
    fn directory_mutation_discards_private_collision_continuation() {
        let stale =
            VfsDirectoryCursor::with_observed_change_attribute(0x1234_5678_9abc_def0, 11, 41);

        let current = normalize_directory_cursor(stale, 42);

        assert_eq!(current.offset(), stale.offset());
        assert_eq!(current.continuation(), 0);
        assert_eq!(current.observed_change_attribute(), Some(42));
    }

    #[test]
    fn unchanged_directory_keeps_private_collision_continuation() {
        let cursor =
            VfsDirectoryCursor::with_observed_change_attribute(0x1234_5678_9abc_def0, 11, 42);

        assert_eq!(normalize_directory_cursor(cursor, 42), cursor);
    }
}
