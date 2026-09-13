//! Namespace-protected directory reads without holding the mounted owner for I/O.

mod blocks;
mod prepare;

use alloc::sync::Arc;

pub use super::block_read::{
    InodeBlockRequest as DirectoryBlockRequest, InodeReadCache as DirectoryReadCache,
};
use super::{
    block_read::{InodeReadSnapshot, PreparedBlockRead},
    *,
};
use crate::dir::find_named_entry;

/// The next ownership phase for a retained directory lookup.
pub enum DirectoryLookupPreparation<D: BlockIo> {
    /// Read one cold parent inode before preparing its directory mapping.
    Parent(PreparedLiveInodeRead<D>),
    /// Traverse the directory through an independent device endpoint.
    Lookup(PreparedDirectoryLookup<D>),
    /// The endpoint explicitly lacks fork support; use serialized lookup.
    Serialized,
}

/// Validated lookup outcome. Acquire a found inode's allocation reference
/// before releasing the namespace guard and mounted owner exclusion.
#[derive(Debug, PartialEq, Eq)]
pub enum DirectoryLookupOutcome {
    /// A mutation or rollback invalidated this attempt; prepare it again.
    Retry,
    /// No entry with this exact name exists in the protected directory.
    Missing,
    /// The caller must retain this inode before exposing it to other tasks.
    Found(InodeNumber),
}

/// One directory incarnation and its independent, coherent read endpoint.
///
/// Keep mount admission, a retained parent allocation, and shared namespace
/// exclusion until completion or cancellation. Namespace exclusion must prevent
/// changing this directory's entries/mappings and recycling its referenced
/// children. Version checks detect superseded results, but do not replace that
/// exclusion. No user-visible result or error is available until completion.
#[must_use = "execute without mount exclusion, then finish on the originating mount"]
pub struct PreparedDirectoryLookup<D: BlockIo> {
    blocks: PreparedBlockRead<D>,
}

/// Private lookup result bound to the mount and directory version that produced it.
#[derive(Debug)]
#[must_use = "validate under mount and namespace exclusion before retaining a child"]
pub struct CompletedDirectoryLookup {
    snapshot: Arc<InodeReadSnapshot>,
    result: Ext4Result<Option<InodeNumber>>,
}

impl<D: BlockIo> PreparedDirectoryLookup<D> {
    /// Performs on-demand extent/legacy mapping and HTree/linear lookup using
    /// the same parsers as serialized lookup. `cache` obtains immutable visible
    /// images using short mount critical sections; device I/O and parsing happen
    /// only after those sections have ended. A failed read stays private until
    /// `finish_directory_lookup` validates the original namespace version.
    pub fn execute(
        self,
        name: FileName<'_>,
        cache: &mut impl DirectoryReadCache,
    ) -> CompletedDirectoryLookup {
        let snapshot = self.blocks.snapshot.clone();
        let mut reader = blocks::IndependentDirectoryRead::new(self.blocks.execute(cache));
        let result = match find_named_entry(&mut reader, name.as_bytes()) {
            Ok(entry) => Ok(Some(entry.ino)),
            Err(error) if error.kind() == Ext4ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        };
        CompletedDirectoryLookup { snapshot, result }
    }
}

impl<D: BlockIo> core::fmt::Debug for PreparedDirectoryLookup<D> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PreparedDirectoryLookup")
            .field("snapshot", &self.blocks.snapshot)
            .finish_non_exhaustive()
    }
}

impl<D: BlockIo> core::fmt::Debug for DirectoryLookupPreparation<D> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Parent(prepared) => formatter.debug_tuple("Parent").field(prepared).finish(),
            Self::Lookup(prepared) => formatter.debug_tuple("Lookup").field(prepared).finish(),
            Self::Serialized => formatter.write_str("Serialized"),
        }
    }
}
