pub mod epoll;
pub mod event;
mod fs;
mod net;
mod pidfd;
mod pipe;

use alloc::{borrow::Cow, sync::Arc};
use core::{any::Any, ffi::c_int, time::Duration};

use axerrno::{AxError, AxResult};
use axfs_ng::{FS_CONTEXT, OpenOptions};
use axfs_ng_vfs::DeviceId;
use axio::{Buf, BufMut, Read, Write};
use axpoll::Pollable;
use axtask::current;
use flatten_objects::FlattenObjects;
use inherit_methods_macro::inherit_methods;
use linux_raw_sys::general::{RLIMIT_NOFILE, stat, statx, statx_timestamp};
use spin::RwLock;
use starry_core::{resources::AX_FILE_LIMIT, task::AsThread};

pub use self::{
    fs::{Directory, File, ResolveAtResult, metadata_to_kstat, resolve_at, with_fs},
    net::Socket,
    pidfd::PidFd,
    pipe::Pipe,
};
use crate::{
    io::IoVectorBufIo,
    mm::{VmBytes, VmBytesMut},
};

#[derive(Debug, Clone, Copy)]
pub struct Kstat {
    pub dev: u64,
    pub ino: u64,
    pub nlink: u32,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub blksize: u32,
    pub blocks: u64,
    pub rdev: DeviceId,
    pub atime: Duration,
    pub mtime: Duration,
    pub ctime: Duration,
}

impl Default for Kstat {
    fn default() -> Self {
        Self {
            dev: 0,
            ino: 1,
            nlink: 1,
            mode: 0,
            uid: 1,
            gid: 1,
            size: 0,
            blksize: 4096,
            blocks: 0,
            rdev: DeviceId::default(),
            atime: Duration::default(),
            mtime: Duration::default(),
            ctime: Duration::default(),
        }
    }
}

impl From<Kstat> for stat {
    fn from(value: Kstat) -> Self {
        // SAFETY: valid for stat
        let mut stat: stat = unsafe { core::mem::zeroed() };
        stat.st_dev = value.dev as _;
        stat.st_ino = value.ino as _;
        stat.st_nlink = value.nlink as _;
        stat.st_mode = value.mode as _;
        stat.st_uid = value.uid as _;
        stat.st_gid = value.gid as _;
        stat.st_size = value.size as _;
        stat.st_blksize = value.blksize as _;
        stat.st_blocks = value.blocks as _;
        stat.st_rdev = value.rdev.0 as _;

        stat.st_atime = value.atime.as_secs() as _;
        stat.st_atime_nsec = value.atime.subsec_nanos() as _;
        stat.st_mtime = value.mtime.as_secs() as _;
        stat.st_mtime_nsec = value.mtime.subsec_nanos() as _;
        stat.st_ctime = value.ctime.as_secs() as _;
        stat.st_ctime_nsec = value.ctime.subsec_nanos() as _;

        stat
    }
}

impl From<Kstat> for statx {
    fn from(value: Kstat) -> Self {
        // SAFETY: valid for statx
        let mut statx: statx = unsafe { core::mem::zeroed() };
        statx.stx_blksize = value.blksize as _;
        statx.stx_attributes = value.mode as _;
        statx.stx_nlink = value.nlink as _;
        statx.stx_uid = value.uid as _;
        statx.stx_gid = value.gid as _;
        statx.stx_mode = value.mode as _;
        statx.stx_ino = value.ino as _;
        statx.stx_size = value.size as _;
        statx.stx_blocks = value.blocks as _;
        statx.stx_rdev_major = value.rdev.major();
        statx.stx_rdev_minor = value.rdev.minor();

        fn time_to_statx(time: &Duration) -> statx_timestamp {
            statx_timestamp {
                tv_sec: time.as_secs() as _,
                tv_nsec: time.subsec_nanos() as _,
                __reserved: 0,
            }
        }
        statx.stx_atime = time_to_statx(&value.atime);
        statx.stx_ctime = time_to_statx(&value.ctime);
        statx.stx_mtime = time_to_statx(&value.mtime);

        statx.stx_dev_major = (value.dev >> 32) as _;
        statx.stx_dev_minor = value.dev as _;

        statx
    }
}

pub enum SealedBuf<'a> {
    Slice(&'a [u8]),
    Bytes(VmBytes),
    IoVec(IoVectorBufIo),
}

impl<'a> From<&'a [u8]> for SealedBuf<'a> {
    fn from(value: &'a [u8]) -> Self {
        Self::Slice(value)
    }
}

impl<'a> From<VmBytes> for SealedBuf<'a> {
    fn from(value: VmBytes) -> Self {
        Self::Bytes(value)
    }
}

impl<'a> From<IoVectorBufIo> for SealedBuf<'a> {
    fn from(value: IoVectorBufIo) -> Self {
        Self::IoVec(value)
    }
}

#[inherit_methods]
impl Read for SealedBuf<'_> {
    fn read(&mut self, buf: &mut [u8]) -> AxResult<usize> {
        match self {
            SealedBuf::Slice(slice) => slice.read(buf),
            SealedBuf::Bytes(bytes) => bytes.read(buf),
            SealedBuf::IoVec(io_vec) => io_vec.read(buf),
        }
    }
}

impl Buf for SealedBuf<'_> {
    fn remaining(&self) -> usize {
        match self {
            SealedBuf::Slice(slice) => slice.remaining(),
            SealedBuf::Bytes(bytes) => bytes.remaining(),
            SealedBuf::IoVec(io_vec) => io_vec.remaining(),
        }
    }

    fn consume(&mut self, f: impl FnMut(&[u8]) -> AxResult<usize>) -> AxResult<usize> {
        match self {
            SealedBuf::Slice(slice) => slice.consume(f),
            SealedBuf::Bytes(bytes) => bytes.consume(f),
            SealedBuf::IoVec(io_vec) => io_vec.consume(f),
        }
    }
}

pub enum SealedBufMut<'a> {
    Slice(&'a mut [u8]),
    Bytes(VmBytesMut),
    IoVec(IoVectorBufIo),
}

impl<'a> From<&'a mut [u8]> for SealedBufMut<'a> {
    fn from(value: &'a mut [u8]) -> Self {
        Self::Slice(value)
    }
}

impl<'a> From<VmBytesMut> for SealedBufMut<'a> {
    fn from(value: VmBytesMut) -> Self {
        Self::Bytes(value)
    }
}

impl<'a> From<IoVectorBufIo> for SealedBufMut<'a> {
    fn from(value: IoVectorBufIo) -> Self {
        Self::IoVec(value)
    }
}

impl Write for SealedBufMut<'_> {
    fn write(&mut self, buf: &[u8]) -> AxResult<usize> {
        match self {
            SealedBufMut::Slice(slice) => slice.write(buf),
            SealedBufMut::Bytes(bytes) => bytes.write(buf),
            SealedBufMut::IoVec(io_vec) => io_vec.write(buf),
        }
    }

    fn flush(&mut self) -> AxResult<()> {
        match self {
            SealedBufMut::Slice(slice) => slice.flush(),
            SealedBufMut::Bytes(bytes) => bytes.flush(),
            SealedBufMut::IoVec(io_vec) => io_vec.flush(),
        }
    }
}

impl BufMut for SealedBufMut<'_> {
    fn remaining_mut(&self) -> usize {
        match self {
            SealedBufMut::Slice(slice) => slice.remaining_mut(),
            SealedBufMut::Bytes(bytes) => bytes.remaining_mut(),
            SealedBufMut::IoVec(io_vec) => io_vec.remaining_mut(),
        }
    }

    fn fill(&mut self, f: impl FnMut(&mut [u8]) -> AxResult<usize>) -> AxResult<usize> {
        match self {
            SealedBufMut::Slice(slice) => slice.fill(f),
            SealedBufMut::Bytes(bytes) => bytes.fill(f),
            SealedBufMut::IoVec(io_vec) => io_vec.fill(f),
        }
    }
}

#[allow(dead_code)]
pub trait FileLike: Pollable + Send + Sync {
    fn read(&self, dst: &mut SealedBufMut) -> AxResult<usize>;
    fn write(&self, src: &mut SealedBuf) -> AxResult<usize>;
    fn stat(&self) -> AxResult<Kstat>;
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
    fn path(&self) -> Cow<str>;
    fn ioctl(&self, _cmd: u32, _arg: usize) -> AxResult<usize> {
        Err(AxError::NotATty)
    }

    fn nonblocking(&self) -> bool {
        false
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> AxResult {
        Ok(())
    }

    fn from_fd(fd: c_int) -> AxResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        get_file_like(fd)?
            .into_any()
            .downcast::<Self>()
            .map_err(|_| AxError::InvalidInput)
    }

    fn add_to_fd_table(self, cloexec: bool) -> AxResult<c_int>
    where
        Self: Sized + 'static,
    {
        add_file_like(Arc::new(self), cloexec)
    }
}

#[derive(Clone)]
pub struct FileDescriptor {
    pub inner: Arc<dyn FileLike>,
    pub cloexec: bool,
}

scope_local::scope_local! {
    /// The current file descriptor table.
    pub static FD_TABLE: Arc<RwLock<FlattenObjects<FileDescriptor, AX_FILE_LIMIT>>> = Arc::default();
}

/// Get a file-like object by `fd`.
pub fn get_file_like(fd: c_int) -> AxResult<Arc<dyn FileLike>> {
    FD_TABLE
        .read()
        .get(fd as usize)
        .map(|fd| fd.inner.clone())
        .ok_or(AxError::BadFileDescriptor)
}

/// Add a file to the file descriptor table.
pub fn add_file_like(f: Arc<dyn FileLike>, cloexec: bool) -> AxResult<c_int> {
    let max_nofile = current().as_thread().proc_data.rlim.read()[RLIMIT_NOFILE].current;
    let mut table = FD_TABLE.write();
    if table.count() as u64 >= max_nofile {
        return Err(AxError::TooManyOpenFiles);
    }
    let fd = FileDescriptor { inner: f, cloexec };
    Ok(table.add(fd).map_err(|_| AxError::TooManyOpenFiles)? as c_int)
}

/// Close a file by `fd`.
pub fn close_file_like(fd: c_int) -> AxResult {
    let f = FD_TABLE
        .write()
        .remove(fd as usize)
        .ok_or(AxError::BadFileDescriptor)?;
    debug!("close_file_like <= count: {}", Arc::strong_count(&f.inner));
    Ok(())
}

pub fn add_stdio(fd_table: &mut FlattenObjects<FileDescriptor, AX_FILE_LIMIT>) -> AxResult<()> {
    assert_eq!(fd_table.count(), 0);
    let cx = FS_CONTEXT.lock();
    let open = |options: &mut OpenOptions| {
        AxResult::Ok(Arc::new(File::new(
            options.open(&cx, "/dev/console")?.into_file()?,
        )))
    };

    let tty_in = open(OpenOptions::new().read(true).write(false))?;
    let tty_out = open(OpenOptions::new().read(false).write(true))?;
    fd_table
        .add(FileDescriptor {
            inner: tty_in,
            cloexec: false,
        })
        .map_err(|_| AxError::TooManyOpenFiles)?;
    fd_table
        .add(FileDescriptor {
            inner: tty_out.clone(),
            cloexec: false,
        })
        .map_err(|_| AxError::TooManyOpenFiles)?;
    fd_table
        .add(FileDescriptor {
            inner: tty_out,
            cloexec: false,
        })
        .map_err(|_| AxError::TooManyOpenFiles)?;

    Ok(())
}
