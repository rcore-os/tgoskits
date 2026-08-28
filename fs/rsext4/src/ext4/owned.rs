//! Owned, OS-independent mounted filesystem boundary.

use alloc::{collections::VecDeque, vec::Vec};

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
        XattrSetMode, create_inode_at, error_after_cleanup, find_named_entry_in_parent,
        get_inode_xattr, inspect_inode_extents, link_inode_at, list_inode_xattrs,
        operate_inode_range, read_inode_data_into, reap_unlinked_inode, remove_inode_xattr,
        rename_inode_at, set_inode_xattr, truncate_inode, unlink_empty_directory_at,
        unlink_inode_at, write_inode_data,
    },
    hashtree::{
        Ext4InodeHashTreeExt, IndexedDirectoryRange, IndexedDirectoryRecord,
        read_indexed_directory_range,
    },
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
    /// Cursor of the next record.
    pub next_cursor: DirectoryCursor,
}

/// Opaque core cursor used to resume directory enumeration.
///
/// Linear directories use byte offsets. Indexed directories use the complete
/// ext4 hash plus a collision ordinal that is deliberately not compressed into
/// a Linux ABI cookie by the OS-independent core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryCursor {
    /// Begin enumeration from the first visible record.
    Start,
    /// Resume a linear directory at an on-disk byte offset.
    Linear { offset: u64 },
    /// Resume an indexed directory at a hash and exact-collision ordinal.
    HTree {
        major: u32,
        minor: u32,
        collision: u32,
    },
    /// Enumeration has reached end of directory.
    End,
}

/// Per-open directory state owned by an embedding VFS open description.
///
/// The representation stays private so HTree paths and cached directory
/// records never become part of the portable core API. The caller-provided
/// [`DirectoryCursor`] remains the authoritative position: this state is a
/// discardable acceleration cache and does not need transactional rollback
/// when an I/O or copy-to-user operation fails.
#[derive(Debug)]
pub struct DirectoryReader {
    directory: InodeNumber,
    indexed: Option<IndexedDirectoryReader>,
}

#[derive(Debug)]
struct IndexedDirectoryReader {
    change_attribute: u64,
    ranges: VecDeque<IndexedDirectoryRange>,
}

impl DirectoryReader {
    fn new(directory: InodeNumber) -> Self {
        Self {
            directory,
            indexed: None,
        }
    }

    pub const fn directory(&self) -> InodeNumber {
        self.directory
    }

    /// Discards parsed HTree ranges after an external seek or policy change.
    pub fn reset(&mut self) {
        self.indexed = None;
    }
}

fn indexed_cursor_key(cursor: DirectoryCursor) -> Ext4Result<(u32, u32, u32)> {
    match cursor {
        DirectoryCursor::Start => Ok((0, 0, 0)),
        DirectoryCursor::HTree {
            major,
            minor,
            collision,
        } if major & 1 == 0 => Ok((major, minor, collision)),
        DirectoryCursor::HTree { .. } => {
            Err(Ext4Error::invalid_input().with_operation("directory:indexed_hash_cursor"))
        }
        DirectoryCursor::Linear { .. } => {
            Err(Ext4Error::invalid_input().with_operation("directory:indexed_cursor"))
        }
        DirectoryCursor::End => {
            Err(Ext4Error::invalid_input().with_operation("directory:indexed_end_cursor"))
        }
    }
}

const fn indexed_record_key(record: &IndexedDirectoryRecord) -> (u32, u32, u32) {
    (record.major, record.minor, record.collision)
}

const fn indexed_key_cursor((major, minor, collision): (u32, u32, u32)) -> DirectoryCursor {
    DirectoryCursor::HTree {
        major,
        minor,
        collision,
    }
}

fn indexed_range_position(range: &IndexedDirectoryRange, cursor: (u32, u32, u32)) -> Option<usize> {
    if range.start == cursor {
        return Some(0);
    }
    range
        .records
        .iter()
        .position(|record| indexed_record_key(record) == cursor)
}

fn indexed_record_count(ranges: &VecDeque<IndexedDirectoryRange>, first_record: usize) -> usize {
    ranges
        .iter()
        .enumerate()
        .map(|(index, range)| {
            if index == 0 {
                range.records.len().saturating_sub(first_record)
            } else {
                range.records.len()
            }
        })
        .sum()
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

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    /// Mounts an ext4 filesystem and transfers ownership of all capabilities.
    pub fn mount<C>(
        device: D,
        services: MountServices<C, E, O, W>,
        options: MountOptions,
    ) -> Ext4Result<Self>
    where
        C: Clock + Send + 'static,
    {
        Self::mount_selecting_options(device, services, |_| Ok(options))
    }

    /// Selects read-only replay before mounting when the on-disk filesystem
    /// has recorded errors; otherwise performs one read-write mount attempt.
    ///
    /// A failed mount is never retried through the same journal/cache owner.
    /// Replay may already have updated home blocks or latched an abort, so a
    /// second attempt would lose the first error and observe polluted state.
    pub fn mount_with_readonly_fallback<C>(
        device: D,
        services: MountServices<C, E, O, W>,
    ) -> Ext4Result<Self>
    where
        C: Clock + Send + 'static,
    {
        Self::mount_selecting_options(device, services, |device| {
            let read_write = MountOptions::read_write();
            let read_only_replay = MountOptions {
                readonly: true,
                replay_journal: true,
                block_validity: true,
            };
            if Ext4FileSystem::device_has_error_state(device)? {
                Ok(read_only_replay)
            } else {
                Ok(read_write)
            }
        })
    }

    fn mount_selecting_options<C, F>(
        device: D,
        services: MountServices<C, E, O, W>,
        select_options: F,
    ) -> Ext4Result<Self>
    where
        C: Clock + Send + 'static,
        F: FnOnce(&mut Jbd2Dev<D>) -> Ext4Result<MountOptions>,
    {
        let MountServices {
            clock,
            mut entropy,
            mut observer,
            mut mmp_delay,
            mmp_identity,
        } = services;
        let mut device = Jbd2Dev::with_clock(0, device, clock, true);
        let options = select_options(&mut device)?;
        let filesystem = Ext4FileSystem::mount_with_services(
            &mut device,
            options,
            &mut observer,
            &mut entropy,
            &mut mmp_delay,
            mmp_identity,
        )?;

        Ok(Self {
            filesystem,
            device,
            services: MountedServices::new(entropy, observer, mmp_delay, mmp_identity),
            options,
        })
    }
}

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
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
            (false, true) => self.remount_read_only(),
            (true, false) => self.remount_read_write(),
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

    /// Reads directory records from a core-owned cursor.
    ///
    /// Deleted records and checksum tails advance the cookie but are not
    /// returned. Malformed records and checksum mismatches are corruption,
    /// never an implicit end-of-directory.
    pub fn read_directory(
        &mut self,
        directory: InodeNumber,
        cursor: DirectoryCursor,
        max_entries: usize,
    ) -> Ext4Result<Vec<DirectoryEntry>> {
        let mut reader = DirectoryReader::new(directory);
        self.read_directory_with_reader(&mut reader, cursor, max_entries)
    }

    /// Opens private directory-enumeration state for one VFS open description.
    pub fn open_directory_reader(&mut self, directory: InodeNumber) -> Ext4Result<DirectoryReader> {
        let inode = self
            .filesystem
            .get_inode_by_num(&mut self.device, directory)?;
        if !inode.is_dir() {
            return Err(Ext4Error::not_dir());
        }
        Ok(DirectoryReader::new(directory))
    }

    /// Reads directory records while retaining discardable per-open HTree
    /// range state.
    ///
    /// `cursor` is the only authoritative enumeration position. Callers may
    /// retry the same cursor after an error even if this method populated or
    /// discarded cached ranges before returning the error.
    pub fn read_directory_with_reader(
        &mut self,
        reader: &mut DirectoryReader,
        cursor: DirectoryCursor,
        max_entries: usize,
    ) -> Ext4Result<Vec<DirectoryEntry>> {
        let directory = reader.directory;
        let mut inode = self
            .filesystem
            .get_inode_by_num(&mut self.device, directory)?;
        if !inode.is_dir() {
            return Err(Ext4Error::not_dir());
        }
        if max_entries == 0 || cursor == DirectoryCursor::End {
            return Ok(Vec::new());
        }
        if inode.is_htree_indexed() {
            return self.read_indexed_directory_with_reader(reader, &inode, cursor, max_entries);
        }
        reader.indexed = None;

        let offset = match cursor {
            DirectoryCursor::Start => 0,
            DirectoryCursor::Linear { offset } => offset,
            DirectoryCursor::HTree { .. } => {
                return Err(Ext4Error::invalid_input().with_operation("directory:linear_cursor"));
            }
            DirectoryCursor::End => return Ok(Vec::new()),
        };
        if offset >= self.filesystem.inode_size(&inode) {
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
                        next_cursor: DirectoryCursor::Linear {
                            offset: next_offset,
                        },
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

    fn read_indexed_directory_with_reader(
        &mut self,
        reader: &mut DirectoryReader,
        inode: &Ext4Inode,
        cursor: DirectoryCursor,
        max_entries: usize,
    ) -> Ext4Result<Vec<DirectoryEntry>> {
        let start = indexed_cursor_key(cursor)?;
        let change_attribute = inode.version(self.filesystem.inode_disk_size());
        match &mut reader.indexed {
            Some(indexed) if indexed.change_attribute == change_attribute => {}
            Some(indexed) => {
                indexed.change_attribute = change_attribute;
                indexed.ranges.clear();
            }
            None => {
                reader.indexed = Some(IndexedDirectoryReader {
                    change_attribute,
                    ranges: VecDeque::new(),
                });
            }
        }

        let range_index = reader.indexed.as_ref().and_then(|indexed| {
            indexed
                .ranges
                .iter()
                .position(|range| indexed_range_position(range, start).is_some())
        });
        let range_index = match range_index {
            Some(index) => index,
            None => {
                let range = self.load_indexed_directory_range(reader.directory, inode, start)?;
                let indexed = reader.indexed.as_mut().ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("directory:reader_state")
                })?;
                indexed.ranges.clear();
                indexed.ranges.push_back(range);
                0
            }
        };
        let indexed = reader
            .indexed
            .as_mut()
            .ok_or_else(|| Ext4Error::corrupted().with_operation("directory:reader_state"))?;
        for _ in 0..range_index {
            let _ = indexed.ranges.pop_front();
        }

        let record_index = indexed
            .ranges
            .front()
            .and_then(|range| indexed_range_position(range, start))
            .ok_or_else(|| Ext4Error::corrupted().with_operation("directory:reader_cursor"))?;
        let lookahead = max_entries.checked_add(1).unwrap_or(max_entries);
        while indexed_record_count(&indexed.ranges, record_index) < lookahead {
            let Some(next_start) = indexed.ranges.back().and_then(|range| range.next_start) else {
                break;
            };
            let previous_start = indexed
                .ranges
                .back()
                .map(|range| range.start)
                .ok_or_else(|| Ext4Error::corrupted().with_operation("directory:reader_range"))?;
            if next_start <= previous_start
                || indexed.ranges.iter().any(|range| range.start == next_start)
            {
                return Err(Ext4Error::corrupted().with_operation("directory:reader_cycle"));
            }
            let range = self.load_indexed_directory_range(reader.directory, inode, next_start)?;
            indexed.ranges.push_back(range);
        }

        let mut records = Vec::with_capacity(lookahead.min(128));
        for (range_index, range) in indexed.ranges.iter().enumerate() {
            let first = if range_index == 0 { record_index } else { 0 };
            for record in range.records.iter().skip(first) {
                records.push(record);
                if records.len() == lookahead {
                    break;
                }
            }
            if records.len() == lookahead {
                break;
            }
        }

        let returned = records.len().min(max_entries);
        let mut output = Vec::with_capacity(returned);
        for index in 0..returned {
            let record = records[index];
            let next_cursor = records
                .get(index + 1)
                .map(|record| indexed_key_cursor(indexed_record_key(record)))
                .unwrap_or(DirectoryCursor::End);
            output.push(DirectoryEntry {
                inode: InodeNumber::new(record.inode).map_err(|_| {
                    Ext4Error::corrupted().with_operation("directory:indexed_inode")
                })?,
                file_type: DirectoryEntryType::from_disk(record.file_type)?,
                name: record.name.clone(),
                next_cursor,
            });
        }
        Ok(output)
    }

    fn load_indexed_directory_range(
        &mut self,
        directory: InodeNumber,
        inode: &Ext4Inode,
        start: (u32, u32, u32),
    ) -> Ext4Result<IndexedDirectoryRange> {
        read_indexed_directory_range(
            &mut self.filesystem,
            &mut self.device,
            directory,
            inode,
            start,
        )
    }

    /// Returns the terminal cursor for directory seek semantics.
    ///
    /// Linear directories use their byte size. HTree directories use an
    /// opaque terminal state so an OS adapter can encode the architecture's
    /// ext4 EOF cookie without leaking ABI policy into the portable core.
    pub fn directory_end_cursor(&mut self, directory: InodeNumber) -> Ext4Result<DirectoryCursor> {
        let inode = self
            .filesystem
            .get_inode_by_num(&mut self.device, directory)?;
        if !inode.is_dir() {
            return Err(Ext4Error::not_dir());
        }
        if inode.is_htree_indexed() {
            Ok(DirectoryCursor::End)
        } else {
            Ok(DirectoryCursor::Linear {
                offset: self.filesystem.inode_size(&inode),
            })
        }
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
    pub fn unlink(&mut self, parent: InodeNumber, name: FileName<'_>) -> Ext4Result<UnlinkOutcome> {
        self.ensure_writable("inode:unlink")?;
        unlink_inode_at(&mut self.filesystem, &mut self.device, parent, name)
    }

    /// Removes an empty directory without reclaiming its inode while the VFS
    /// may still hold a live directory reference.
    pub fn remove_empty_directory(
        &mut self,
        parent: InodeNumber,
        name: FileName<'_>,
    ) -> Ext4Result<UnlinkOutcome> {
        self.ensure_writable("directory:remove")?;
        unlink_empty_directory_at(&mut self.filesystem, &mut self.device, parent, name)
    }

    /// Renames or exchanges two raw directory names below resolved parents.
    pub fn rename(
        &mut self,
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
            size: self.filesystem.inode_size(&inode),
            blocks: inode.blocks_count(block_size, huge_file),
            atime: inode.i_atime,
            ctime: inode.i_ctime,
            mtime: inode.i_mtime,
            btime: inode.i_crtime,
            change_attribute: inode.version(self.filesystem.inode_disk_size()),
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
        self.ensure_mounted("inode:read")?;
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
        truncate_inode(&mut self.device, &mut self.filesystem, number, size)
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
        operate_inode_range(
            &mut self.device,
            &mut self.filesystem,
            number,
            offset,
            len,
            operation,
        )
    }

    pub fn sync(&mut self) -> Ext4Result<()> {
        self.ensure_mounted("sync:unmounted")?;
        if self.options.readonly {
            return self.device.flush();
        }
        self.filesystem.mmp.ensure_writable("sync:mmp_failed")?;
        self.filesystem
            .sync_filesystem_with_observer(&mut self.device, &mut self.services.observer)
    }

    /// Refreshes MMP ownership after the embedding runtime's lock-free wait.
    ///
    /// `elapsed` is the monotonic duration since the previous successful MMP
    /// publication. The caller must not hold this mount's outer lock while
    /// waiting for the returned interval.
    pub fn refresh_mmp(
        &mut self,
        elapsed: core::time::Duration,
    ) -> Ext4Result<Option<core::time::Duration>> {
        if self.options.readonly || !self.filesystem.mmp.is_active() {
            return Ok(None);
        }
        let interval = self.filesystem.mmp.refresh(
            &mut self.device,
            &self.filesystem.superblock,
            self.services.mmp_identity,
            elapsed,
        )?;
        Ok(Some(interval))
    }

    /// Returns the periodic MMP interval without performing I/O.
    pub const fn mmp_refresh_interval(&self) -> Option<core::time::Duration> {
        self.filesystem.mmp.refresh_interval()
    }

    /// Latches loss of the embedding runtime's periodic MMP driver.
    ///
    /// Once reported, all subsequent mutations fail until a new mount owns a
    /// functioning runtime driver.
    pub fn report_mmp_runtime_failure(&mut self, error: Ext4Error) {
        if self.filesystem.mmp.is_active() {
            self.filesystem.mmp.mark_failed(error);
        }
    }

    /// Persists a clean filesystem and then releases writable MMP ownership.
    ///
    /// If the final MMP write fails, the ext4/JBD2 state is already clean and
    /// this mount becomes terminal: further mutations and remounts are
    /// rejected. Retrying an uncertain CLEAN write could overwrite a new MMP
    /// owner that claimed the device after observing the first write.
    pub fn unmount(&mut self) -> Ext4Result<()> {
        self.ensure_mounted("unmount:unmounted")?;
        if self.options.readonly {
            self.filesystem
                .finish_read_only_unmount(&mut self.services.observer);
            return Ok(());
        }
        self.filesystem
            .umount_with_observer(&mut self.device, &mut self.services.observer)?;
        self.release_mmp()
    }

    fn ensure_writable(&self, operation: &'static str) -> Ext4Result<()> {
        self.ensure_mounted(operation)?;
        if self.options.readonly {
            Err(Ext4Error::read_only().with_operation(operation))
        } else {
            self.filesystem.mmp.ensure_writable(operation)
        }
    }

    fn ensure_mounted(&self, operation: &'static str) -> Ext4Result<()> {
        if self.filesystem.mounted {
            Ok(())
        } else {
            // A failed final MMP release is a terminal unmounted state, but
            // the loss of ownership is only the consequence.  Keep reporting
            // the latched I/O failure instead of hiding it behind EBUSY.
            self.filesystem.mmp.ensure_writable(operation)?;
            Err(Ext4Error::busy().with_operation(operation))
        }
    }

    fn remount_read_only(&mut self) -> Ext4Result<()> {
        self.filesystem.remount_read_only(&mut self.device)?;
        match self.release_mmp() {
            Ok(()) => Ok(()),
            Err(error) => self.rollback_read_only_transition(error),
        }
    }

    fn remount_read_write(&mut self) -> Ext4Result<()> {
        self.claim_mmp()?;
        if let Err(error) = self
            .filesystem
            .remount_read_write(&mut self.device, &mut self.services.observer)
        {
            return Err(error_after_cleanup(error, self.release_mmp()));
        }
        if let Err(error) = self.refresh_claimed_mmp() {
            let remount_cleanup = self.filesystem.remount_read_only(&mut self.device);
            let release_cleanup = self.release_mmp();
            let cleanup_error = error_after_cleanup(error, remount_cleanup);
            return Err(error_after_cleanup(cleanup_error, release_cleanup));
        }
        Ok(())
    }

    fn claim_and_refresh_mmp(&mut self) -> Ext4Result<()> {
        self.claim_mmp()?;
        match self.refresh_claimed_mmp() {
            Ok(()) => Ok(()),
            Err(error) => Err(error_after_cleanup(error, self.release_mmp())),
        }
    }

    fn claim_mmp(&mut self) -> Ext4Result<()> {
        self.filesystem.mmp = super::mmp::MmpState::claim(
            &mut self.device,
            &self.filesystem.superblock,
            &mut self.services.entropy,
            &mut self.services.mmp_delay,
        )?;
        Ok(())
    }

    fn refresh_claimed_mmp(&mut self) -> Ext4Result<()> {
        if !self.filesystem.mmp.is_active() {
            return Ok(());
        }
        self.filesystem.mmp.refresh(
            &mut self.device,
            &self.filesystem.superblock,
            self.services.mmp_identity,
            core::time::Duration::ZERO,
        )?;
        Ok(())
    }

    fn release_mmp(&mut self) -> Ext4Result<()> {
        self.filesystem
            .mmp
            .release_clean(&mut self.device, &self.filesystem.superblock)
    }

    fn rollback_read_only_transition(&mut self, operation_error: Ext4Error) -> Ext4Result<()> {
        if let Err(reclaim_error) = self.claim_and_refresh_mmp() {
            self.filesystem.mmp.mark_failed(reclaim_error);
            return Err(reclaim_error);
        }
        if let Err(remount_error) = self
            .filesystem
            .remount_read_write(&mut self.device, &mut self.services.observer)
        {
            self.filesystem.mmp.mark_failed(remount_error);
            return Err(remount_error);
        }
        Err(operation_error)
    }
}
