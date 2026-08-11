//! Owned, OS-independent mounted filesystem boundary.

use alloc::vec::Vec;

use super::{Ext4FileSystem, FileSystemStats, MountOptions};
use crate::{
    blockdev::Jbd2Dev,
    bmalloc::InodeNumber,
    checksum::{verify_ext4_dirblock_checksum, verify_ext4_dx_checksum},
    dir::{CreateEntryRequest, FileName, LinkEntryRequest, create_directory_at},
    disknode::Ext4Inode,
    entries::Ext4DirEntry2,
    error::{Ext4Error, Ext4ErrorKind, Ext4Result},
    file::{
        RenameEntryRequest, RenameOptions, RenameOutcome, UnlinkOutcome, create_inode_at,
        find_named_entry_in_parent, link_inode_at, read_inode_data_into, reap_unlinked_inode,
        rename_inode_at, truncate_inode, unlink_inode_at, write_inode_data,
    },
    hashtree::Ext4InodeHashTreeExt,
    io::BlockIo,
    loopfile::resolve_inode_blocks,
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
}

impl<D: BlockIo, E, P, K, O: Observer> Ext4<D, MountedServices<E, P, K, O>> {
    pub const fn options(&self) -> MountOptions {
        self.options
    }

    pub fn root_inode(&self) -> InodeNumber {
        self.filesystem.root_inode
    }

    pub fn statfs(&self) -> FileSystemStats {
        self.filesystem.statfs()
    }

    pub fn inode(&mut self, number: InodeNumber) -> Ext4Result<InodeInfo> {
        let inode = self.filesystem.get_inode_by_num(&mut self.device, number)?;
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
            None,
            Ext4DirEntry2::EXT4_FT_REG_FILE,
        )?;
        self.lookup_child(parent, name)?.ok_or_else(|| {
            Ext4Error::corrupted().with_operation("inode:create_missing_directory_entry")
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

    pub fn sync(&mut self) -> Ext4Result<()> {
        self.filesystem
            .sync_filesystem_with_observer(&mut self.device, &mut self.services.observer)
    }

    pub fn unmount(&mut self) -> Ext4Result<()> {
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
