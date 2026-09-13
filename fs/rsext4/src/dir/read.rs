//! Read capabilities for one directory incarnation, independent of its owner.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

use crate::{
    Ext4Result,
    blockdev::{BlockIo, Jbd2Dev},
    bmalloc::{AbsoluteBN, InodeNumber},
    disknode::Ext4Inode,
    ext4::Ext4FileSystem,
    loopfile::{resolve_inode_block, resolve_inode_blocks},
    superblock::Ext4Superblock,
};

/// The inode snapshot and filesystem geometry must describe the same mounted
/// directory throughout an algorithm invocation. A reader owns its mapping and
/// block-visibility protocol; directory algorithms cannot mutate a mount,
/// allocate filesystem blocks, or publish a namespace change through it.
///
/// Returned block images are immutable owners, not borrows of a device's
/// reusable scratch buffer. A reader must overlay newer cache/journal bytes
/// before returning them. An independent reader additionally needs a namespace
/// lifetime and validation protocol before its result can be published.
pub(crate) trait DirectoryBlockRead {
    fn directory(&self) -> InodeNumber;
    fn inode(&self) -> &Ext4Inode;
    fn superblock(&self) -> &Ext4Superblock;
    fn map_block(&mut self, logical: u32) -> Ext4Result<Option<AbsoluteBN>>;
    fn mapped_blocks(&mut self) -> Ext4Result<BTreeMap<u32, AbsoluteBN>>;
    fn read_block(&mut self, physical: AbsoluteBN) -> Ext4Result<Arc<Vec<u8>>>;

    fn block_size(&self) -> usize {
        self.superblock().block_size() as usize
    }

    fn inode_size(&self) -> u64 {
        self.inode().size_in_filesystem(
            self.superblock()
                .has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_LARGEDIR),
        )
    }
}

/// Adapter for algorithms running under the existing mounted owner. The inode
/// copy is paired once with its number, rather than passed independently at
/// every HTree level. Exclusive mount access retains the current namespace,
/// mapping, cache, and journal visibility for the lifetime of this adapter.
pub(crate) struct MountedDirectoryRead<'a, B: BlockIo> {
    filesystem: &'a mut Ext4FileSystem,
    device: &'a mut Jbd2Dev<B>,
    directory: InodeNumber,
    inode: Ext4Inode,
}

impl<'a, B: BlockIo> MountedDirectoryRead<'a, B> {
    pub(crate) fn new(
        filesystem: &'a mut Ext4FileSystem,
        device: &'a mut Jbd2Dev<B>,
        directory: InodeNumber,
        inode: Ext4Inode,
    ) -> Self {
        Self {
            filesystem,
            device,
            directory,
            inode,
        }
    }
}

impl<B: BlockIo> DirectoryBlockRead for MountedDirectoryRead<'_, B> {
    fn directory(&self) -> InodeNumber {
        self.directory
    }

    fn inode(&self) -> &Ext4Inode {
        &self.inode
    }

    fn superblock(&self) -> &Ext4Superblock {
        &self.filesystem.superblock
    }

    fn map_block(&mut self, logical: u32) -> Ext4Result<Option<AbsoluteBN>> {
        let mut inode = self.inode;
        resolve_inode_block(
            self.filesystem,
            self.device,
            self.directory,
            &mut inode,
            logical,
        )
    }

    fn mapped_blocks(&mut self) -> Ext4Result<BTreeMap<u32, AbsoluteBN>> {
        let mut inode = self.inode;
        resolve_inode_blocks(self.filesystem, self.device, self.directory, &mut inode)
    }

    fn read_block(&mut self, physical: AbsoluteBN) -> Ext4Result<Arc<Vec<u8>>> {
        self.filesystem
            .datablock_cache
            .get_or_load(self.device, physical)
            .map(|block| block.data)
    }
}
