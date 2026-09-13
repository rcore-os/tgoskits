//! Open-file access, cursor ownership, and persistence completion.

#[cfg(test)]
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::{AtomicU8, Ordering};

use ax_io::{SeekFrom, prelude::*};
use axfs_ng_vfs::{
    FileExtentMap, FileExtentTarget, FileRangeOperation, Location, NodeFlags, NodeType,
    PreallocationMode, VfsError, VfsResult, WritebackPolicy, path::Path,
};
use axpoll::{IoEvents, Pollable};

use super::open::{FileFlags, OpenOptions, OpenResult};
use crate::{fs_core::FsContext, os::sync::SleepMutex as Mutex, vfs_error_to_io_error};

mod backend;
pub use backend::FileBackend;

/// Persistence required before reporting a successful file write.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WriteSync {
    /// Leave durability to explicit sync or filesystem writeback.
    #[default]
    Buffered,
    /// Persist data and the metadata needed to read it back.
    Data,
    /// Persist data and all associated file metadata.
    All,
}

/// Provides `std::fs::File`-like interface.
pub struct File {
    inner: FileBackend,
    flags: AtomicU8,
    position: Option<Mutex<u64>>,
    access_flags: AtomicU8,
    write_sync: WriteSync,
}

impl File {
    /// Creates a new [`File`] from a [`FileBackend`] and access flags.
    pub fn new(inner: FileBackend, flags: FileFlags) -> Self {
        // man 2 open: "The file offset is set to the beginning of the file"
        // — initial position is always 0, regardless of O_APPEND.
        // O_APPEND only relocates the offset BEFORE EACH WRITE (handled in
        // `write()` via the `access(FileFlags::APPEND)` branch). Setting
        // initial position to EOF would break read() on RDONLY|APPEND
        // (read sees EOF immediately) — see bug-open-rdonly-append-promotes-rw.
        let position = if inner.location().flags().contains(NodeFlags::STREAM) {
            None
        } else {
            Some(Mutex::new(0))
        };
        Self {
            inner,
            flags: AtomicU8::new(flags.bits()),
            position,
            access_flags: AtomicU8::new(0),
            write_sync: WriteSync::Buffered,
        }
    }

    /// Configures write completion before publishing this open file.
    /// Only regular files and block devices use this persistence policy.
    pub fn with_write_sync(mut self, policy: WriteSync) -> Self {
        self.write_sync = policy;
        self
    }

    /// Opens an existing file for reading.
    pub fn open(context: &FsContext, path: impl AsRef<Path>) -> VfsResult<Self> {
        OpenOptions::new()
            .read(true)
            .open(context, path.as_ref())
            .and_then(OpenResult::into_file)
    }

    /// Opens a file for writing, creating it if it does not exist and
    /// truncating it if it does.
    pub fn create(context: &FsContext, path: impl AsRef<Path>) -> VfsResult<Self> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(context, path.as_ref())
            .and_then(OpenResult::into_file)
    }

    /// Checks that the file has the required `flags` and returns the backend.
    pub fn access(&self, flags: FileFlags) -> VfsResult<&FileBackend> {
        if self.flags().contains(flags) && !self.is_path() {
            if self.inner.location().is_readonly()
                && flags.intersects(FileFlags::WRITE | FileFlags::APPEND)
            {
                return Err(VfsError::ReadOnlyFilesystem);
            }
            Ok(&self.inner)
        } else {
            Err(VfsError::BadFileDescriptor)
        }
    }

    /// Returns `true` if this is a path-only handle (no I/O permitted).
    pub fn is_path(&self) -> bool {
        self.flags().contains(FileFlags::PATH)
    }

    /// Returns the access flags this file was opened with.
    pub fn flags(&self) -> FileFlags {
        FileFlags::from_bits_truncate(self.flags.load(Ordering::Acquire))
    }

    /// Atomically sets or clears a single flag bit.
    pub fn set_flag(&self, flag: FileFlags, enabled: bool) {
        let bits = flag.bits();
        if enabled {
            self.flags.fetch_or(bits, Ordering::AcqRel);
        } else {
            self.flags.fetch_and(!bits, Ordering::AcqRel);
        }
    }

    /// Returns the file's current read/write cursor, or `None` for stream
    /// nodes (sockets / pipes / `STREAM`-flagged) that have no addressable
    /// position. Read-only snapshot; does not move the cursor.
    pub fn position(&self) -> Option<u64> {
        self.position.as_ref().map(|m| *m.lock())
    }

    /// Returns a reference to the underlying [`FileBackend`].
    pub fn backend(&self) -> VfsResult<&FileBackend> {
        self.access(FileFlags::empty())?;
        Ok(&self.inner)
    }

    /// Returns a reference to the underlying [`Location`].
    pub fn location(&self) -> &Location {
        self.inner.location()
    }

    /// Reads a number of bytes starting from a given offset.
    pub fn read_at(&self, dst: impl Write + IoBufMut, offset: u64) -> VfsResult<usize> {
        self.access(FileFlags::READ)?.read_at(dst, offset)
    }

    /// Writes a number of bytes starting from a given offset.
    pub fn write_at(&self, src: impl Read + IoBuf, offset: u64) -> VfsResult<usize> {
        let written = self.access(FileFlags::WRITE)?.write_at(src, offset)?;
        self.finish_write(written)
    }

    /// Truncates or extends the file to `len` bytes.
    pub fn set_len(&self, len: u64) -> VfsResult<()> {
        self.access(FileFlags::WRITE)?.set_len(len)
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
        self.access(FileFlags::WRITE)?
            .operate_range(offset, len, operation)
    }

    /// Queries allocated file-to-device mappings without changing file state.
    pub fn map_extents(
        &self,
        offset: u64,
        len: u64,
        target: FileExtentTarget,
        extent_limit: usize,
    ) -> VfsResult<FileExtentMap> {
        self.access(FileFlags::empty())?
            .map_extents(offset, len, target, extent_limit)
    }

    /// Attempts to sync OS-internal file content and metadata to disk.
    ///
    /// If `data_only` is `true`, only the file data is synced, not the
    /// metadata.
    pub fn sync(&self, data_only: bool) -> VfsResult<()> {
        self.access(FileFlags::empty())?;
        self.inner.sync(data_only)
    }

    /// Reads data from the current position, advancing the cursor.
    pub fn read(&self, dst: impl Write + IoBufMut) -> ax_io::Result<usize> {
        self.access_flags.fetch_or(1, Ordering::AcqRel);
        if let Some(pos) = self.position.as_ref() {
            let mut pos = pos.lock();
            self.read_at(dst, *pos)
                .map_err(vfs_error_to_io_error)
                .inspect(|n| {
                    *pos += *n as u64;
                })
        } else {
            self.read_at(dst, 0).map_err(vfs_error_to_io_error)
        }
    }

    /// Writes data at the current position (or appends), advancing the cursor.
    pub fn write(&self, src: impl Read + IoBuf) -> ax_io::Result<usize> {
        self.access_flags.fetch_or(3, Ordering::AcqRel);
        // WRITE bit is mandatory for any write path, regardless of whether
        // APPEND is set. Otherwise O_RDONLY|O_APPEND fd would silently
        // succeed writes (since access(APPEND) only checks the APPEND bit).
        // Fixes bug-open-rdonly-append-promotes-rw (the part inside axfs).
        self.access(FileFlags::WRITE)
            .map_err(vfs_error_to_io_error)?;
        if let Some(pos) = self.position.as_ref() {
            let mut pos = pos.lock();
            if let Ok(f) = self.access(FileFlags::APPEND) {
                let (written, new_size) = f.append(src).map_err(vfs_error_to_io_error)?;
                self.finish_write(written).map_err(vfs_error_to_io_error)?;
                if written != 0 {
                    *pos = new_size;
                }
                Ok(written)
            } else {
                self.write_at(src, *pos)
                    .map_err(vfs_error_to_io_error)
                    .inspect(|n| {
                        *pos += *n as u64;
                    })
            }
        } else {
            self.write_at(src, 0).map_err(vfs_error_to_io_error)
        }
    }

    fn finish_write(&self, written: usize) -> VfsResult<usize> {
        if written != 0
            && matches!(
                self.location().node_type(),
                NodeType::RegularFile | NodeType::BlockDevice
            )
        {
            let synchronous = self
                .location()
                .writeback_policy()?
                .contains(WritebackPolicy::SYNCHRONOUS);
            if synchronous || self.write_sync != WriteSync::Buffered {
                self.inner
                    .sync(!synchronous && self.write_sync == WriteSync::Data)?;
            }
        }
        Ok(written)
    }

    /// Flushes any internally buffered data. Currently a no-op.
    pub fn flush(&self) -> ax_io::Result {
        self.access(FileFlags::empty())
            .map_err(vfs_error_to_io_error)?;
        Ok(())
    }
}

impl Read for &File {
    fn read(&mut self, buf: &mut [u8]) -> ax_io::Result<usize> {
        (*self).read(buf)
    }
}

impl Write for &File {
    fn write(&mut self, buf: &[u8]) -> ax_io::Result<usize> {
        (*self).write(buf)
    }

    fn flush(&mut self) -> ax_io::Result {
        (*self).flush()
    }
}

impl Seek for &File {
    fn seek(&mut self, pos: SeekFrom) -> ax_io::Result<u64> {
        self.access(FileFlags::empty())
            .map_err(vfs_error_to_io_error)?;

        if let Some(guard) = self.position.as_ref() {
            let mut guard = guard.lock();
            let new_pos = match pos {
                SeekFrom::Start(pos) => pos,
                SeekFrom::End(off) => {
                    let size = self.inner.len().map_err(vfs_error_to_io_error)?;
                    size.checked_add_signed(off)
                        .ok_or(ax_io::Error::InvalidInput)?
                }
                SeekFrom::Current(off) => guard
                    .checked_add_signed(off)
                    .ok_or(ax_io::Error::InvalidInput)?,
            };
            *guard = new_pos;
            Ok(new_pos)
        } else {
            Ok(0)
        }
    }
}

impl Pollable for File {
    fn poll(&self) -> IoEvents {
        self.inner.location().poll()
    }

    unsafe fn register_shared(
        &self,
        sink: &mut dyn axpoll::SharedRegistrationSink,
        events: IoEvents,
    ) {
        unsafe { self.inner.location().register_shared(sink, events) }
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        unsafe { self.inner.location().register_exclusive(sink, events) }
    }
}

fn needs_metadata_update_on_drop(location: &Location, access_flags: u8) -> bool {
    access_flags != 0 && !location.is_readonly()
}

#[cfg(test)]
static DROP_METADATA_UPDATE_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

impl Drop for File {
    fn drop(&mut self) {
        let flags = self.access_flags.load(Ordering::Acquire);
        if needs_metadata_update_on_drop(self.inner.location(), flags) {
            #[cfg(test)]
            DROP_METADATA_UPDATE_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            let mut update = axfs_ng_vfs::MetadataUpdate::default();
            if flags & 1 != 0 {
                update.atime = Some(crate::os::wall_time());
            }
            if flags & 2 != 0 {
                update.mtime = Some(crate::os::wall_time());
            }
            if let Err(err) = self.inner.location().update_metadata(update) {
                warn!("Failed to update file times on drop: {err:?}");
            }
        }
    }
}

#[cfg(test)]
mod tests;
