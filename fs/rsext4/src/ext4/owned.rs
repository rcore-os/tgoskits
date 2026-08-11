//! Owned, OS-independent mounted filesystem boundary.

use super::{Ext4FileSystem, FileSystemStats, MountOptions};
use crate::{
    blockdev::Jbd2Dev,
    bmalloc::InodeNumber,
    disknode::Ext4Inode,
    error::{Ext4Error, Ext4Result},
    file::{read_inode_data_into, truncate_inode, write_inode_data},
    io::BlockIo,
    runtime::{Clock, MountServices, MountedServices, Observer},
};

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
