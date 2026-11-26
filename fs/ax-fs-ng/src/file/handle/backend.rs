//! Positioned data I/O and page-cache integration.
use alloc::sync::Arc;
use core::io::BorrowedCursor;

use ax_io::prelude::*;
use axfs_ng_vfs::{
    FileExtentMap, FileExtentTarget, FileIoRequest, FileIoTarget, FileRangeOperation, Location,
    NodeFlags, NodeType, PathHandle, PreallocationMode, VfsError, VfsResult, WritebackPolicy,
};

use super::super::access::WriteAccess;
use crate::{file::cache::CachedFile, io_error_to_vfs_error, vfs_error_to_io_error};

/// Bounds the kernel copy of one write to a packet node.
const MAX_PACKET_RECORD: usize = 1 << 17;

/// Low-level interface for file operations.
#[derive(Clone)]
pub enum FileBackend {
    /// File I/O goes through the page cache.
    Cached(CachedFile),
    /// File I/O bypasses the page cache and hits the VFS directly.
    Direct(PathHandle),
    /// A layered writable description bound to its accepted native endpoint.
    Layered(Arc<LayeredFile>),
}

/// Keeps the original path graph and native cache through writable I/O.
/// Fields are private so a layered backend cannot select another logical root
/// or replace its writer lease after the description has been published.
pub struct LayeredFile {
    native: FileBackend,
    target: FileIoTarget,
    // Native caches and path uses retire before the original writer authority.
    write: Option<Arc<WriteAccess>>,
}

impl FileBackend {
    pub(crate) fn new_layered(
        location: Location,
        write: &Arc<WriteAccess>,
        direct: bool,
    ) -> VfsResult<Self> {
        let target = FileIoRequest::write(PathHandle::acquire(location)?, write.prepared_write())?
            .resolve()?;
        write.bind_backing(&target)?;
        let location = target.location().clone();
        let node_flags = location.flags();
        let native = if node_flags.contains(NodeFlags::ALWAYS_CACHE)
            || !(direct || node_flags.contains(NodeFlags::NON_CACHEABLE))
        {
            Self::new_cached(location)?
        } else {
            Self::new_direct(location)?
        };
        Ok(Self::Layered(Arc::new(LayeredFile {
            native,
            target,
            write: Some(write.clone()),
        })))
    }

    pub(crate) fn finish_readonly_open(&mut self) {
        if let Self::Layered(file) = self {
            // The backend has not escaped OpenOptions, including on O_TRUNC.
            let access = Arc::get_mut(file)
                .expect("unpublished file backend")
                .write
                .take();
            drop(access);
        }
    }

    /// Borrows the actual native cache selected by this description.
    /// Mapping consumers must retain the description's writer lease separately.
    pub fn cached_file(&self) -> Option<&CachedFile> {
        match self {
            Self::Cached(cached) => Some(cached),
            Self::Direct(_) => None,
            Self::Layered(file) => file.native.cached_file(),
        }
    }

    /// Native data endpoint; `path_handle` continues to identify the logical fd.
    pub fn data_path(&self) -> &PathHandle {
        match self {
            Self::Layered(file) => file.native.data_path(),
            _ => self.path_handle(),
        }
    }

    pub(crate) fn new_direct(location: Location) -> VfsResult<Self> {
        Ok(Self::Direct(PathHandle::acquire(location)?))
    }

    pub(crate) fn new_cached(location: Location) -> VfsResult<Self> {
        Ok(Self::Cached(CachedFile::get_or_create(
            PathHandle::acquire(location)?,
        )?))
    }

    /// Returns the backend-visible file length.
    pub fn len(&self) -> VfsResult<u64> {
        match self {
            Self::Cached(cached) => Ok(cached.len()),
            Self::Direct(path) => path.location().len(),
            Self::Layered(file) => file.native.len(),
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
            Self::Layered(file) => file.native.read_at(dst, offset),
            Self::Direct(path) => {
                let loc = path.location();
                let packet = loc.flags().contains(NodeFlags::PACKET);
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
                        // Completed reads may already have consumed device data.
                        // Report that progress before any later read/copy error.
                        Err(_) if total > 0 => break,
                        Err(err) => return Err(err),
                    };
                    if read == 0 {
                        break;
                    }
                    total += read;
                    if packet {
                        break;
                    }
                }
                Ok(total)
            }
        }
    }

    /// Reads directly into exclusively borrowed, resident kernel storage.
    ///
    /// Unlike an arbitrary Writer, this destination cannot fault or reenter
    /// the cache during a copy. Cached reads need no intermediate page.
    /// Direct backends retain their existing initialization and I/O contract.
    /// Returns only this call's byte count; an error may leave a filled prefix.
    pub fn read_buf_at(&self, dst: BorrowedCursor<'_, u8>, offset: u64) -> VfsResult<usize> {
        match self {
            Self::Cached(cached) => cached.read_buf_at(dst, offset),
            Self::Layered(file) => file.native.read_buf_at(dst, offset),
            Self::Direct(_) => self.read_at(dst, offset),
        }
    }

    /// Writes `src` to the file at `offset`.
    pub fn write_at(&self, mut src: impl Read + IoBuf, offset: u64) -> VfsResult<usize> {
        match self {
            Self::Cached(cached) => cached.write_at_with_completion(&mut src, offset, |written| {
                Self::finish_cached_write(cached, written)
            }),
            Self::Layered(file) => file.native.write_at(src, offset),
            Self::Direct(path) => {
                let loc = path.location();
                if loc.flags().contains(NodeFlags::PACKET) {
                    let len = src.remaining();
                    if len > MAX_PACKET_RECORD {
                        return Err(VfsError::InvalidInput);
                    }
                    let mut record = alloc::vec![0; len];
                    src.read_exact(&mut record).map_err(io_error_to_vfs_error)?;
                    return loc.entry().as_file()?.write_at(&record, offset);
                }
                let length = src.remaining();
                loc.entry()
                    .as_file()?
                    .write_iter(&mut src, length, Some(offset))
                    .map(|(written, _)| written)
            }
        }
    }

    /// Appends `src` to the end of the file. Returns `(bytes_written, new_end)`.
    pub fn append(&self, mut src: impl Read + IoBuf) -> VfsResult<(usize, u64)> {
        match self {
            Self::Cached(cached) => cached.append_with_completion(&mut src, |written| {
                Self::finish_cached_write(cached, written)
            }),
            Self::Layered(file) => file.native.append(src),
            Self::Direct(path) => {
                let loc = path.location();
                let length = src.remaining();
                loc.entry().as_file()?.write_iter(&mut src, length, None)
            }
        }
    }

    fn finish_cached_write(cached: &CachedFile, written: usize) -> VfsResult<()> {
        if written != 0
            && matches!(
                cached.location().node_type(),
                NodeType::RegularFile | NodeType::BlockDevice
            )
            && cached
                .location()
                .entry()
                .writeback_policy()?
                .contains(WritebackPolicy::SYNCHRONOUS)
        {
            // Complete dirty cache pages before a write to a synchronous inode
            // is reported successful, including writes from file-backed block I/O.
            cached.sync(false)?;
        }
        Ok(())
    }

    /// Returns a reference to the underlying [`Location`].
    pub fn location(&self) -> &Location {
        match self {
            Self::Cached(cached) => cached.location(),
            Self::Direct(path) => path.location(),
            Self::Layered(file) => file.target.paths()[0].location(),
        }
    }

    /// Borrows the exact mount use retained by this I/O endpoint.
    pub fn path_handle(&self) -> &PathHandle {
        match self {
            Self::Cached(cached) => cached.path_handle(),
            Self::Direct(path) => path,
            Self::Layered(file) => &file.target.paths()[0],
        }
    }

    /// Flushes cached data (and optionally metadata) to disk.
    pub fn sync(&self, data_only: bool) -> VfsResult<()> {
        match self {
            Self::Cached(cached) => cached.sync(data_only),
            Self::Direct(path) => path.location().entry().as_file()?.sync(data_only),
            Self::Layered(file) => file.native.sync(data_only),
        }
    }

    /// Truncates or extends the file to `len` bytes.
    pub fn set_len(&self, len: u64) -> VfsResult<()> {
        match self {
            Self::Cached(cached) => cached.set_len(len),
            Self::Direct(path) => path.location().entry().as_file()?.set_len(len),
            Self::Layered(file) => file.native.set_len(len),
        }
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
            Self::Layered(file) => file.native.operate_range(offset, len, operation),
            Self::Direct(path) => path
                .location()
                .entry()
                .as_file()?
                .operate_range(offset, len, operation),
        }
    }

    /// Queries the backing filesystem's allocated extent mappings.
    pub fn map_extents(
        &self,
        offset: u64,
        len: u64,
        target: FileExtentTarget,
        extent_limit: usize,
    ) -> VfsResult<FileExtentMap> {
        self.data_path().location().entry().as_file()?.map_extents(
            offset,
            len,
            target,
            extent_limit,
        )
    }
}
