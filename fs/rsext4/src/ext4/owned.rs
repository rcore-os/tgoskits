//! Owned, OS-independent mounted filesystem boundary.

use alloc::vec::Vec;

use bitflags::bitflags;

use super::{Ext4FileSystem, FileSystemStats, MkfsOptions, MountOptions, mkfs_with_options};
use crate::{
    blockdev::Jbd2Dev,
    bmalloc::InodeNumber,
    checksum::{verify_ext4_dirblock_checksum, verify_ext4_dx_checksum},
    dir::{CreateEntryRequest, FileName, LinkEntryRequest, create_directory_at},
    disknode::{DeviceNumber, Ext4Inode, Ext4TimeSpec, Ext4Timestamp},
    entries::Ext4DirEntry2,
    error::{Ext4Error, Ext4ErrorKind, Ext4Result},
    file::{
        CreateInodePayload, FileExtentMap, FileExtentTarget, PreallocationOptions, RangeOperation,
        RenameEntryRequest, RenameOptions, RenameOutcome, UnlinkOutcome, XattrName, XattrNamespace,
        XattrSetMode, build_file_block_mapping_with_inode_num, create_inode_at,
        discard_unpublished_inode_blocks, error_after_cleanup, find_named_entry_in_parent,
        get_inode_xattr, inspect_inode_extents, link_inode_at, list_inode_xattrs,
        operate_inode_range, read_inode_data_into, reap_unlinked_inode, remove_inode_xattr,
        rename_inode_at, set_inode_xattr, truncate_inode, unlink_empty_directory_at,
        unlink_inode_at, write_inode_data,
    },
    hashtree::Ext4InodeHashTreeExt,
    io::BlockIo,
    loopfile::resolve_inode_blocks,
    metadata::{Ext4InodeMetadataUpdate, Ext4MetadataReason, Ext4ModeUpdate},
    runtime::{Clock, MountServices, MountedServices, Observer},
};

/// Stable directory-entry type independent from VFS or Linux ABI enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryEntryType {
    Unknown,
    RegularFile,
    Directory,
    CharacterDevice,
    BlockDevice,
    Fifo,
    Socket,
    Symlink,
}

/// Special inode kind accepted by the portable core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialInodeKind {
    CharacterDevice(DeviceNumber),
    BlockDevice(DeviceNumber),
    Fifo,
    Socket,
}

impl SpecialInodeKind {
    const fn inode_type(self) -> u16 {
        match self {
            Self::CharacterDevice(_) => Ext4Inode::S_IFCHR,
            Self::BlockDevice(_) => Ext4Inode::S_IFBLK,
            Self::Fifo => Ext4Inode::S_IFIFO,
            Self::Socket => Ext4Inode::S_IFSOCK,
        }
    }

    const fn directory_entry_type(self) -> u8 {
        match self {
            Self::CharacterDevice(_) => Ext4DirEntry2::EXT4_FT_CHRDEV,
            Self::BlockDevice(_) => Ext4DirEntry2::EXT4_FT_BLKDEV,
            Self::Fifo => Ext4DirEntry2::EXT4_FT_FIFO,
            Self::Socket => Ext4DirEntry2::EXT4_FT_SOCK,
        }
    }

    const fn payload(self) -> CreateInodePayload<'static> {
        match self {
            Self::CharacterDevice(device) | Self::BlockDevice(device) => {
                CreateInodePayload::Device(device)
            }
            Self::Fifo | Self::Socket => CreateInodePayload::Empty,
        }
    }
}

impl DirectoryEntryType {
    fn from_disk(value: u8) -> Ext4Result<Self> {
        match value {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::RegularFile),
            2 => Ok(Self::Directory),
            3 => Ok(Self::CharacterDevice),
            4 => Ok(Self::BlockDevice),
            5 => Ok(Self::Fifo),
            6 => Ok(Self::Socket),
            7 => Ok(Self::Symlink),
            _ => Err(Ext4Error::corrupted().with_operation("directory:file_type")),
        }
    }
}

/// One directory record returned by [`Ext4::read_directory`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub inode: InodeNumber,
    pub file_type: DirectoryEntryType,
    pub name: Vec<u8>,
    /// Byte offset of the next record, suitable as the next readdir cookie.
    pub next_offset: u64,
}

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

    const fn masked_by(self, umask: u16) -> u16 {
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

/// Mounted ext4 instance that owns its device, caches, journal, and services.
///
/// The representation is private. The embedding OS serializes access to this
/// value with a sleepable lock when sharing a mount between tasks.
pub struct Ext4<D: BlockIo, S> {
    filesystem: Ext4FileSystem,
    device: Jbd2Dev<D>,
    services: S,
    options: MountOptions,
}

/// Formats a device with an OS-independent clock and returns device ownership.
pub fn format<D, C>(device: D, clock: C, options: MkfsOptions) -> Ext4Result<D>
where
    D: BlockIo,
    C: Clock + Send + 'static,
{
    let mut device = Jbd2Dev::with_clock(0, device, clock, true);
    mkfs_with_options(&mut device, options)?;
    Ok(device.into_inner())
}

impl<D, E, P, K, O> Ext4<D, MountedServices<E, P, K, O>>
where
    D: BlockIo,
    O: Observer,
{
    /// Mounts an ext4 filesystem and transfers ownership of all capabilities.
    pub fn mount<C>(
        device: D,
        services: MountServices<C, E, P, K, O>,
        options: MountOptions,
    ) -> Ext4Result<Self>
    where
        C: Clock + Send + 'static,
    {
        let MountServices {
            clock,
            entropy,
            crypto,
            keys,
            mut observer,
        } = services;
        let mut device = Jbd2Dev::with_clock(0, device, clock, true);
        let filesystem =
            Ext4FileSystem::mount_with_options_and_observer(&mut device, options, &mut observer)?;
        Ok(Self {
            filesystem,
            device,
            services: MountedServices::new(entropy, crypto, keys, observer),
            options,
        })
    }

    /// Selects read-only replay before mounting when the on-disk filesystem
    /// has recorded errors; otherwise performs one read-write mount attempt.
    ///
    /// A failed mount is never retried through the same journal/cache owner.
    /// Replay may already have updated home blocks or latched an abort, so a
    /// second attempt would lose the first error and observe polluted state.
    pub fn mount_with_readonly_fallback<C>(
        device: D,
        services: MountServices<C, E, P, K, O>,
    ) -> Ext4Result<Self>
    where
        C: Clock + Send + 'static,
    {
        let MountServices {
            clock,
            entropy,
            crypto,
            keys,
            mut observer,
        } = services;
        let mut device = Jbd2Dev::with_clock(0, device, clock, true);
        let read_write = MountOptions::read_write();
        let read_only_replay = MountOptions {
            readonly: true,
            replay_journal: true,
            block_validity: true,
        };
        let options = if Ext4FileSystem::device_has_error_state(&mut device)? {
            read_only_replay
        } else {
            read_write
        };
        let filesystem =
            Ext4FileSystem::mount_with_options_and_observer(&mut device, options, &mut observer)?;

        Ok(Self {
            filesystem,
            device,
            services: MountedServices::new(entropy, crypto, keys, observer),
            options,
        })
    }
}

impl<D: BlockIo, E, P, K, O: Observer> Ext4<D, MountedServices<E, P, K, O>> {
    pub const fn options(&self) -> MountOptions {
        self.options
    }

    /// Applies mount options without releasing device or journal ownership.
    pub fn remount(&mut self, options: MountOptions) -> Ext4Result<()> {
        if !self.filesystem.mounted {
            return Err(Ext4Error::busy().with_operation("remount:unmounted"));
        }
        if options.replay_journal != self.options.replay_journal {
            return Err(Ext4Error::unsupported().with_operation("remount:replay_policy"));
        }

        let previous_options = self.options;
        self.filesystem
            .set_block_validity(&mut self.device, options.block_validity)?;
        let mode_result = match (previous_options.readonly, options.readonly) {
            (false, true) => self.filesystem.remount_read_only(&mut self.device),
            (true, false) => self
                .filesystem
                .remount_read_write(&mut self.device, &mut self.services.observer),
            _ => Ok(()),
        };
        if let Err(error) = mode_result {
            let rollback = self
                .filesystem
                .set_block_validity(&mut self.device, previous_options.block_validity);
            return Err(error_after_cleanup(error, rollback));
        }
        self.options = options;
        Ok(())
    }

    pub fn root_inode(&self) -> InodeNumber {
        self.filesystem.root_inode
    }

    pub fn statfs(&self) -> FileSystemStats {
        self.filesystem.statfs()
    }

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
        _context: MutationContext,
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
        _context: MutationContext,
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
        _context: MutationContext,
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

    /// Looks up one raw child name without performing path traversal.
    pub fn lookup_child(
        &mut self,
        parent: InodeNumber,
        name: FileName<'_>,
    ) -> Ext4Result<Option<InodeInfo>> {
        let parent_inode = self.filesystem.get_inode_by_num(&mut self.device, parent)?;
        let entry = match find_named_entry_in_parent(
            &mut self.filesystem,
            &mut self.device,
            parent,
            &parent_inode,
            name.as_bytes(),
        ) {
            Ok(entry) => entry,
            Err(error) if error.kind() == Ext4ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let inode = self
            .filesystem
            .get_inode_by_num(&mut self.device, entry.ino)?;
        self.inspect_inode(entry.ino, inode).map(Some)
    }

    /// Reads directory records from an ext4 byte offset.
    ///
    /// Deleted records and checksum tails advance the cookie but are not
    /// returned. Malformed records and checksum mismatches are corruption,
    /// never an implicit end-of-directory.
    pub fn read_directory(
        &mut self,
        directory: InodeNumber,
        offset: u64,
        max_entries: usize,
    ) -> Ext4Result<Vec<DirectoryEntry>> {
        let mut inode = self
            .filesystem
            .get_inode_by_num(&mut self.device, directory)?;
        if !inode.is_dir() {
            return Err(Ext4Error::not_dir());
        }
        if max_entries == 0 || offset >= inode.size() {
            return Ok(Vec::new());
        }

        let block_size = self.filesystem.block_size();
        let mappings = resolve_inode_blocks(
            &mut self.filesystem,
            &mut self.device,
            directory,
            &mut inode,
        )?;
        let mut output = Vec::new();
        for (logical_block, physical_block) in mappings {
            let block_base = u64::from(logical_block)
                .checked_mul(block_size as u64)
                .ok_or_else(Ext4Error::overflow)?;
            let block_end = block_base
                .checked_add(block_size as u64)
                .ok_or_else(Ext4Error::overflow)?;
            if block_end <= offset {
                continue;
            }

            let cached = self
                .filesystem
                .datablock_cache
                .get_or_load(&mut self.device, physical_block)?;
            let data = &cached.data;
            let checksum_ok = if inode.is_htree_indexed() {
                verify_ext4_dx_checksum(
                    &self.filesystem.superblock,
                    directory.raw(),
                    inode.i_generation,
                    data,
                )
                .unwrap_or_else(|| {
                    verify_ext4_dirblock_checksum(
                        &self.filesystem.superblock,
                        directory.raw(),
                        inode.i_generation,
                        data,
                    )
                })
            } else {
                verify_ext4_dirblock_checksum(
                    &self.filesystem.superblock,
                    directory.raw(),
                    inode.i_generation,
                    data,
                )
            };
            if !checksum_ok {
                return Err(Ext4Error::checksum().with_operation("directory:block"));
            }

            let mut position = 0usize;
            while position < data.len() {
                let header = data.get(position..position + 8).ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("directory:record_header")
                })?;
                let inode_raw = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
                let record_len = usize::from(u16::from_le_bytes([header[4], header[5]]));
                let name_len = usize::from(header[6]);
                let file_type = header[7];
                let record_end = position.checked_add(record_len).ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("directory:record_overflow")
                })?;
                if record_len < 8
                    || !record_len.is_multiple_of(4)
                    || record_end > data.len()
                    || name_len > record_len - 8
                {
                    return Err(Ext4Error::corrupted().with_operation("directory:record"));
                }

                let entry_offset = block_base
                    .checked_add(position as u64)
                    .ok_or_else(Ext4Error::overflow)?;
                let next_offset = block_base
                    .checked_add(record_end as u64)
                    .ok_or_else(Ext4Error::overflow)?;
                if inode_raw != 0 && entry_offset >= offset {
                    let inode_number = InodeNumber::new(inode_raw)
                        .map_err(|_| Ext4Error::corrupted().with_operation("directory:inode"))?;
                    let name = data[position + 8..position + 8 + name_len].to_vec();
                    FileName::new(&name).map_err(|_| {
                        Ext4Error::corrupted().with_operation("directory:stored_name")
                    })?;
                    output.push(DirectoryEntry {
                        inode: inode_number,
                        file_type: DirectoryEntryType::from_disk(file_type)?,
                        name,
                        next_offset,
                    });
                    if output.len() == max_entries {
                        return Ok(output);
                    }
                }
                position = record_end;
            }
        }
        Ok(output)
    }

    /// Creates an empty regular file below an already resolved directory.
    pub fn create_regular_file(
        &mut self,
        context: MutationContext,
        parent: InodeNumber,
        name: FileName<'_>,
        permissions: FilePermissions,
    ) -> Ext4Result<InodeInfo> {
        self.ensure_writable("inode:create")?;
        create_inode_at(
            &mut self.device,
            &mut self.filesystem,
            CreateEntryRequest {
                parent,
                name,
                mode: Ext4Inode::S_IFREG | permissions.masked_by(context.umask),
                uid: context.uid,
                gid: context.gid,
            },
            CreateInodePayload::Empty,
            Ext4DirEntry2::EXT4_FT_REG_FILE,
        )?;
        self.lookup_child(parent, name)?.ok_or_else(|| {
            Ext4Error::corrupted().with_operation("inode:create_missing_directory_entry")
        })
    }

    /// Creates a character device, block device, FIFO, or socket inode.
    pub fn create_special_inode(
        &mut self,
        context: MutationContext,
        parent: InodeNumber,
        name: FileName<'_>,
        permissions: FilePermissions,
        kind: SpecialInodeKind,
    ) -> Ext4Result<InodeInfo> {
        self.ensure_writable("inode:create_special")?;
        create_inode_at(
            &mut self.device,
            &mut self.filesystem,
            CreateEntryRequest {
                parent,
                name,
                mode: kind.inode_type() | permissions.masked_by(context.umask),
                uid: context.uid,
                gid: context.gid,
            },
            kind.payload(),
            kind.directory_entry_type(),
        )?;
        self.lookup_child(parent, name)?.ok_or_else(|| {
            Ext4Error::corrupted().with_operation("inode:create_missing_special_entry")
        })
    }

    /// Creates a symbolic link below a resolved directory.
    pub fn create_symlink(
        &mut self,
        context: MutationContext,
        parent: InodeNumber,
        name: FileName<'_>,
        target: &[u8],
    ) -> Ext4Result<InodeInfo> {
        self.ensure_writable("symlink:create")?;
        create_inode_at(
            &mut self.device,
            &mut self.filesystem,
            CreateEntryRequest {
                parent,
                name,
                mode: Ext4Inode::S_IFLNK | 0o777,
                uid: context.uid,
                gid: context.gid,
            },
            CreateInodePayload::Data(target),
            Ext4DirEntry2::EXT4_FT_SYMLINK,
        )?;
        self.lookup_child(parent, name)?.ok_or_else(|| {
            Ext4Error::corrupted().with_operation("symlink:create_missing_directory_entry")
        })
    }

    /// Creates a directory below an already resolved directory.
    pub fn create_directory(
        &mut self,
        context: MutationContext,
        parent: InodeNumber,
        name: FileName<'_>,
        permissions: FilePermissions,
    ) -> Ext4Result<InodeInfo> {
        self.ensure_writable("directory:create")?;
        create_directory_at(
            &mut self.device,
            &mut self.filesystem,
            CreateEntryRequest {
                parent,
                name,
                mode: Ext4Inode::S_IFDIR | permissions.masked_by(context.umask),
                uid: context.uid,
                gid: context.gid,
            },
        )?;
        self.lookup_child(parent, name)?.ok_or_else(|| {
            Ext4Error::corrupted().with_operation("directory:create_missing_directory_entry")
        })
    }

    /// Adds a hard link to a non-directory inode.
    pub fn hard_link(
        &mut self,
        _context: MutationContext,
        target: InodeNumber,
        parent: InodeNumber,
        name: FileName<'_>,
    ) -> Ext4Result<InodeInfo> {
        self.ensure_writable("inode:link")?;
        link_inode_at(
            &mut self.filesystem,
            &mut self.device,
            LinkEntryRequest {
                parent,
                name,
                target,
            },
        )?;
        self.lookup_child(parent, name)?
            .ok_or_else(|| Ext4Error::corrupted().with_operation("link:missing_directory_entry"))
    }

    /// Removes one non-directory name without reclaiming a final zero-link
    /// inode that may still be referenced by the embedding VFS.
    pub fn unlink(
        &mut self,
        _context: MutationContext,
        parent: InodeNumber,
        name: FileName<'_>,
    ) -> Ext4Result<UnlinkOutcome> {
        self.ensure_writable("inode:unlink")?;
        unlink_inode_at(&mut self.filesystem, &mut self.device, parent, name)
    }

    /// Removes an empty directory without reclaiming its inode while the VFS
    /// may still hold a live directory reference.
    pub fn remove_empty_directory(
        &mut self,
        _context: MutationContext,
        parent: InodeNumber,
        name: FileName<'_>,
    ) -> Ext4Result<UnlinkOutcome> {
        self.ensure_writable("directory:remove")?;
        unlink_empty_directory_at(&mut self.filesystem, &mut self.device, parent, name)
    }

    /// Renames or exchanges two raw directory names below resolved parents.
    pub fn rename(
        &mut self,
        _context: MutationContext,
        old_parent: InodeNumber,
        old_name: FileName<'_>,
        new_parent: InodeNumber,
        new_name: FileName<'_>,
        options: RenameOptions,
    ) -> Ext4Result<RenameOutcome> {
        self.ensure_writable("inode:rename")?;
        rename_inode_at(
            &mut self.filesystem,
            &mut self.device,
            RenameEntryRequest {
                old_parent,
                old_name,
                new_parent,
                new_name,
                options,
            },
        )
    }

    /// Reclaims an orphaned zero-link inode after the VFS releases its final
    /// live reference.
    pub fn reap_unlinked_inode(&mut self, inode: InodeNumber) -> Ext4Result<()> {
        self.ensure_writable("inode:reap_unlinked")?;
        reap_unlinked_inode(&mut self.filesystem, &mut self.device, inode)
    }

    fn inspect_inode(&self, number: InodeNumber, inode: Ext4Inode) -> Ext4Result<InodeInfo> {
        let block_size = self.filesystem.superblock.block_size() as u32;
        let huge_file = self.filesystem.superblock.has_feature_ro_compat(
            crate::superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE,
        );
        Ok(InodeInfo {
            number,
            mode: inode.i_mode,
            uid: inode.uid(),
            gid: inode.gid(),
            links: inode.i_links_count,
            size: inode.size(),
            blocks: inode.blocks_count(block_size, huge_file),
            atime: inode.i_atime,
            ctime: inode.i_ctime,
            mtime: inode.i_mtime,
            btime: inode.i_crtime,
            project_id: inode.i_projid,
            flags: InodeFlags::from_bits_retain(inode.i_flags & Ext4Inode::EXT4_FL_USER_VISIBLE),
            device_number: inode.device_number()?,
        })
    }

    pub fn read_inode(
        &mut self,
        number: InodeNumber,
        offset: u64,
        output: &mut [u8],
    ) -> Ext4Result<usize> {
        read_inode_data_into(
            &mut self.device,
            &mut self.filesystem,
            number,
            offset,
            output,
        )
    }

    pub fn write_inode(
        &mut self,
        _context: MutationContext,
        number: InodeNumber,
        offset: u64,
        input: &[u8],
    ) -> Ext4Result<()> {
        self.ensure_writable("inode:write")?;
        write_inode_data(
            &mut self.device,
            &mut self.filesystem,
            number,
            offset,
            input,
        )
    }

    pub fn truncate_inode(
        &mut self,
        _context: MutationContext,
        number: InodeNumber,
        size: u64,
    ) -> Ext4Result<()> {
        self.ensure_writable("inode:truncate")?;
        truncate_inode(&mut self.device, &mut self.filesystem, number, size)
    }

    pub fn preallocate_inode(
        &mut self,
        _context: MutationContext,
        number: InodeNumber,
        offset: u64,
        len: u64,
        options: PreallocationOptions,
    ) -> Ext4Result<()> {
        self.operate_inode_range(
            _context,
            number,
            offset,
            len,
            RangeOperation::Allocate(options),
        )
    }

    /// Applies one allocation or mapping operation to a byte range.
    pub fn operate_inode_range(
        &mut self,
        _context: MutationContext,
        number: InodeNumber,
        offset: u64,
        len: u64,
        operation: RangeOperation,
    ) -> Ext4Result<()> {
        self.ensure_writable("inode:operate_range")?;
        operate_inode_range(
            &mut self.device,
            &mut self.filesystem,
            number,
            offset,
            len,
            operation,
        )
    }

    /// Replaces the target bytes of an existing symbolic-link inode.
    pub fn set_symlink_target(
        &mut self,
        _context: MutationContext,
        number: InodeNumber,
        target: &[u8],
    ) -> Ext4Result<()> {
        self.ensure_writable("symlink:set_target")?;
        let mut old_inode = self.filesystem.get_inode_by_num(&mut self.device, number)?;
        if !old_inode.is_symlink() {
            return Err(Ext4Error::invalid_input().with_operation("symlink:not_symlink"));
        }
        let old_blocks = resolve_inode_blocks(
            &mut self.filesystem,
            &mut self.device,
            number,
            &mut old_inode,
        )?;

        let target_len = target.len();
        let mut new_inode = old_inode;
        new_inode.i_size_lo = (target_len as u64 & 0xffff_ffff) as u32;
        new_inode.i_size_high = ((target_len as u64) >> 32) as u32;
        new_inode.i_blocks_lo = 0;
        new_inode.l_i_blocks_high = 0;
        new_inode.i_block = [0; 15];
        let mut new_data_blocks = Vec::new();

        if target_len < 60 {
            new_inode.i_flags &= !Ext4Inode::EXT4_EXTENTS_FL;
            let mut inline = [0u8; 60];
            inline[..target_len].copy_from_slice(target);
            for (word, bytes) in new_inode.i_block.iter_mut().zip(inline.as_chunks::<4>().0) {
                *word = u32::from_le_bytes(*bytes);
            }
        } else {
            let block_size = self.filesystem.block_size();
            let storage_len = target_len.checked_add(1).ok_or_else(Ext4Error::overflow)?;
            let mut remaining = storage_len;
            let mut source_offset = 0usize;
            while remaining != 0 {
                if !self.filesystem.superblock.has_extents() && new_data_blocks.len() >= 12 {
                    let cleanup = discard_unpublished_inode_blocks(
                        &mut self.filesystem,
                        &mut self.device,
                        &new_data_blocks,
                    );
                    return Err(error_after_cleanup(
                        Ext4Error::unsupported().with_operation("symlink:legacy_indirect"),
                        cleanup,
                    ));
                }
                let block = match self.filesystem.alloc_block(&mut self.device) {
                    Ok(block) => block,
                    Err(error) => {
                        let cleanup = discard_unpublished_inode_blocks(
                            &mut self.filesystem,
                            &mut self.device,
                            &new_data_blocks,
                        );
                        return Err(error_after_cleanup(error, cleanup));
                    }
                };
                let write_len = core::cmp::min(remaining, block_size);
                if let Err(error) =
                    self.filesystem
                        .datablock_cache
                        .modify_new(&mut self.device, block, |data| {
                            data.fill(0);
                            let copy_len =
                                core::cmp::min(write_len, target_len.saturating_sub(source_offset));
                            let source_end = source_offset + copy_len;
                            data[..copy_len].copy_from_slice(&target[source_offset..source_end]);
                        })
                {
                    self.filesystem.datablock_cache.invalidate(block);
                    new_data_blocks.push(block);
                    let cleanup = discard_unpublished_inode_blocks(
                        &mut self.filesystem,
                        &mut self.device,
                        &new_data_blocks,
                    );
                    return Err(error_after_cleanup(error, cleanup));
                }
                new_data_blocks.push(block);
                source_offset = source_offset.saturating_add(write_len);
                remaining -= write_len;
            }

            if self.filesystem.superblock.has_extents() {
                new_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
                new_inode.write_extend_header();
            } else {
                new_inode.i_flags &= !Ext4Inode::EXT4_EXTENTS_FL;
            }
            let sectors = match (new_data_blocks.len() as u64).checked_mul(block_size as u64 / 512)
            {
                Some(sectors) => sectors,
                None => {
                    let cleanup = discard_unpublished_inode_blocks(
                        &mut self.filesystem,
                        &mut self.device,
                        &new_data_blocks,
                    );
                    return Err(error_after_cleanup(Ext4Error::overflow(), cleanup));
                }
            };
            let huge_file = self.filesystem.superblock.has_feature_ro_compat(
                crate::superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE,
            );
            if let Err(error) = new_inode.set_blocks_count(sectors, block_size as u32, huge_file) {
                let cleanup = discard_unpublished_inode_blocks(
                    &mut self.filesystem,
                    &mut self.device,
                    &new_data_blocks,
                );
                return Err(error_after_cleanup(error, cleanup));
            }
            if let Err(error) = build_file_block_mapping_with_inode_num(
                &mut self.filesystem,
                &mut new_inode,
                number,
                &new_data_blocks,
                &mut self.device,
            ) {
                let cleanup = discard_unpublished_inode_blocks(
                    &mut self.filesystem,
                    &mut self.device,
                    &new_data_blocks,
                );
                return Err(error_after_cleanup(error, cleanup));
            }
        }

        if let Err(error) = self.filesystem.finalize_inode_update(
            &mut self.device,
            number,
            &mut new_inode,
            Ext4InodeMetadataUpdate::write_access(),
        ) {
            let cleanup = discard_unpublished_inode_blocks(
                &mut self.filesystem,
                &mut self.device,
                &new_data_blocks,
            );
            return Err(error_after_cleanup(error, cleanup));
        }
        for block in old_blocks.into_values() {
            self.filesystem.datablock_cache.invalidate(block);
            self.filesystem.free_block(&mut self.device, block)?;
        }
        Ok(())
    }

    pub fn sync(&mut self) -> Ext4Result<()> {
        if self.options.readonly {
            return self.device.flush();
        }
        self.filesystem
            .sync_filesystem_with_observer(&mut self.device, &mut self.services.observer)
    }

    pub fn unmount(&mut self) -> Ext4Result<()> {
        if self.options.readonly {
            self.filesystem
                .finish_read_only_unmount(&mut self.services.observer);
            return Ok(());
        }
        self.filesystem
            .umount_with_observer(&mut self.device, &mut self.services.observer)
    }

    fn ensure_writable(&self, operation: &'static str) -> Ext4Result<()> {
        if self.options.readonly {
            Err(Ext4Error::read_only().with_operation(operation))
        } else {
            Ok(())
        }
    }
}
