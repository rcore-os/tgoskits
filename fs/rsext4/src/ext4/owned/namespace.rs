//! Atomic directory-entry creation, linking, removal and rename.

use super::*;

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
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
        self.writes.ensure_inode_idle(inode)?;
        reap_unlinked_inode(&mut self.filesystem, &mut self.device, inode)
    }
}
