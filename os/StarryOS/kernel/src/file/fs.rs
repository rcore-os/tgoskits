use alloc::{borrow::Cow, string::ToString, sync::Arc};
use core::{
    ffi::c_int,
    hint::likely,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use ax_fs_ng::vfs::{
    FileBackend, FileFlags, FileLocation, FsContext, FsContextOperationView, LocationOperationView,
    OpenedDirectory, current_fs_context,
};
use ax_io::{Seek, SeekFrom};
use ax_sync::PiMutex;
use axfs_ng_vfs::{FsIoEvents, FsPollable, Metadata, NodeFlags};
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::general::{AT_EMPTY_PATH, AT_FDCWD, AT_SYMLINK_NOFOLLOW, O_APPEND, O_EXCL};
use starry_vm::VmPtr;

use super::{FileLike, Kstat, get_file_like};
use crate::{
    file::{IoDst, IoSrc},
    pseudofs::Device,
    task::{
        current_user_task,
        future::{block_on_user, poll_io_for},
    },
};

// FusionIO/directFS atomic-write toggle used by MySQL.
const DFS_IOCTL_ATOMIC_WRITE_SET: u32 = 0x4004_9502;

pub fn with_fs<R>(
    dirfd: c_int,
    operation: impl for<'operation> FnOnce(FsContextOperationView<'operation>) -> AxResult<R>,
) -> AxResult<R> {
    let fs_context = current_fs_context();
    if dirfd == AT_FDCWD {
        fs_context.lock().with_operation_scope(operation)
    } else {
        let directory = Directory::from_fd(dirfd)?;
        let fs = fs_context.lock();
        directory.with_fs_context(&fs, operation)
    }
}

pub enum ResolveAtResult {
    File(FileLocation),
    Directory(Arc<Directory>),
    Other(Arc<dyn FileLike>),
}

impl ResolveAtResult {
    /// Runs one restricted operation while retaining the exact generation lease.
    pub fn with_operation<T>(
        &self,
        operation: impl for<'operation> FnOnce(LocationOperationView<'operation>) -> AxResult<T>,
    ) -> AxResult<T> {
        match self {
            Self::File(location) => location.with_operation(operation),
            Self::Directory(directory) => directory.with_operation(operation),
            Self::Other(_) => Err(AxError::BadFileDescriptor),
        }
    }

    pub fn stat(&self) -> AxResult<Kstat> {
        match self {
            Self::File(file) => file.with_operation(|view| {
                view.metadata().map(|metadata| metadata_to_kstat(&metadata))
            }),
            Self::Directory(directory) => directory.stat(),
            Self::Other(file_like) => file_like.stat(),
        }
    }
}

pub fn resolve_at(dirfd: c_int, path: Option<&str>, flags: u32) -> AxResult<ResolveAtResult> {
    match path {
        Some("") | None => {
            if flags & AT_EMPTY_PATH == 0 {
                return Err(AxError::NotFound);
            }
            let file_like = get_file_like(dirfd)?;
            Ok(if let Ok(file) = file_like.clone().downcast_arc::<File>() {
                // Use location() directly: backend() rejects PATH-only fds
                // (BadFileDescriptor) which would break fstat(O_PATH-fd).
                // man "O_PATH": fstat(2) is in the allowed-operations list.
                // Fixes bug-open-path-fstat-ebadf.
                ResolveAtResult::File(file.inner().file_location())
            } else if let Ok(directory) = file_like.clone().downcast_arc::<Directory>() {
                ResolveAtResult::Directory(directory)
            } else {
                ResolveAtResult::Other(file_like)
            })
        }
        Some(path) => {
            let dirfd = if path.starts_with('/') {
                AT_FDCWD
            } else {
                dirfd
            };
            with_fs(dirfd, |fs| {
                if flags & AT_SYMLINK_NOFOLLOW != 0 {
                    fs.resolve_file_location_no_follow(path)
                } else {
                    fs.resolve_file_location(path)
                }
                .map(ResolveAtResult::File)
            })
        }
    }
}

pub fn metadata_to_kstat(metadata: &Metadata) -> Kstat {
    let ty = metadata.node_type as u8;
    let perm = metadata.mode.bits() as u32;
    let mode = ((ty as u32) << 12) | perm;
    Kstat {
        dev: metadata.device,
        ino: metadata.inode,
        mode,
        nlink: metadata.nlink as _,
        uid: metadata.uid,
        gid: metadata.gid,
        size: metadata.size,
        blksize: metadata.block_size as _,
        blocks: metadata.blocks,
        rdev: metadata.rdev,
        atime: metadata.atime,
        mtime: metadata.mtime,
        ctime: metadata.ctime,
    }
}

/// File wrapper for `ax_fs_ng::fops::File`.
pub struct File {
    inner: ax_fs_ng::File,
    open_flags: u32,
    nonblock: AtomicBool,
    append: AtomicBool,
}

impl File {
    pub fn new(inner: ax_fs_ng::File, open_flags: u32) -> Self {
        Self {
            inner,
            open_flags,
            nonblock: AtomicBool::new(false),
            append: AtomicBool::new(open_flags & O_APPEND != 0),
        }
    }

    pub fn inner(&self) -> &ax_fs_ng::File {
        &self.inner
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let exclusive = self.open_flags & O_EXCL != 0;
        let _ = self.inner.with_node::<Device, _>(|device| {
            device.inner().close(exclusive);
            Ok(())
        });
    }
}

impl File {
    fn is_blocking(&self) -> bool {
        self.inner
            .node_flags()
            .is_ok_and(|flags| flags.contains(NodeFlags::BLOCKING))
    }
}

fn path_for(file: &ax_fs_ng::File) -> Cow<'static, str> {
    file.absolute_path()
        .map_or_else(|_| "<error>".into(), |path| Cow::Owned(path.to_string()))
}

fn fs_events_to_io(events: FsIoEvents) -> IoEvents {
    IoEvents::from_bits_truncate(events.bits())
}

fn io_events_to_fs(events: IoEvents) -> FsIoEvents {
    FsIoEvents::from_bits_truncate(events.bits())
}

impl FileLike for File {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        let inner = self.inner();
        if likely(self.is_blocking()) {
            inner.read(dst)
        } else {
            let task = current_user_task();
            block_on_user(
                &task,
                poll_io_for(&task, self, IoEvents::IN, self.nonblocking(), || {
                    inner.read(&mut *dst)
                }),
            )
        }
    }

    fn write(&self, src: &mut IoSrc) -> AxResult<usize> {
        let mut inner = self.inner();
        if self.append() {
            inner.seek(SeekFrom::End(0))?;
        }
        let result = if likely(self.is_blocking()) {
            inner.write(src)
        } else {
            let task = current_user_task();
            block_on_user(
                &task,
                poll_io_for(&task, self, IoEvents::OUT, self.nonblocking(), || {
                    inner.write(&mut *src)
                }),
            )
        };
        if let Ok(bytes) = result
            && bytes > 0
        {
            let path = path_for(inner).into_owned();
            crate::file::inotify::notify_modify_path(&path);
        }
        result
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(metadata_to_kstat(&self.inner().metadata()?))
    }

    fn inode_key(&self) -> Option<(u64, u64)> {
        let m = self.inner().metadata().ok()?;
        Some((m.device, m.inode))
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        match cmd {
            DFS_IOCTL_ATOMIC_WRITE_SET => {
                let _enabled: u32 = (arg as *const u32).vm_read()?;
                Ok(0)
            }
            _ => self.inner().backend()?.ioctl(cmd, arg),
        }
    }

    fn file_mmap(&self) -> AxResult<(FileBackend, FileFlags)> {
        Ok((self.inner().backend()?.clone(), self.inner().flags()))
    }

    fn set_nonblocking(&self, flag: bool) -> AxResult {
        self.nonblock.store(flag, Ordering::Release);
        Ok(())
    }

    fn nonblocking(&self) -> bool {
        self.nonblock.load(Ordering::Acquire)
    }

    fn append(&self) -> bool {
        self.append.load(Ordering::Acquire)
    }

    fn set_append(&self, flag: bool) -> AxResult {
        self.append.store(flag, Ordering::Release);
        self.inner().set_flag(FileFlags::APPEND, flag);
        Ok(())
    }

    fn open_flags(&self) -> u32 {
        self.open_flags
    }

    fn path(&self) -> Cow<'_, str> {
        path_for(&self.inner)
    }

    fn from_fd(fd: c_int) -> AxResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        let any = get_file_like(fd)?;
        if let Ok(file) = any.clone().downcast_arc::<File>() {
            return Ok(file);
        }
        // Memfd wraps a regular File and is meant to behave as one for
        // every read-data / size-changing syscall (lseek, fallocate,
        // sendfile, pread, pwrite, ...). Hand back the inner File so
        // those paths don't trip on the wrapper. Seal-aware ftruncate
        // already takes a separate Memfd::from_fd branch upstream.
        if let Ok(memfd) = any.clone().downcast_arc::<crate::file::memfd::Memfd>() {
            return Ok(memfd.inner().clone());
        }
        Err(if any.is::<Directory>() {
            AxError::IsADirectory
        } else {
            AxError::InvalidInput
        })
    }
}
impl Pollable for File {
    fn poll(&self) -> IoEvents {
        fs_events_to_io(self.inner().poll())
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.inner().register(context, io_events_to_fs(events));
    }
}

/// Directory wrapper for `ax_fs_ng::fops::Directory`.
pub struct Directory {
    inner: OpenedDirectory,
    pub offset: PiMutex<u64>,
    /// Original open flags (used by fd_is_path / sys_fchmodat to detect
    /// O_PATH on directory descriptors — open(dir, O_PATH|O_DIRECTORY)
    /// must reject fchmod just like O_PATH on a regular file).
    open_flags: u32,
}

impl Directory {
    pub fn new(inner: OpenedDirectory, open_flags: u32) -> Self {
        Self {
            inner,
            offset: PiMutex::new(0),
            open_flags,
        }
    }

    fn with_fs_context<T>(
        &self,
        context: &FsContext,
        operation: impl for<'operation> FnOnce(FsContextOperationView<'operation>) -> AxResult<T>,
    ) -> AxResult<T> {
        self.inner.with_fs_context(context, operation)
    }

    pub fn set_context_current_dir(&self, context: &mut FsContext) -> AxResult<()> {
        self.inner.set_context_current_dir(context)
    }

    /// Runs one restricted operation while retaining the directory generation lease.
    pub fn with_operation<T>(
        &self,
        operation: impl for<'operation> FnOnce(LocationOperationView<'operation>) -> AxResult<T>,
    ) -> AxResult<T> {
        self.inner.with_operation(operation)
    }
}

impl FileLike for Directory {
    fn read(&self, _dst: &mut IoDst) -> AxResult<usize> {
        Err(AxError::IsADirectory)
    }

    fn write(&self, _src: &mut IoSrc) -> AxResult<usize> {
        // Directories cannot be opened for writing, so any write attempt
        // means the fd is not open for writing → EBADF.
        // Linux VFS checks FMODE_WRITE before reaching the filesystem layer.
        Err(AxError::BadFileDescriptor)
    }

    fn stat(&self) -> AxResult<Kstat> {
        self.with_operation(|view| Ok(metadata_to_kstat(&view.metadata()?)))
    }

    fn inode_key(&self) -> Option<(u64, u64)> {
        self.with_operation(|view| {
            let metadata = view.metadata()?;
            Ok((metadata.device, metadata.inode))
        })
        .ok()
    }

    fn open_flags(&self) -> u32 {
        self.open_flags
    }

    fn path(&self) -> Cow<'_, str> {
        self.with_operation(|view| {
            Ok(view
                .absolute_path()
                .map_or_else(|_| "<error>".to_string(), |path| path.to_string()))
        })
        .map(Cow::Owned)
        .unwrap_or(Cow::Borrowed("<error>"))
    }

    fn from_fd(fd: c_int) -> AxResult<Arc<Self>> {
        get_file_like(fd)?
            .downcast_arc()
            .map_err(|_| AxError::NotADirectory)
    }
}
impl Pollable for Directory {
    fn poll(&self) -> IoEvents {
        fs_events_to_io(FsIoEvents::IN | FsIoEvents::OUT)
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}
