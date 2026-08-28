#[cfg(test)]
use core::sync::atomic::AtomicUsize;
use core::{
    sync::atomic::{AtomicU8, Ordering},
    task::Context,
};

use ax_io::{SeekFrom, prelude::*};
use axfs_ng_vfs::{
    FileExtentMap, FileExtentTarget, FileRangeOperation, FsIoEvents, FsPollable, Location,
    NodeFlags, PreallocationMode, VfsError, VfsResult, path::Path,
};

use super::{
    cache::CachedFile,
    open::{FileFlags, OpenOptions, OpenResult},
};
use crate::{
    fs_core::FsContext, io_error_to_vfs_error, os::sync::SleepMutex as Mutex, vfs_error_to_io_error,
};

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
            Self::Direct(loc) => loc.entry().as_file()?.operate_range(offset, len, operation),
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
        self.location()
            .entry()
            .as_file()?
            .map_extents(offset, len, target, extent_limit)
    }
}

/// Provides `std::fs::File`-like interface.
pub struct File {
    inner: FileBackend,
    flags: AtomicU8,
    position: Option<Mutex<u64>>,
    access_flags: AtomicU8,
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
        }
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
        self.access(FileFlags::WRITE)?.write_at(src, offset)
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
                f.append(src)
                    .map_err(vfs_error_to_io_error)
                    .map(|(written, new_size)| {
                        *pos = new_size;
                        written
                    })
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

impl FsPollable for File {
    fn poll(&self) -> FsIoEvents {
        self.inner.location().poll()
    }

    fn register(&self, context: &mut Context<'_>, events: FsIoEvents) {
        self.inner.location().register(context, events)
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
mod tests {
    use alloc::sync::Arc;
    use core::{
        any::Any,
        sync::atomic::{AtomicUsize, Ordering},
        task::Context,
        time::Duration,
    };

    use axfs_ng_vfs::{
        DeviceId, DirEntry, FileNode, FileNodeOps, Filesystem, FilesystemOps, FsIoEvents,
        FsPollable, Metadata, MetadataUpdate, Mountpoint, NodeOps, NodePermission, NodeType,
        Reference, StatFs,
    };

    use super::*;

    struct TestFilesystem {
        name: &'static str,
        readonly: bool,
    }

    static READONLY_TEST_FILESYSTEM: TestFilesystem = TestFilesystem {
        name: "readonly-file-drop-test",
        readonly: true,
    };

    static WRITABLE_TEST_FILESYSTEM: TestFilesystem = TestFilesystem {
        name: "writable-file-drop-test",
        readonly: false,
    };

    static DROP_METADATA_UPDATE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl FilesystemOps for TestFilesystem {
        fn name(&self) -> &str {
            self.name
        }

        fn is_readonly(&self) -> bool {
            self.readonly
        }

        fn root_dir(&self) -> DirEntry {
            test_entry(Arc::new(MetadataTrackingTestFile::new(
                &READONLY_TEST_FILESYSTEM,
                Arc::new(AtomicUsize::new(0)),
            )))
        }

        fn stat(&self) -> VfsResult<StatFs> {
            Err(VfsError::InvalidInput)
        }
    }

    struct MetadataTrackingTestFile {
        filesystem: &'static dyn FilesystemOps,
        metadata_updates: Arc<AtomicUsize>,
    }

    impl MetadataTrackingTestFile {
        fn new(filesystem: &'static dyn FilesystemOps, metadata_updates: Arc<AtomicUsize>) -> Self {
            Self {
                filesystem,
                metadata_updates,
            }
        }
    }

    impl NodeOps for MetadataTrackingTestFile {
        fn inode(&self) -> u64 {
            1
        }

        fn metadata(&self) -> VfsResult<Metadata> {
            Ok(Metadata {
                device: 1,
                inode: self.inode(),
                nlink: 1,
                mode: NodePermission::default(),
                node_type: NodeType::RegularFile,
                uid: 0,
                gid: 0,
                size: 1,
                block_size: 1,
                blocks: 1,
                rdev: DeviceId::default(),
                atime: Duration::ZERO,
                mtime: Duration::ZERO,
                ctime: Duration::ZERO,
            })
        }

        fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
            self.metadata_updates.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn filesystem(&self) -> &dyn FilesystemOps {
            self.filesystem
        }

        fn sync(&self, _data_only: bool) -> VfsResult<()> {
            Ok(())
        }

        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }

        fn flags(&self) -> NodeFlags {
            NodeFlags::empty()
        }
    }

    impl FsPollable for MetadataTrackingTestFile {
        fn poll(&self) -> FsIoEvents {
            FsIoEvents::IN | FsIoEvents::OUT
        }

        fn register(&self, _context: &mut Context<'_>, _events: FsIoEvents) {}
    }

    impl FileNodeOps for MetadataTrackingTestFile {
        fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
            if offset == 0
                && let Some(byte) = buf.first_mut()
            {
                *byte = 0;
                Ok(1)
            } else {
                Ok(0)
            }
        }

        fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
            Err(VfsError::ReadOnlyFilesystem)
        }

        fn append(&self, _buf: &[u8]) -> VfsResult<(usize, u64)> {
            Err(VfsError::ReadOnlyFilesystem)
        }

        fn set_len(&self, _len: u64) -> VfsResult<()> {
            Err(VfsError::ReadOnlyFilesystem)
        }
    }

    fn test_entry(node: Arc<MetadataTrackingTestFile>) -> DirEntry {
        DirEntry::new_file(
            FileNode::new(node),
            NodeType::RegularFile,
            Reference::root(),
        )
    }

    fn reset_drop_metadata_update_attempts() {
        DROP_METADATA_UPDATE_ATTEMPTS.store(0, Ordering::Relaxed);
    }

    #[test]
    fn readonly_file_drop_skips_metadata_update_after_read() {
        let _guard = DROP_METADATA_UPDATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_drop_metadata_update_attempts();
        let metadata_updates = Arc::new(AtomicUsize::new(0));
        let filesystem = Filesystem::new(Arc::new(TestFilesystem {
            name: "readonly-file-drop-test",
            readonly: true,
        }));
        let mountpoint = Mountpoint::new_root(&filesystem);
        let location = Location::new(
            mountpoint,
            test_entry(Arc::new(MetadataTrackingTestFile::new(
                &READONLY_TEST_FILESYSTEM,
                metadata_updates.clone(),
            ))),
        );

        let file = File::new(FileBackend::new_direct(location), FileFlags::READ);
        let mut byte = [0];
        assert_eq!(file.read(&mut byte[..]).unwrap(), 1);
        drop(file);

        assert_eq!(metadata_updates.load(Ordering::Relaxed), 0);
        assert_eq!(DROP_METADATA_UPDATE_ATTEMPTS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn writable_file_drop_updates_metadata_after_read() {
        let _guard = DROP_METADATA_UPDATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_drop_metadata_update_attempts();
        let metadata_updates = Arc::new(AtomicUsize::new(0));
        let filesystem = Filesystem::new(Arc::new(TestFilesystem {
            name: "writable-file-drop-test",
            readonly: false,
        }));
        let mountpoint = Mountpoint::new_root(&filesystem);
        let location = Location::new(
            mountpoint,
            test_entry(Arc::new(MetadataTrackingTestFile::new(
                &WRITABLE_TEST_FILESYSTEM,
                metadata_updates.clone(),
            ))),
        );

        let file = File::new(FileBackend::new_direct(location), FileFlags::READ);
        let mut byte = [0];
        assert_eq!(file.read(&mut byte[..]).unwrap(), 1);
        drop(file);

        assert_eq!(metadata_updates.load(Ordering::Relaxed), 1);
        assert_eq!(DROP_METADATA_UPDATE_ATTEMPTS.load(Ordering::Relaxed), 1);
    }
}
