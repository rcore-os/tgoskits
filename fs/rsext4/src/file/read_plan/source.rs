//! Capability boundary for extent metadata and immutable visible file bytes.

use super::*;
use crate::{blockdev::MetadataBlockRead, disknode::Ext4Inode, ext4::BlockMapContext};

pub(crate) struct FileReadMapping<'a> {
    pub(crate) number: InodeNumber,
    pub(crate) inode: Ext4Inode,
    pub(crate) context: BlockMapContext<'a>,
}

pub(crate) trait FileBlockRead: MetadataBlockRead {
    fn data_images(
        &mut self,
        physical: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<Vec<Option<Arc<Vec<u8>>>>>;
}

pub(super) struct MountedFileRead<'a, B: BlockIo> {
    filesystem: &'a Ext4FileSystem,
    device: &'a mut Jbd2Dev<B>,
}

impl<'a, B: BlockIo> MountedFileRead<'a, B> {
    pub(super) fn new(filesystem: &'a Ext4FileSystem, device: &'a mut Jbd2Dev<B>) -> Self {
        Self { filesystem, device }
    }
}

impl<B: BlockIo> MetadataBlockRead for MountedFileRead<'_, B> {
    fn total_blocks(&self) -> u64 {
        self.device.total_blocks()
    }

    fn with_block<T>(
        &mut self,
        physical: AbsoluteBN,
        inspect: impl FnOnce(&[u8]) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        self.device.with_block(physical, inspect)
    }
}

impl<B: BlockIo> FileBlockRead for MountedFileRead<'_, B> {
    fn data_images(
        &mut self,
        physical: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<Vec<Option<Arc<Vec<u8>>>>> {
        let mut images = Vec::new();
        images
            .try_reserve_exact(count as usize)
            .map_err(|_| Ext4Error::no_memory())?;
        for index in 0..count {
            let block = physical.checked_add(index)?;
            let image = self
                .filesystem
                .datablock_cache
                .get(block)
                .map(|cached| cached.data)
                .or_else(|| {
                    self.device
                        .visible_block_image(block)
                        .map(|bytes| Arc::new(bytes.to_vec()))
                });
            images.push(image);
        }
        Ok(images)
    }
}
