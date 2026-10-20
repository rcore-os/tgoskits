//! Filesystem images served through an owned file backend.

use alloc::{
    string::{String, ToString},
    sync::Arc,
};

use axfs_ng_vfs::{VfsError, VfsResult};

use super::FsBlockDevice;
use crate::{BlockError, BlockResult, file::FileBackend, os::sync::Mutex};

const SECTOR_SIZE: usize = 512;

/// Private release hook shared by the image device and its ext4 filesystem.
///
/// Implementations must release the source binding at most once. The final
/// lease retirement calls this after shutdown; dropping the last shared owner
/// remains the RAII fallback when shutdown never completes.
pub(crate) trait FileBackingLease: Send + Sync {
    fn release(&self);
}

pub(crate) struct SharedFileBacking<L> {
    lease: Mutex<Option<L>>,
}

impl<L> SharedFileBacking<L> {
    pub(crate) fn new(lease: L) -> Self {
        Self {
            lease: Mutex::new(Some(lease)),
        }
    }
}

impl<L: Send> FileBackingLease for SharedFileBacking<L> {
    fn release(&self) {
        // Taking the lease out while holding the cell lock would run the
        // caller's Drop (potentially doing I/O) under that lock.
        let lease = self.lease.lock().take();
        drop(lease);
    }
}

pub(crate) struct FileImageDevice {
    backend: FileBackend,
    name: String,
    num_blocks: u64,
    read_only: bool,
    // Keep the source binding alive even after a lazy mount detach.
    _lease: Arc<dyn FileBackingLease>,
}

impl FileImageDevice {
    pub(crate) fn new(
        backend: FileBackend,
        read_only: bool,
        lease: Arc<dyn FileBackingLease>,
    ) -> VfsResult<Self> {
        let num_blocks = backend.len()? / SECTOR_SIZE as u64;
        if num_blocks == 0 {
            return Err(VfsError::InvalidInput);
        }
        let name = backend.location().absolute_path()?.to_string();
        Ok(Self {
            backend,
            name,
            num_blocks,
            read_only,
            _lease: lease,
        })
    }

    fn byte_offset(&self, block: u64, len: usize) -> BlockResult<u64> {
        if !len.is_multiple_of(SECTOR_SIZE) {
            return Err(BlockError::InvalidRequest);
        }
        let blocks = (len / SECTOR_SIZE) as u64;
        let end = block
            .checked_add(blocks)
            .ok_or(BlockError::InvalidRequest)?;
        if end > self.num_blocks {
            return Err(BlockError::InvalidRequest);
        }
        block
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(BlockError::InvalidRequest)
    }
}

impl FsBlockDevice for FileImageDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_blocks(&self) -> u64 {
        self.num_blocks
    }

    fn block_size(&self) -> usize {
        SECTOR_SIZE
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> BlockResult {
        let offset = self.byte_offset(block, buf.len())?;
        let len = buf.len();
        let read = self
            .backend
            .read_at(buf, offset)
            .map_err(|_| BlockError::Io)?;
        if read != len {
            return Err(BlockError::Io);
        }
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8]) -> BlockResult {
        if self.read_only {
            return Err(BlockError::Unsupported);
        }
        let offset = self.byte_offset(block, buf.len())?;
        let written = self
            .backend
            .write_at(buf, offset)
            .map_err(|_| BlockError::Io)?;
        if written != buf.len() {
            return Err(BlockError::Io);
        }
        Ok(())
    }

    fn flush(&mut self) -> BlockResult {
        self.backend.sync(false).map_err(|_| BlockError::Io)
    }
}
