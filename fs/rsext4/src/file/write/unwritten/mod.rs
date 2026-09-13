//! Separate owners for unwritten preparation, data I/O and conversion.

use super::*;
use crate::blockdev::{ReservedJournalHandle, TransactionCredits};

mod completion;
mod data;
mod prepare;

use completion::free_unwritten_finish_reservation;
pub(super) use prepare::extent_write_needs_preparation;

/// Metadata owner retained between preparation and data completion.
/// Reservation and rollback snapshots travel together; a data failure cannot
/// accidentally enter the extent-conversion phase.
pub(super) struct PreparedUnwrittenWrite {
    target: WriteTarget,
    inode: Ext4Inode,
    prepared: Vec<PreparedUnwrittenRun>,
    finish_reservation: Option<ReservedJournalHandle>,
    leaf_snapshots: Vec<ExtentMetadataSnapshot>,
}

impl PreparedUnwrittenWrite {
    pub(super) fn prepare_detached<B: BlockIo>(
        device: &mut Jbd2Dev<B>,
        fs: &mut Ext4FileSystem,
        target: WriteTarget,
    ) -> Ext4Result<Self> {
        let mut prepared = prepare::prepare_unwritten_write(device, fs, target)?;
        // A completion which needs journal progress must not retain credits
        // that prevent the worker from sealing that very transaction.
        free_unwritten_finish_reservation(device, &mut prepared.finish_reservation)?;
        Ok(prepared)
    }
}

/// Both success and failure retain the preparation owner until publication.
struct CompletedUnwrittenWrite {
    prepared: PreparedUnwrittenWrite,
    result: Ext4Result<()>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PreparedUnwrittenRun {
    logical_start: u32,
    physical_start: AbsoluteBN,
    len: u32,
}

struct ExtentMetadataSnapshot {
    block: AbsoluteBN,
    bytes: Vec<u8>,
}

pub(super) fn write_inode_data_through_unwritten<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    target: WriteTarget,
    write: &WriteSlice<'_>,
) -> Ext4Result<()> {
    let prepared = prepare::prepare_unwritten_write(device, fs, target)?;
    prepared
        .execute_serialized(device, fs, write)
        .finish(device, fs, write.end)
}
