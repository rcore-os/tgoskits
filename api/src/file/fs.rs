use alloc::{borrow::Cow, string::ToString, sync::Arc};
use core::{
    any::Any,
    ffi::c_int,
    hint::likely,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use axerrno::{AxError, AxResult};
use axfs_ng::{FS_CONTEXT, FsContext};
use axfs_ng_vfs::{Location, Metadata, NodeFlags};
use axpoll::{IoEvents, Pollable};
use axsync::Mutex;
use axtask::future::Poller;
use linux_raw_sys::general::{AT_EMPTY_PATH, AT_FDCWD, AT_SYMLINK_NOFOLLOW};

use super::{FileLike, Kstat, get_file_like};
use crate::file::{SealedBuf, SealedBufMut};

pub fn with_fs<R>(dirfd: c_int, f: impl FnOnce(&mut FsContext) -> AxResult<R>) -> AxResult<R> {
    let mut fs = FS_CONTEXT.lock();
    if dirfd == AT_FDCWD {
        f(&mut fs)
    } else {
        let dir = Directory::from_fd(dirfd)?.inner.clone();
        f(&mut fs.with_current_dir(dir)?)
    }
}

pub enum ResolveAtResult {
    File(Location),
    Other(Arc<dyn FileLike>),
}

impl ResolveAtResult {
    pub fn into_file(self) -> Option<Location> {
        match self {
            Self::File(file) => Some(file),
            Self::Other(_) => None,
        }
    }

    pub fn stat(&self) -> AxResult<Kstat> {
        match self {
            Self::File(file) => file.metadata().map(|it| metadata_to_kstat(&it)),
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
            let f = file_like.clone().into_any();
            Ok(if let Some(file) = f.downcast_ref::<File>() {
                ResolveAtResult::File(file.inner().backend()?.location().clone())
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
    inner: axfs_ng::File,
    nonblock: AtomicBool,
}

impl File {
    pub fn new(inner: axfs_ng::File) -> Self {
        Self {
            inner,
            nonblock: AtomicBool::new(false),
        }
    }

    pub fn inner(&self) -> &axfs_ng::File {
        &self.inner
    }

    fn is_blocking(&self) -> bool {
        self.inner.location().flags().contains(NodeFlags::BLOCKING)
    }
}

fn path_for(loc: &Location) -> Cow<'static, str> {
    loc.absolute_path()
        .map_or_else(|_| "<error>".into(), |f| Cow::Owned(f.to_string()))
}

impl FileLike for File {
    fn read(&self, dst: &mut SealedBufMut) -> AxResult<usize> {
        let inner = self.inner();
        if likely(self.is_blocking()) {
            inner.read(dst)
        } else {
            Poller::new(self, IoEvents::IN)
                .non_blocking(self.nonblocking())
                .poll(|| inner.read(dst))
        }
    }

    fn write(&self, src: &mut SealedBuf) -> AxResult<usize> {
        let inner = self.inner();
        if likely(self.is_blocking()) {
            inner.write(src)
        } else {
            Poller::new(self, IoEvents::OUT)
                .non_blocking(self.nonblocking())
                .poll(|| inner.write(src))
        }
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(metadata_to_kstat(&self.inner().location().metadata()?))
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        self.inner().backend()?.location().ioctl(cmd, arg)
    }

    fn set_nonblocking(&self, flag: bool) -> AxResult {
        self.nonblock.store(flag, Ordering::Release);
        Ok(())
    }

    fn nonblocking(&self) -> bool {
        self.nonblock.load(Ordering::Acquire)
    }

    fn path(&self) -> Cow<str> {
        path_for(self.inner.location())
    }

    fn from_fd(fd: c_int) -> AxResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        let any = get_file_like(fd)?.into_any();

        any.downcast::<Self>().map_err(|any| {
            if any.is::<Directory>() {
                AxError::IsADirectory
            } else {
                AxError::BrokenPipe
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
    inner: Location,
    pub offset: Mutex<u64>,
}

impl Directory {
    pub fn new(inner: Location) -> Self {
        Self {
            inner,
            offset: Mutex::new(0),
        }
    }

    /// Get the inner node of the directory.
    pub fn inner(&self) -> &Location {
        &self.inner
    }
}

impl FileLike for Directory {
    fn read(&self, _dst: &mut SealedBufMut) -> AxResult<usize> {
        Err(AxError::BadFileDescriptor)
    }

    fn write(&self, _src: &mut SealedBuf) -> AxResult<usize> {
        Err(AxError::BadFileDescriptor)
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(metadata_to_kstat(&self.inner.metadata()?))
    }

    fn path(&self) -> Cow<str> {
        path_for(&self.inner)
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn from_fd(fd: c_int) -> AxResult<Arc<Self>> {
        get_file_like(fd)?
            .into_any()
            .downcast::<Self>()
            .map_err(|_| AxError::NotADirectory)
    }
}
impl Pollable for Directory {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}
