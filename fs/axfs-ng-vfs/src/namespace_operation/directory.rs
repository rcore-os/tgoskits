//! Owned directory batches; callbacks end before another backing operation.

use alloc::vec::Vec;

use crate::{DirEntrySink, DirectoryCursor, NodeType, VfsError, VfsResult};

#[derive(Clone, Debug)]
pub struct DirectoryRecord {
    pub name: Vec<u8>,
    pub inode: u64,
    pub node_type: NodeType,
    pub next: DirectoryCursor,
}

/// One bounded snapshot, with no inode, admission, gate or iterator ownership.
#[derive(Clone, Debug)]
pub struct DirectoryBatch {
    records: Vec<DirectoryRecord>,
}

impl DirectoryBatch {
    pub const MAX_ENTRIES: usize = 128;

    /// Backend snapshot construction; the bound request validates progress.
    pub fn new(records: Vec<DirectoryRecord>) -> VfsResult<Self> {
        if records.len() > Self::MAX_ENTRIES {
            return Err(VfsError::InvalidInput);
        }
        Ok(Self { records })
    }

    pub fn records(&self) -> &[DirectoryRecord] {
        &self.records
    }

    /// Collects at most one batch from an existing native reader. The callback
    /// must use that reader's already-held admission and retirement owner.
    pub fn collect(
        read: impl FnOnce(&mut dyn DirEntrySink) -> VfsResult<usize>,
    ) -> VfsResult<Self> {
        let mut records = Vec::new();
        records
            .try_reserve_exact(Self::MAX_ENTRIES)
            .map_err(|_| VfsError::NoMemory)?;
        let mut error = None;
        let count = read(&mut |name: &[u8], inode, node_type, next| {
            if records.len() == Self::MAX_ENTRIES {
                return false;
            }
            let mut owned = Vec::new();
            if owned.try_reserve_exact(name.len()).is_err() {
                error = Some(VfsError::NoMemory);
                return false;
            }
            owned.extend_from_slice(name);
            records.push(DirectoryRecord {
                name: owned,
                inode,
                node_type,
                next,
            });
            true
        })?;
        if let Some(error) = error {
            return Err(error);
        }
        if count != records.len() {
            return Err(VfsError::BadState);
        }
        Ok(Self { records })
    }

    pub(crate) fn validate(&self, mut cursor: DirectoryCursor) -> VfsResult<()> {
        if self.records.len() > Self::MAX_ENTRIES {
            return Err(VfsError::BadState);
        }
        for record in &self.records {
            if record.name.is_empty()
                || record.name.contains(&b'/')
                || record.name.contains(&0)
                || record.next == cursor
            {
                return Err(VfsError::BadState);
            }
            cursor = record.next;
        }
        Ok(())
    }
}
