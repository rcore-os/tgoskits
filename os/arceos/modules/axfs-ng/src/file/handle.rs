use alloc::sync::Arc;
use core::{
    any::Any,
    sync::atomic::{AtomicU8, Ordering},
    task::Context,
};

use ax_io::{SeekFrom, prelude::*};
use axfs_ng_vfs::{
    FsIoEvents, FsPollable, Location, Metadata, NodeFlags, NodeOps, StatFs, VfsError, VfsResult,
    path::{Path, PathBuf},
};

use super::{
    cache::CachedFile,
    location::{FileLocation, GenerationBoundLocation, UnmanagedLocation},
    open::{FileFlags, OpenOptions, OpenResult},
    operation::{LocationOperationView, MountIdentity},
};
use crate::{
    fs_core::FsContext,
    lifecycle::{FsOpenHandleLease, FsOperationLease, FsRuntimeError},
    os::sync::PiMutex,
};

/// Low-level interface for file operations.
#[derive(Clone)]
pub enum FileBackend {
    /// File I/O goes through the page cache.
    Cached(CachedFile),
    /// File I/O bypasses the page cache and hits the VFS directly.
    Direct(UnmanagedLocation),
    /// Direct I/O tied to one mounted filesystem generation.
    ManagedDirect(ManagedDirectBackend),
}

/// A direct-I/O behavior handle retaining one counted filesystem lease.
#[derive(Clone, Debug)]
pub struct ManagedDirectBackend {
    location: GenerationBoundLocation,
    lease: FsOpenHandleLease,
}

impl ManagedDirectBackend {
    fn new(location: Location, lease: FsOpenHandleLease) -> Self {
        Self {
            location: GenerationBoundLocation::from_handle(location, &lease),
            lease,
        }
    }

    fn begin_operation(&self) -> Result<FsOperationLease, FsRuntimeError> {
        self.lease.begin_operation()
    }

    fn location(&self) -> &Location {
        self.location.location()
    }

    fn file_location(&self) -> GenerationBoundLocation {
        self.location.clone()
    }

    /// Verifies that this backend still belongs to the mounted generation.
    pub fn validate_generation(&self) -> Result<(), FsRuntimeError> {
        self.lease.validate()
    }
}

impl FileBackend {
    pub(crate) fn new_direct(
        location: Location,
        lease: Option<FsOpenHandleLease>,
    ) -> VfsResult<Self> {
        match lease {
            Some(lease) => Ok(Self::ManagedDirect(ManagedDirectBackend::new(
                location, lease,
            ))),
            None => UnmanagedLocation::try_new(location)
                .map(Self::Direct)
                .map_err(|_| VfsError::InvalidInput),
        }
    }

    pub(crate) fn new_cached(
        location: Location,
        lease: Option<FsOpenHandleLease>,
    ) -> VfsResult<Self> {
        let cached = match lease {
            Some(lease) => CachedFile::get_or_create_generation_bound(
                GenerationBoundLocation::from_handle(location, &lease),
                lease,
            )?,
            None => CachedFile::get_or_create(
                UnmanagedLocation::try_new(location).map_err(|_| VfsError::InvalidInput)?,
            )?,
        };
        Ok(Self::Cached(cached))
    }

    /// Returns the backend-visible file length.
    pub fn len(&self) -> VfsResult<u64> {
        match self {
            Self::Cached(cached) => {
                let _operation = cached.begin_operation()?;
                Ok(cached.len())
            }
            Self::Direct(loc) => loc.as_inner().len(),
            Self::ManagedDirect(managed) => {
                let _operation = managed
                    .begin_operation()
                    .map_err(FsRuntimeError::into_ax_error)?;
                managed.location().len()
            }
        }
    }

    /// Returns whether the backend-visible file length is zero.
    pub fn is_empty(&self) -> VfsResult<bool> {
        self.len().map(|len| len == 0)
    }

    /// Reads data from the file at `offset` into `dst`.
    pub fn read_at(&self, dst: impl Write + IoBufMut, offset: u64) -> VfsResult<usize> {
        match self {
            Self::Cached(cached) => cached.read_at(dst, offset),
            Self::Direct(unmanaged) => read_direct_at(unmanaged.as_inner(), dst, offset),
            Self::ManagedDirect(managed) => {
                let _operation = managed
                    .begin_operation()
                    .map_err(FsRuntimeError::into_ax_error)?;
                read_direct_at(managed.location(), dst, offset)
            }
        }
    }

    /// Writes `src` to the file at `offset`.
    pub fn write_at(&self, src: impl Read + IoBuf, offset: u64) -> VfsResult<usize> {
        match self {
            Self::Cached(cached) => cached.write_at(src, offset),
            Self::Direct(unmanaged) => write_direct_at(unmanaged.as_inner(), src, offset),
            Self::ManagedDirect(managed) => {
                let _operation = managed
                    .begin_operation()
                    .map_err(FsRuntimeError::into_ax_error)?;
                write_direct_at(managed.location(), src, offset)
            }
        }
    }

    /// Appends `src` to the end of the file. Returns `(bytes_written, new_end)`.
    pub fn append(&self, src: impl Read + IoBuf) -> VfsResult<(usize, u64)> {
        match self {
            Self::Cached(cached) => cached.append(src),
            Self::Direct(unmanaged) => append_direct(unmanaged.as_inner(), src),
            Self::ManagedDirect(managed) => {
                let _operation = managed
                    .begin_operation()
                    .map_err(FsRuntimeError::into_ax_error)?;
                append_direct(managed.location(), src)
            }
        }
    }

    pub(crate) fn location_ref(&self) -> &Location {
        match self {
            Self::Cached(cached) => cached.location_ref(),
            Self::Direct(loc) => loc.as_inner(),
            Self::ManagedDirect(managed) => managed.location(),
        }
    }

    /// Returns the location together with its generation or unmanaged proof.
    pub fn file_location(&self) -> FileLocation {
        match self {
            Self::Cached(cached) => cached.file_location(),
            Self::Direct(location) => FileLocation::Unmanaged(location.clone()),
            Self::ManagedDirect(managed) => FileLocation::Managed(managed.file_location()),
        }
    }

    /// Runs one restricted operation while retaining backend authority.
    pub fn with_operation<T>(
        &self,
        operation: impl for<'operation> FnOnce(LocationOperationView<'operation>) -> VfsResult<T>,
    ) -> VfsResult<T> {
        match self {
            Self::Cached(cached) => cached.with_operation(operation),
            Self::Direct(location) => {
                operation(LocationOperationView::unmanaged(location.as_inner()))
            }
            Self::ManagedDirect(managed) => {
                let operation_lease = managed
                    .begin_operation()
                    .map_err(FsRuntimeError::into_ax_error)?;
                operation(LocationOperationView::managed(
                    managed.location(),
                    &operation_lease,
                ))
            }
        }
    }

    /// Returns metadata while retaining backend generation authority.
    pub fn metadata(&self) -> VfsResult<Metadata> {
        self.with_operation(|view| view.metadata())
    }

    /// Returns the absolute namespace path as an owned value.
    pub fn absolute_path(&self) -> VfsResult<PathBuf> {
        self.with_operation(|view| view.absolute_path())
    }

    /// Returns the inode number while retaining backend generation authority.
    pub fn inode(&self) -> VfsResult<u64> {
        self.with_operation(|view| Ok(view.inode()))
    }

    /// Returns node behavior flags.
    pub fn node_flags(&self) -> VfsResult<NodeFlags> {
        self.with_operation(|view| Ok(view.node_flags()))
    }

    /// Dispatches a backend node ioctl.
    pub fn ioctl(&self, command: u32, argument: usize) -> VfsResult<usize> {
        self.with_operation(|view| view.ioctl(command, argument))
    }

    /// Returns containing-filesystem statistics and mount device identity.
    pub fn filesystem_statistics(&self) -> VfsResult<(StatFs, u64)> {
        self.with_operation(|view| Ok((view.filesystem_statistics()?, view.mount_device())))
    }

    /// Flushes the containing filesystem.
    pub fn flush_filesystem(&self) -> VfsResult<()> {
        self.with_operation(|view| view.flush_filesystem())
    }

    /// Tests whether this backend belongs to `mountpoint`.
    pub fn is_on_mount(&self, mount: &MountIdentity) -> VfsResult<bool> {
        self.with_operation(|view| Ok(view.is_on_mount(mount)))
    }

    /// Runs an operation against a typed node without exposing its entry.
    pub fn with_node<T, R>(
        &self,
        operation: impl for<'node> FnOnce(&'node T) -> VfsResult<R>,
    ) -> VfsResult<R>
    where
        T: NodeOps,
    {
        self.with_operation(|view| view.with_node(operation))
    }

    /// Clones typed node-attached data under generation authority.
    pub fn get_user_data<T>(&self) -> VfsResult<Option<Arc<T>>>
    where
        T: Any + Send + Sync,
    {
        self.with_operation(|view| Ok(view.get_user_data::<T>()))
    }

    /// Flushes cached data (and optionally metadata) to disk.
    pub fn sync(&self, data_only: bool) -> VfsResult<()> {
        match self {
            Self::Cached(cached) => cached.sync(data_only),
            Self::Direct(loc) => loc.as_inner().entry().as_file()?.sync(data_only),
            Self::ManagedDirect(managed) => {
                let _operation = managed
                    .begin_operation()
                    .map_err(FsRuntimeError::into_ax_error)?;
                managed.location().entry().as_file()?.sync(data_only)
            }
        }
    }

    /// Truncates or extends the file to `len` bytes.
    pub fn set_len(&self, len: u64) -> VfsResult<()> {
        match self {
            Self::Cached(cached) => cached.set_len(len),
            Self::Direct(loc) => loc.as_inner().entry().as_file()?.set_len(len),
            Self::ManagedDirect(managed) => {
                let _operation = managed
                    .begin_operation()
                    .map_err(FsRuntimeError::into_ax_error)?;
                managed.location().entry().as_file()?.set_len(len)
            }
        }
    }

    pub(crate) fn set_len_during(
        &self,
        len: u64,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<()> {
        match self {
            Self::Cached(cached) => cached.set_len_during(len, operation),
            Self::Direct(location) => location.as_inner().entry().as_file()?.set_len(len),
            Self::ManagedDirect(managed) => {
                managed
                    .location
                    .validate_operation(operation.ok_or(VfsError::BadState)?)
                    .map_err(FsRuntimeError::into_ax_error)?;
                managed.location().entry().as_file()?.set_len(len)
            }
        }
    }

    fn is_generation_bound(&self) -> bool {
        match self {
            Self::Cached(cached) => cached.is_generation_bound(),
            Self::Direct(_) => false,
            Self::ManagedDirect(_) => true,
        }
    }
}

fn read_direct_at(
    loc: &Location,
    mut dst: impl Write + IoBufMut,
    mut offset: u64,
) -> VfsResult<usize> {
    let mut total = 0;
    while !dst.is_full() {
        let read = match dst.read_from(&mut ax_io::read_fn(|buf| {
            loc.entry().as_file()?.read_at(buf, offset).inspect(|read| {
                offset += *read as u64;
            })
        })) {
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

fn write_direct_at(
    loc: &Location,
    mut src: impl Read + IoBuf,
    mut offset: u64,
) -> VfsResult<usize> {
    let mut total = 0;
    let mut buf = [0; ax_io::DEFAULT_BUF_SIZE];
    while !src.is_empty() {
        let limit = src.remaining().min(buf.len());
        let read = src.read(&mut buf[..limit])?;
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

fn append_direct(loc: &Location, mut src: impl Read + IoBuf) -> VfsResult<(usize, u64)> {
    let mut total = 0;
    let mut end = loc.entry().as_file()?.len()?;
    while src.remaining() > 0 {
        let chunk = src.remaining().min(ax_io::DEFAULT_BUF_SIZE);
        let written = match src.write_to(&mut ax_io::write_fn(|buf| {
            loc.entry().as_file()?.append(buf).map(|(n, offset)| {
                end = offset;
                n
            })
        })) {
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

/// Provides `std::fs::File`-like interface.
pub struct File {
    inner: FileBackend,
    lease: Option<FsOpenHandleLease>,
    flags: AtomicU8,
    position: Option<PiMutex<u64>>,
    access_flags: AtomicU8,
}

impl File {
    /// Creates a file from a backend owned by a non-detachable filesystem.
    ///
    /// Generation-bound backends must be created through [`OpenOptions`] so
    /// the resulting file owns an externally visible open-handle lease.
    pub fn from_unmanaged(inner: FileBackend, flags: FileFlags) -> VfsResult<Self> {
        if inner.is_generation_bound() {
            return Err(VfsError::InvalidInput);
        }
        Ok(Self::new_with_lease(inner, flags, None))
    }

    pub(crate) fn new_with_lease(
        inner: FileBackend,
        flags: FileFlags,
        lease: Option<FsOpenHandleLease>,
    ) -> Self {
        // man 2 open: "The file offset is set to the beginning of the file"
        // — initial position is always 0, regardless of O_APPEND.
        // O_APPEND only relocates the offset BEFORE EACH WRITE (handled in
        // `write()` via the `access(FileFlags::APPEND)` branch). Setting
        // initial position to EOF would break read() on RDONLY|APPEND
        // (read sees EOF immediately) — see bug-open-rdonly-append-promotes-rw.
        let position = if inner.location_ref().flags().contains(NodeFlags::STREAM) {
            None
        } else {
            Some(PiMutex::new(0))
        };
        Self {
            inner,
            lease,
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
        self.validate_generation()
            .map_err(FsRuntimeError::into_ax_error)?;
        if self.flags().contains(flags) && !self.is_path() {
            if self.inner.with_operation(|view| Ok(view.is_readonly()))?
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

    /// Returns the location together with its generation or unmanaged proof.
    pub fn file_location(&self) -> FileLocation {
        self.inner.file_location()
    }

    /// Runs one restricted location operation for this open file.
    ///
    /// The callback is higher-ranked so its [`LocationOperationView`] cannot be
    /// returned or retained after the generation operation lease is released.
    ///
    /// ```compile_fail
    /// use ax_fs_ng::file::File;
    ///
    /// fn escape_operation_view(file: &File) {
    ///     let _escaped = file.with_operation(|view| Ok(view));
    /// }
    /// ```
    pub fn with_operation<T>(
        &self,
        operation: impl for<'operation> FnOnce(LocationOperationView<'operation>) -> VfsResult<T>,
    ) -> VfsResult<T> {
        let operation_lease = self.begin_operation()?;
        let view = match operation_lease.as_ref() {
            Some(operation_lease) => {
                LocationOperationView::managed(self.inner.location_ref(), operation_lease)
            }
            None => LocationOperationView::unmanaged(self.inner.location_ref()),
        };
        operation(view)
    }

    /// Returns metadata for this open file.
    pub fn metadata(&self) -> VfsResult<Metadata> {
        self.with_operation(|view| view.metadata())
    }

    /// Returns the backend-visible file length.
    pub fn len(&self) -> VfsResult<u64> {
        self.access(FileFlags::empty())?.len()
    }

    /// Returns whether the backend-visible file is empty.
    pub fn is_empty(&self) -> VfsResult<bool> {
        self.len().map(|len| len == 0)
    }

    /// Returns node behavior flags.
    pub fn node_flags(&self) -> VfsResult<NodeFlags> {
        self.with_operation(|view| Ok(view.node_flags()))
    }

    /// Returns the absolute namespace path as an owned value.
    pub fn absolute_path(&self) -> VfsResult<PathBuf> {
        self.with_operation(|view| view.absolute_path())
    }

    /// Dispatches an ioctl against this open file's node.
    pub fn ioctl(&self, command: u32, argument: usize) -> VfsResult<usize> {
        self.access(FileFlags::empty())?.ioctl(command, argument)
    }

    /// Returns containing-filesystem statistics and mount device identity.
    pub fn filesystem_statistics(&self) -> VfsResult<(StatFs, u64)> {
        self.with_operation(|view| Ok((view.filesystem_statistics()?, view.mount_device())))
    }

    /// Flushes the containing filesystem.
    pub fn flush_filesystem(&self) -> VfsResult<()> {
        self.with_operation(|view| view.flush_filesystem())
    }

    /// Tests whether this file belongs to `mountpoint`.
    pub fn is_on_mount(&self, mount: &MountIdentity) -> VfsResult<bool> {
        self.with_operation(|view| Ok(view.is_on_mount(mount)))
    }

    /// Runs an operation against a typed node without exposing its entry.
    pub fn with_node<T, R>(
        &self,
        operation: impl for<'node> FnOnce(&'node T) -> VfsResult<R>,
    ) -> VfsResult<R>
    where
        T: NodeOps,
    {
        self.with_operation(|view| view.with_node(operation))
    }

    /// Clones typed node-attached data under the file operation lease.
    pub fn get_user_data<T>(&self) -> VfsResult<Option<Arc<T>>>
    where
        T: Any + Send + Sync,
    {
        self.with_operation(|view| Ok(view.get_user_data::<T>()))
    }

    /// Reads a number of bytes starting from a given offset.
    pub fn read_at(&self, dst: impl Write + IoBufMut, offset: u64) -> VfsResult<usize> {
        self.access(FileFlags::READ)?.read_at(dst, offset)
    }

    /// Writes a number of bytes starting from a given offset.
    pub fn write_at(&self, src: impl Read + IoBuf, offset: u64) -> VfsResult<usize> {
        self.access(FileFlags::WRITE)?.write_at(src, offset)
    }

    /// Attempts to sync OS-internal file content and metadata to disk.
    ///
    /// If `data_only` is `true`, only the file data is synced, not the
    /// metadata.
    pub fn sync(&self, data_only: bool) -> VfsResult<()> {
        self.access(FileFlags::empty())?;
        self.inner.sync(data_only)
    }

    /// Changes the backend-visible file length.
    pub fn set_len(&self, len: u64) -> VfsResult<()> {
        self.access(FileFlags::WRITE)?.set_len(len)
    }

    /// Reads data from the current position, advancing the cursor.
    pub fn read(&self, dst: impl Write + IoBufMut) -> ax_io::Result<usize> {
        self.access_flags.fetch_or(1, Ordering::AcqRel);
        if let Some(pos) = self.position.as_ref() {
            let mut pos = pos.lock();
            self.read_at(dst, *pos).inspect(|n| {
                *pos += *n as u64;
            })
        } else {
            self.read_at(dst, 0)
        }
    }

    /// Writes data at the current position (or appends), advancing the cursor.
    pub fn write(&self, src: impl Read + IoBuf) -> ax_io::Result<usize> {
        self.access_flags.fetch_or(3, Ordering::AcqRel);
        // WRITE bit is mandatory for any write path, regardless of whether
        // APPEND is set. Otherwise O_RDONLY|O_APPEND fd would silently
        // succeed writes (since access(APPEND) only checks the APPEND bit).
        // Fixes bug-open-rdonly-append-promotes-rw (the part inside axfs).
        self.access(FileFlags::WRITE)?;
        if let Some(pos) = self.position.as_ref() {
            let mut pos = pos.lock();
            if let Ok(f) = self.access(FileFlags::APPEND) {
                f.append(src).map(|(written, new_size)| {
                    *pos = new_size;
                    written
                })
            } else {
                self.write_at(src, *pos).inspect(|n| {
                    *pos += *n as u64;
                })
            }
        } else {
            self.write_at(src, 0)
        }
    }

    /// Flushes any internally buffered data. Currently a no-op.
    pub fn flush(&self) -> ax_io::Result {
        self.access(FileFlags::empty())?;
        Ok(())
    }

    /// Verifies that this file belongs to the currently mounted generation.
    pub fn validate_generation(&self) -> Result<(), FsRuntimeError> {
        self.lease
            .as_ref()
            .map(FsOpenHandleLease::validate)
            .transpose()
            .map(|_| ())
    }

    fn begin_operation(&self) -> VfsResult<Option<FsOperationLease>> {
        self.lease
            .as_ref()
            .map(FsOpenHandleLease::begin_operation)
            .transpose()
            .map_err(|error| error.into_ax_error())
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
        self.access(FileFlags::empty())?;

        if let Some(guard) = self.position.as_ref() {
            let mut guard = guard.lock();
            let new_pos = match pos {
                SeekFrom::Start(pos) => pos,
                SeekFrom::End(off) => {
                    let size = self.inner.len()?;
                    size.checked_add_signed(off).ok_or(VfsError::InvalidInput)?
                }
                SeekFrom::Current(off) => guard
                    .checked_add_signed(off)
                    .ok_or(VfsError::InvalidInput)?,
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
        if self.validate_generation().is_err() {
            return FsIoEvents::ERR | FsIoEvents::HUP;
        }
        self.with_operation(|view| Ok(view.poll()))
            .unwrap_or(FsIoEvents::ERR | FsIoEvents::HUP)
    }

    fn register(&self, context: &mut Context<'_>, events: FsIoEvents) {
        if self.validate_generation().is_err() {
            context.waker().wake_by_ref();
            return;
        }
        if self
            .with_operation(|view| {
                view.register(context, events);
                Ok(())
            })
            .is_err()
        {
            context.waker().wake_by_ref();
        }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let Ok(operation_lease) = self.begin_operation() else {
            return;
        };
        let flags = self.access_flags.load(Ordering::Acquire);
        if flags != 0 {
            let mut update = axfs_ng_vfs::MetadataUpdate::default();
            if flags & 1 != 0 {
                update.atime = Some(crate::os::wall_time());
            }
            if flags & 2 != 0 {
                update.mtime = Some(crate::os::wall_time());
            }
            let view = match operation_lease.as_ref() {
                Some(operation_lease) => {
                    LocationOperationView::managed(self.inner.location_ref(), operation_lease)
                }
                None => LocationOperationView::unmanaged(self.inner.location_ref()),
            };
            if let Err(err) = view.update_metadata(update) {
                warn!("Failed to update file times on drop: {err:?}");
            }
        }
    }
}
