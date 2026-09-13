//! Positioned I/O and cache integration.

use ax_io::prelude::*;
use axfs_ng_vfs::{
    FileExtentMap, FileExtentTarget, FileRangeOperation, Location, PreallocationMode, VfsError,
    VfsResult, WritebackPolicy,
};

use crate::{file::cache::CachedFile, io_error_to_vfs_error, vfs_error_to_io_error};

/// Low-level interface for file operations.
#[derive(Clone)]
pub enum FileBackend {
    /// File I/O goes through the page cache.
    Cached(CachedFile),
    /// File I/O bypasses the page cache and hits the VFS directly.
    Direct(Location),
}

impl FileBackend {
    pub(crate) fn new_direct(location: Location) -> Self {
        Self::Direct(location)
    }

    pub(crate) fn new_cached(location: Location) -> VfsResult<Self> {
        Ok(Self::Cached(CachedFile::get_or_create(location)?))
    }

    /// Returns the backend-visible file length.
    pub fn len(&self) -> VfsResult<u64> {
        match self {
            Self::Cached(cached) => Ok(cached.len()),
            Self::Direct(loc) => loc.len(),
        }
    }

    /// Returns whether the backend-visible file length is zero.
    pub fn is_empty(&self) -> VfsResult<bool> {
        self.len().map(|len| len == 0)
    }

    /// Reads data from the file at `offset` into `dst`.
    pub fn read_at(&self, mut dst: impl Write + IoBufMut, mut offset: u64) -> VfsResult<usize> {
        match self {
            Self::Cached(cached) => cached.read_at(dst, offset),
            Self::Direct(loc) => {
                let mut total = 0;
                while !dst.is_full() {
                    let read = match dst
                        .read_from(&mut ax_io::read_fn(|buf| {
                            loc.entry()
                                .as_file()
                                .map_err(vfs_error_to_io_error)?
                                .read_at(buf, offset)
                                .map_err(vfs_error_to_io_error)
                                .inspect(|read| {
                                    offset += *read as u64;
                                })
                        }))
                        .map_err(io_error_to_vfs_error)
                    {
                        Ok(read) => read,
                        Err(VfsError::WouldBlock) if total > 0 => break,
                        Err(err) => return Err(err),
                    };
                    if read == 0 {
                        break;
                    }
                    total += read;
                }
                Ok(total)
            }
        }
    }

    /// Writes `src` to the file at `offset`.
    pub fn write_at(&self, mut src: impl Read + IoBuf, mut offset: u64) -> VfsResult<usize> {
        match self {
            Self::Cached(cached) => cached.write_at(src, offset),
            Self::Direct(loc) => {
                let mut total = 0;
                let mut buf = [0; ax_io::DEFAULT_BUF_SIZE];
                while !src.is_empty() {
                    let limit = src.remaining().min(buf.len());
                    let read = src.read(&mut buf[..limit]).map_err(io_error_to_vfs_error)?;
                    if read == 0 {
                        break;
                    }
                    let mut chunk_written = 0;
                    while chunk_written < read {
                        let written = match loc
                            .entry()
                            .as_file()?
                            .write_at(&buf[chunk_written..read], offset)
                        {
                            Ok(written) => written,
                            Err(VfsError::WouldBlock) if total > 0 => return Ok(total),
                            Err(err) => return Err(err),
                        };
                        if written == 0 {
                            return Ok(total);
                        }
                        offset += written as u64;
                        total += written;
                        chunk_written += written;
                    }
                }
                Ok(total)
            }
        }
    }

    /// Appends `src` to the end of the file. Returns `(bytes_written, new_end)`.
    pub fn append(&self, mut src: impl Read + IoBuf) -> VfsResult<(usize, u64)> {
        match self {
            Self::Cached(cached) => cached.append(src),
            Self::Direct(loc) => {
                let mut total = 0;
                let mut end = loc.entry().as_file()?.len()?;
                while src.remaining() > 0 {
                    let chunk = src.remaining().min(ax_io::DEFAULT_BUF_SIZE);
                    let written = match src
                        .write_to(&mut ax_io::write_fn(|buf| {
                            loc.entry()
                                .as_file()
                                .map_err(vfs_error_to_io_error)?
                                .append(buf)
                                .map_err(vfs_error_to_io_error)
                                .map(|(n, offset)| {
                                    end = offset;
                                    n
                                })
                        }))
                        .map_err(io_error_to_vfs_error)
                    {
                        Ok(written) => written,
                        Err(VfsError::WouldBlock) if total > 0 => break,
                        Err(err) => return Err(err),
                    };
                    if written == 0 {
                        break;
                    }
                    total += written;
                    if written < chunk {
                        break;
                    }
                }
                Ok((total, end))
            }
        }
    }

    /// Returns a reference to the underlying [`Location`].
    pub fn location(&self) -> &Location {
        match self {
            Self::Cached(cached) => cached.location(),
            Self::Direct(loc) => loc,
        }
    }

    /// Flushes cached data (and optionally metadata) to disk.
    pub fn sync(&self, data_only: bool) -> VfsResult<()> {
        match self {
            Self::Cached(cached) => cached.sync(data_only),
            Self::Direct(loc) => loc.entry().as_file()?.sync(data_only),
        }
    }

    /// Truncates or extends the file to `len` bytes.
    pub fn set_len(&self, len: u64) -> VfsResult<()> {
        match self {
            Self::Cached(cached) => cached.set_len(len),
            Self::Direct(loc) => loc.entry().as_file()?.set_len(len),
        }?;
        self.sync_mount_mutation()
    }

    /// Reserves backing storage for a byte range.
    pub fn preallocate(&self, offset: u64, len: u64, mode: PreallocationMode) -> VfsResult<()> {
        self.operate_range(offset, len, FileRangeOperation::Allocate(mode))
    }

    /// Applies a storage or mapping operation to a byte range.
    pub fn operate_range(
        &self,
        offset: u64,
        len: u64,
        operation: FileRangeOperation,
    ) -> VfsResult<()> {
        match self {
            Self::Cached(cached) => cached.operate_range(offset, len, operation),
            Self::Direct(loc) => loc.entry().as_file()?.operate_range(offset, len, operation),
        }?;
        self.sync_mount_mutation()
    }

    fn sync_mount_mutation(&self) -> VfsResult<()> {
        if self
            .location()
            .writeback_policy()?
            .contains(WritebackPolicy::SYNCHRONOUS)
        {
            self.sync(false)?;
        }
        Ok(())
    }

    /// Queries the backing filesystem's allocated extent mappings.
    pub fn map_extents(
        &self,
        offset: u64,
        len: u64,
        target: FileExtentTarget,
        extent_limit: usize,
    ) -> VfsResult<FileExtentMap> {
        self.location()
            .entry()
            .as_file()?
            .map_extents(offset, len, target, extent_limit)
    }
}
