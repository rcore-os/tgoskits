use alloc::sync::Arc;
use core::{any::Any, ffi::c_int, task::Context};

use axerrno::{LinuxError, LinuxResult};
use axfs_ng::{FS_CONTEXT, FsContext};
use axfs_ng_vfs::{Location, Metadata};
use axio::{IoEvents, Pollable, Read, Write};
use axsync::{Mutex, MutexGuard, RawMutex};
use linux_raw_sys::general::{AT_EMPTY_PATH, AT_FDCWD, AT_SYMLINK_NOFOLLOW};

use super::{FileLike, Kstat, get_file_like};

pub fn with_fs<R>(
    dirfd: c_int,
    f: impl FnOnce(&mut FsContext<RawMutex>) -> LinuxResult<R>,
) -> LinuxResult<R> {
    let mut fs = FS_CONTEXT.lock();
    if dirfd == AT_FDCWD {
        f(&mut fs)
    } else {
        let dir = Directory::from_fd(dirfd)?.inner.clone();
        f(&mut fs.with_current_dir(dir)?)
    }
}

pub enum ResolveAtResult {
    File(Location<RawMutex>),
    Other(Arc<dyn FileLike>),
}

impl ResolveAtResult {
    pub fn into_file(self) -> Option<Location<RawMutex>> {
        match self {
            Self::File(file) => Some(file),
            Self::Other(_) => None,
        }
    }

    pub fn stat(&self) -> LinuxResult<Kstat> {
        match self {
            Self::File(file) => file.metadata().map(|it| metadata_to_kstat(&it)),
            Self::Other(file_like) => file_like.stat(),
        }
    }
}

pub fn resolve_at(dirfd: c_int, path: Option<&str>, flags: u32) -> LinuxResult<ResolveAtResult> {
    match path {
        Some("") | None => {
            if flags & AT_EMPTY_PATH == 0 {
                return Err(LinuxError::ENOENT);
            }
            let file_like = get_file_like(dirfd)?;
            let f = file_like.clone().into_any();
            Ok(if let Some(file) = f.downcast_ref::<File>() {
                ResolveAtResult::File(file.inner.lock().backend()?.location().clone())
            } else if let Some(dir) = f.downcast_ref::<Directory>() {
                ResolveAtResult::File(dir.inner().clone())
            } else {
                ResolveAtResult::Other(file_like)
            })
        }
        Some(path) => with_fs(dirfd, |fs| {
            if flags & AT_SYMLINK_NOFOLLOW != 0 {
                fs.resolve_no_follow(path)
            } else {
                fs.resolve(path)
            }
            .map(ResolveAtResult::File)
        }),
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

/// File wrapper for `axfs::fops::File`.
pub struct File {
    inner: Arc<Mutex<axfs_ng::File<RawMutex>>>,
}

impl File {
    pub fn new(inner: axfs_ng::File<RawMutex>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    pub fn inner(&self) -> MutexGuard<'_, axfs_ng::File<RawMutex>> {
        self.inner.lock()
    }
}

impl FileLike for File {
    fn read(&self, buf: &mut [u8]) -> LinuxResult<usize> {
        self.inner().read(buf)
    }

    fn write(&self, buf: &[u8]) -> LinuxResult<usize> {
        self.inner().write(buf)
    }

    fn stat(&self) -> LinuxResult<Kstat> {
        Ok(metadata_to_kstat(&self.inner().location().metadata()?))
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> LinuxResult<usize> {
        self.inner().backend()?.location().ioctl(cmd, arg)
    }

    fn from_fd(fd: c_int) -> LinuxResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        let any = get_file_like(fd)?.into_any();

        any.downcast::<Self>().map_err(|any| {
            if any.is::<Directory>() {
                LinuxError::EISDIR
            } else {
                LinuxError::ESPIPE
            }
        })
    }
}
impl Pollable for File {
    fn poll(&self) -> IoEvents {
        self.inner().location().poll()
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.inner().location().register(context, events);
    }
}

/// Directory wrapper for `axfs::fops::Directory`.
pub struct Directory {
    inner: Location<RawMutex>,
    pub offset: Mutex<u64>,
}

impl Directory {
    pub fn new(inner: Location<RawMutex>) -> Self {
        Self {
            inner,
            offset: Mutex::new(0),
        }
    }

    /// Get the inner node of the directory.
    pub fn inner(&self) -> &Location<RawMutex> {
        &self.inner
    }
}

impl FileLike for Directory {
    fn read(&self, _buf: &mut [u8]) -> LinuxResult<usize> {
        Err(LinuxError::EBADF)
    }

    fn write(&self, _buf: &[u8]) -> LinuxResult<usize> {
        Err(LinuxError::EBADF)
    }

    fn stat(&self) -> LinuxResult<Kstat> {
        Ok(metadata_to_kstat(&self.inner.metadata()?))
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn from_fd(fd: c_int) -> LinuxResult<Arc<Self>> {
        get_file_like(fd)?
            .into_any()
            .downcast::<Self>()
            .map_err(|_| LinuxError::ENOTDIR)
    }
}
impl Pollable for Directory {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}
