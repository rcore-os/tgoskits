//! Directory parsers consume the shared independent inode block reader.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

use super::*;
use crate::{
    bmalloc::AbsoluteBN,
    dir::DirectoryBlockRead,
    ext4::owned::block_read::{IndependentBlockRead, InodeBlockKind},
    loopfile::{resolve_inode_block_with_reader, resolve_inode_blocks_with_reader},
    superblock::Ext4Superblock,
};

pub(super) struct IndependentDirectoryRead<'a, D: BlockIo, C> {
    blocks: IndependentBlockRead<'a, D, C>,
}

impl<'a, D: BlockIo, C: DirectoryReadCache> IndependentDirectoryRead<'a, D, C> {
    pub(super) fn new(blocks: IndependentBlockRead<'a, D, C>) -> Self {
        Self { blocks }
    }
}

impl<D: BlockIo, C: DirectoryReadCache> DirectoryBlockRead for IndependentDirectoryRead<'_, D, C> {
    fn directory(&self) -> InodeNumber {
        self.blocks.snapshot().number
    }

    fn inode(&self) -> &Ext4Inode {
        &self.blocks.snapshot().inode
    }

    fn superblock(&self) -> &Ext4Superblock {
        &self.blocks.snapshot().superblock
    }

    fn map_block(&mut self, logical: u32) -> Ext4Result<Option<AbsoluteBN>> {
        let snapshot = self.blocks.snapshot().clone();
        let mut inode = snapshot.inode;
        resolve_inode_block_with_reader(
            snapshot.context(),
            &mut self.blocks,
            snapshot.number,
            &mut inode,
            logical,
        )
    }

    fn mapped_blocks(&mut self) -> Ext4Result<BTreeMap<u32, AbsoluteBN>> {
        let snapshot = self.blocks.snapshot().clone();
        let mut inode = snapshot.inode;
        resolve_inode_blocks_with_reader(
            snapshot.context(),
            &mut self.blocks,
            snapshot.number,
            &mut inode,
        )
    }

    fn read_block(&mut self, physical: AbsoluteBN) -> Ext4Result<Arc<Vec<u8>>> {
        self.blocks.block_image(physical, InodeBlockKind::Data)
    }
}

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    /// Compatibility entry for a protected directory reader's block visibility.
    /// The same mounted inode-view rules apply to both data and mapping blocks.
    pub fn directory_block_image(
        &mut self,
        request: &DirectoryBlockRequest,
    ) -> Ext4Result<Option<Arc<Vec<u8>>>> {
        self.inode_read_block_image(request)
    }
}
