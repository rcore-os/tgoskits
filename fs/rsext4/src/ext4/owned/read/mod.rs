//! Independent file mapping, private I/O and validated byte publication.

mod prepare;

use alloc::{sync::Arc, vec::Vec};
use core::ops::Range;

use super::{
    block_read::{IndependentBlockRead, InodeReadSnapshot, PreparedBlockRead},
    *,
};
use crate::{
    bmalloc::AbsoluteBN,
    file::{CompletedFileRead, FileBlockRead, FileReadMapping, PreparedFileRead},
};

/// Next phase of a read whose mount and inode allocation the caller retains.
pub enum InodeReadPreparation<D: BlockIo> {
    /// Read a cold inode table outside mount exclusion, then validate it.
    Inode(PreparedLiveInodeRead<D>),
    /// Walk mappings and read data outside mount exclusion.
    Read(PreparedInodeRead<D>),
    /// A zero-length request needs no storage access.
    Empty,
    /// Use the existing serialized legacy/unsupported/oversized path.
    Serialized,
}

/// Independent file reader bound to one protected inode incarnation.
///
/// Keep mount admission, allocation lifetime and shared inode-content exclusion
/// through preparation, execution, validation and final byte copying. The
/// version check supplements that exclusion; it does not prevent block reuse.
#[must_use = "execute outside mount exclusion, then validate on the originating mount"]
pub struct PreparedInodeRead<D: BlockIo> {
    blocks: PreparedBlockRead<D>,
    range: Range<u64>,
}

/// Private bytes or an error, not yet validated against the originating inode.
#[derive(Debug)]
#[must_use = "validate on the originating mount before exposing bytes or errors"]
pub struct CompletedInodeRead {
    snapshot: Arc<InodeReadSnapshot>,
    result: Ext4Result<CompletedFileRead>,
}

/// Validated immutable bytes whose atime completion has succeeded.
///
/// This view borrows completed storage, not mount state. Copy outside mount
/// exclusion, retaining the inode/mount admission used to prepare the read.
#[derive(Debug)]
#[must_use = "copy outside mount exclusion while retaining inode and mount admission"]
pub struct ValidatedInodeRead<'a> {
    bytes: &'a CompletedFileRead,
}

impl<D: BlockIo> PreparedInodeRead<D> {
    /// Walks the existing validated extent parser and performs data I/O without
    /// holding mount exclusion. Visibility callbacks retain immutable images
    /// only briefly; buffers, parsing and copying are independent. Both data
    /// and errors stay private until finish_inode_read checks the version.
    pub fn execute(self, cache: &mut impl InodeReadCache) -> CompletedInodeRead {
        let snapshot = self.blocks.snapshot.clone();
        let mut reader = self.blocks.execute(cache);
        let result = (|| {
            let plan = PreparedFileRead::prepare_with_reader(
                FileReadMapping {
                    number: snapshot.number,
                    inode: snapshot.inode,
                    context: snapshot.context(),
                },
                &mut reader,
                self.range,
            )?
            .ok_or_else(|| Ext4Error::corrupted().with_operation("inode:read_plan"))?;
            plan.read(|physical, bytes| reader.read_run(physical, bytes))
        })();
        CompletedInodeRead { snapshot, result }
    }
}

impl ValidatedInodeRead<'_> {
    /// Copies only valid file bytes, leaving the destination tail unchanged.
    ///
    /// # Errors
    /// Returns InvalidInput with buffer-size context without copying if short.
    pub fn copy_to(self, destination: &mut [u8]) -> Ext4Result<usize> {
        self.bytes.copy_to(destination)
    }
}

impl<D: BlockIo, C: InodeReadCache> FileBlockRead for IndependentBlockRead<'_, D, C> {
    fn data_images(
        &mut self,
        physical: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<Vec<Option<Arc<Vec<u8>>>>> {
        self.data_images(physical, count)
    }
}

impl<D: BlockIo> core::fmt::Debug for PreparedInodeRead<D> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PreparedInodeRead")
            .field("snapshot", &self.blocks.snapshot)
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

impl<D: BlockIo> core::fmt::Debug for InodeReadPreparation<D> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Inode(prepared) => formatter.debug_tuple("Inode").field(prepared).finish(),
            Self::Read(prepared) => formatter.debug_tuple("Read").field(prepared).finish(),
            Self::Empty => formatter.write_str("Empty"),
            Self::Serialized => formatter.write_str("Serialized"),
        }
    }
}
