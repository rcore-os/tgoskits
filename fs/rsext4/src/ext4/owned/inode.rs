//! Inode attributes, file contents, and allocation-range operations.

use super::*;

/// Pure caller metadata associated with one filesystem mutation.
///
/// Permission and capability checks stay in the VFS. These values are only
/// inputs to on-disk ownership, umask, and quota semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationContext {
    pub uid: u32,
    pub gid: u32,
    pub project_id: u32,
    pub umask: u16,
}

impl MutationContext {
    pub const fn new(uid: u32, gid: u32, project_id: u32, umask: u16) -> Self {
        Self {
            uid,
            gid,
            project_id,
            umask,
        }
    }
}

/// Permission bits supplied by a VFS after its policy checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct FilePermissions(u16);

impl FilePermissions {
    const VALID_BITS: u16 = 0o7777;

    pub fn new(bits: u16) -> Ext4Result<Self> {
        if bits & !Self::VALID_BITS != 0 {
            return Err(Ext4Error::invalid_input().with_operation("inode:permissions"));
        }
        Ok(Self(bits))
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub(super) const fn masked_by(self, umask: u16) -> u16 {
        self.0 & !(umask & 0o777)
    }
}

bitflags! {
    /// Stable user-visible ext4 inode flags.
    ///
    /// The core may preserve additional on-disk implementation flags, but they
    /// never cross this boundary and cannot be changed by callers.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InodeFlags: u32 {
        const SYNC = Ext4Inode::EXT4_SYNC_FL;
        const IMMUTABLE = Ext4Inode::EXT4_IMMUTABLE_FL;
        const APPEND = Ext4Inode::EXT4_APPEND_FL;
        const NO_DUMP = Ext4Inode::EXT4_NODUMP_FL;
        const NO_ATIME = Ext4Inode::EXT4_NOATIME_FL;
        const DIRECTORY_SYNC = Ext4Inode::EXT4_DIRSYNC_FL;
        const TOP_DIRECTORY = Ext4Inode::EXT4_TOPDIR_FL;
        const PROJECT_INHERIT = Ext4Inode::EXT4_PROJINHERIT_FL;
        const DIRTY = Ext4Inode::EXT4_DIRTY_FL;
        const COMPRESSED_BLOCKS = Ext4Inode::EXT4_COMPRBLK_FL;
        const NO_COMPRESSION = Ext4Inode::EXT4_NOCOMPR_FL;
        const ENCRYPTED = Ext4Inode::EXT4_ENCRYPT_FL;
        const DIRECTORY_INDEX = Ext4Inode::EXT4_INDEX_FL;
        const HUGE_FILE = Ext4Inode::EXT4_HUGE_FILE_FL;
        const EXTENTS = Ext4Inode::EXT4_EXTENTS_FL;
        const EA_INODE = Ext4Inode::EXT4_EA_INODE_FL;
        const EOF_BLOCKS = Ext4Inode::EXT4_EOFBLOCKS_FL;
        const INLINE_DATA = Ext4Inode::EXT4_INLINE_DATA_FL;
    }
}

/// Stable inode inspection data returned across the portable core boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InodeInfo {
    pub number: InodeNumber,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub links: u16,
    pub size: u64,
    pub blocks: u64,
    pub atime: u32,
    pub ctime: u32,
    pub mtime: u32,
    pub btime: u32,
    /// Persistent ext4 change attribute used to invalidate directory state.
    pub change_attribute: u64,
    pub project_id: u32,
    pub flags: InodeFlags,
    pub device_number: Option<DeviceNumber>,
}

impl InodeInfo {
    /// Returns the stable inode kind without exposing the on-disk mode layout.
    pub const fn file_type(&self) -> DirectoryEntryType {
        match self.mode & Ext4Inode::S_IFMT {
            Ext4Inode::S_IFREG => DirectoryEntryType::RegularFile,
            Ext4Inode::S_IFDIR => DirectoryEntryType::Directory,
            Ext4Inode::S_IFCHR => DirectoryEntryType::CharacterDevice,
            Ext4Inode::S_IFBLK => DirectoryEntryType::BlockDevice,
            Ext4Inode::S_IFIFO => DirectoryEntryType::Fifo,
            Ext4Inode::S_IFSOCK => DirectoryEntryType::Socket,
            Ext4Inode::S_IFLNK => DirectoryEntryType::Symlink,
            _ => DirectoryEntryType::Unknown,
        }
    }

    pub const fn is_directory(&self) -> bool {
        matches!(self.file_type(), DirectoryEntryType::Directory)
    }
}

/// Metadata changes already authorized and normalized by the embedding VFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InodeMetadataUpdate {
    pub permissions: Option<FilePermissions>,
    pub owner: Option<(u32, u32)>,
    pub device_number: Option<DeviceNumber>,
    pub atime: Option<Ext4Timestamp>,
    pub mtime: Option<Ext4Timestamp>,
    pub project_id: Option<u32>,
    pub flags: Option<InodeFlags>,
}

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    pub fn inode(&mut self, number: InodeNumber) -> Ext4Result<InodeInfo> {
        if !self
            .filesystem
            .inode_is_allocated_checked(&mut self.device, number)?
        {
            return Err(Ext4Error::not_found().with_operation("inode:inspect_unallocated"));
        }
        let inode = self.filesystem.get_inode_by_num(&mut self.device, number)?;
        self.inspect_inode(number, inode)
    }

    /// Returns a bounded, byte-addressed view of allocated file mappings.
    pub fn inode_extents(
        &mut self,
        number: InodeNumber,
        start: u64,
        length: u64,
        target: FileExtentTarget,
        extent_limit: usize,
    ) -> Ext4Result<FileExtentMap> {
        inspect_inode_extents(
            &mut self.device,
            &mut self.filesystem,
            number,
            start,
            length,
            target,
            extent_limit,
        )
    }

    /// Reads one ext4 extended attribute by inode number and raw namespace name.
    pub fn get_xattr(
        &mut self,
        number: InodeNumber,
        namespace: XattrNamespace,
        name: &[u8],
    ) -> Ext4Result<Vec<u8>> {
        get_inode_xattr(
            &mut self.device,
            &mut self.filesystem,
            number,
            namespace,
            name,
        )
    }

    /// Lists ext4 extended-attribute names without applying OS visibility policy.
    pub fn list_xattrs(&mut self, number: InodeNumber) -> Ext4Result<Vec<XattrName>> {
        list_inode_xattrs(&mut self.device, &mut self.filesystem, number)
    }

    /// Creates or replaces one VFS-authorized extended attribute.
    pub fn set_xattr(
        &mut self,
        number: InodeNumber,
        namespace: XattrNamespace,
        name: &[u8],
        value: &[u8],
        mode: XattrSetMode,
    ) -> Ext4Result<()> {
        self.ensure_writable("xattr:set")?;
        set_inode_xattr(
            &mut self.device,
            &mut self.filesystem,
            number,
            namespace,
            name,
            value,
            mode,
        )
    }

    /// Removes one VFS-authorized extended attribute.
    pub fn remove_xattr(
        &mut self,
        number: InodeNumber,
        namespace: XattrNamespace,
        name: &[u8],
    ) -> Ext4Result<()> {
        self.ensure_writable("xattr:remove")?;
        remove_inode_xattr(
            &mut self.device,
            &mut self.filesystem,
            number,
            namespace,
            name,
        )
    }

    /// Applies VFS-authorized metadata fields through the checked inode codec.
    pub fn update_inode_metadata(
        &mut self,
        number: InodeNumber,
        update: InodeMetadataUpdate,
    ) -> Ext4Result<InodeInfo> {
        let current = self.inode(number)?;
        let has_project_feature = self.filesystem.superblock.has_feature_ro_compat(
            crate::superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_PROJECT,
        );
        let mut update = update;
        if !has_project_feature && update.project_id == Some(0) {
            update.project_id = None;
        }
        if update == InodeMetadataUpdate::default() {
            return Ok(current);
        }
        self.ensure_writable("inode:update_metadata")?;
        let mut inode = self.filesystem.get_inode_by_num(&mut self.device, number)?;
        if let Some(project_id) = update.project_id {
            if !has_project_feature && project_id != 0 {
                return Err(Ext4Error::unsupported().with_operation("inode:project_feature"));
            }
            if has_project_feature {
                self.filesystem
                    .ensure_extra_isize_for_field(&mut inode, Ext4Inode::FIELD_END_I_PROJID)?;
                inode.i_projid = project_id;
            }
        }
        if let Some(flags) = update.flags {
            let modifiable = Ext4Inode::mask_flags_for_mode(
                inode.i_mode,
                flags.bits() & Ext4Inode::EXT4_FL_USER_MODIFIABLE,
            );
            inode.i_flags = (inode.i_flags & !Ext4Inode::EXT4_FL_USER_MODIFIABLE) | modifiable;
        }
        if let Some(device_number) = update.device_number {
            inode.set_device_number(device_number)?;
        }
        let (uid, gid) = match update.owner {
            Some((uid, gid)) => (Some(uid), Some(gid)),
            None => (None, None),
        };
        self.filesystem.finalize_inode_update(
            &mut self.device,
            number,
            &mut inode,
            Ext4InodeMetadataUpdate {
                reason: Ext4MetadataReason::Utimens,
                mode: update
                    .permissions
                    .map(|permissions| Ext4ModeUpdate::Chmod(permissions.bits())),
                uid,
                gid,
                atime: update.atime.map(Ext4TimeSpec::Set),
                mtime: update.mtime.map(Ext4TimeSpec::Set),
                ctime: Some(Ext4TimeSpec::Now),
                clear_suid_sgid_on_chown: update.owner.is_some(),
                ..Default::default()
            },
        )?;
        self.inspect_inode(number, inode)
    }

    pub(super) fn inspect_inode(
        &self,
        number: InodeNumber,
        inode: Ext4Inode,
    ) -> Ext4Result<InodeInfo> {
        super::metadata::InodeLayout::from_filesystem(&self.filesystem).decode(number, inode)
    }

    pub fn read_inode(
        &mut self,
        number: InodeNumber,
        offset: u64,
        output: &mut [u8],
    ) -> Ext4Result<usize> {
        self.ensure_mounted("inode:read")?;
        self.writes.ensure_inode_idle(number)?;
        let copied = read_inode_data_into(
            &mut self.device,
            &mut self.filesystem,
            number,
            offset,
            output,
        )?;
        if copied != 0 && !self.options.readonly {
            self.filesystem
                .touch_inode_atime_if_needed(&mut self.device, number)?;
        }
        Ok(copied)
    }

    pub fn write_inode(
        &mut self,
        number: InodeNumber,
        offset: u64,
        input: &[u8],
    ) -> Ext4Result<()> {
        self.ensure_writable("inode:write")?;
        self.writes.ensure_inode_idle(number)?;
        write_inode_data(
            &mut self.device,
            &mut self.filesystem,
            number,
            offset,
            input,
        )
    }

    pub fn truncate_inode(&mut self, number: InodeNumber, size: u64) -> Ext4Result<()> {
        self.ensure_writable("inode:truncate")?;
        self.writes.ensure_inode_idle(number)?;
        truncate_inode(&mut self.device, &mut self.filesystem, number, size)
    }

    /// Advances a resize intent after lock-external journal progress.
    pub fn resize_inode(&mut self, resize: &mut crate::InodeResize) -> Ext4Result<()> {
        self.ensure_writable("inode:resize")?;
        self.writes.ensure_inode_idle(resize.inode_number())?;
        resize.resume(&mut self.device, &mut self.filesystem)
    }

    /// Whether writes use the extent path with restartable allocation steps.
    /// Legacy indirect writes require the adapter's synchronous compatibility
    /// exclusion because their rollback spans multiple journal handles.
    pub fn inode_has_restartable_writes(&mut self, number: InodeNumber) -> Ext4Result<bool> {
        self.ensure_mounted("inode:write_mapping")?;
        Ok(self
            .filesystem
            .get_inode_by_num(&mut self.device, number)?
            .uses_extents())
    }

    pub fn preallocate_inode(
        &mut self,
        number: InodeNumber,
        offset: u64,
        len: u64,
        options: PreallocationOptions,
    ) -> Ext4Result<()> {
        self.operate_inode_range(number, offset, len, RangeOperation::Allocate(options))
    }

    /// Applies one allocation or mapping operation to a byte range.
    pub fn operate_inode_range(
        &mut self,
        number: InodeNumber,
        offset: u64,
        len: u64,
        operation: RangeOperation,
    ) -> Ext4Result<()> {
        self.ensure_writable("inode:operate_range")?;
        self.writes.ensure_inode_idle(number)?;
        operate_inode_range(
            &mut self.device,
            &mut self.filesystem,
            number,
            offset,
            len,
            operation,
        )
    }
}
