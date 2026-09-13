//! Read-only metadata access shared by mounted and independent tree walkers.

use super::{BlockIo, Jbd2Dev};
use crate::{Ext4Result, bmalloc::AbsoluteBN};

/// A reader supplies complete, coherently visible block bytes for one parsing
/// callback. It cannot expose a mutable journal, filesystem or scratch buffer.
/// The callback's result must own any state needed after this call returns.
pub(crate) trait MetadataBlockRead {
    fn total_blocks(&self) -> u64;

    fn with_block<T>(
        &mut self,
        block: AbsoluteBN,
        inspect: impl FnOnce(&[u8]) -> Ext4Result<T>,
    ) -> Ext4Result<T>;
}

impl<B: BlockIo> MetadataBlockRead for Jbd2Dev<B> {
    fn total_blocks(&self) -> u64 {
        self.total_blocks()
    }

    fn with_block<T>(
        &mut self,
        block: AbsoluteBN,
        inspect: impl FnOnce(&[u8]) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        self.read_block(block)?;
        inspect(self.buffer())
    }
}
