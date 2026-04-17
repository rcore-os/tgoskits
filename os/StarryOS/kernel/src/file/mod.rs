pub mod epoll;
pub mod event;
mod fs;
mod net;
mod pidfd;
mod pipe;
pub mod signalfd;

use alloc::{borrow::Cow, sync::Arc};
use core::{ffi::c_int, time::Duration};

use ax_errno::{AxError, AxResult};
use ax_fs::{FS_CONTEXT, OpenOptions};
use ax_io::prelude::*;
use ax_task::current;
use axfs_ng_vfs::DeviceId;
use axpoll::Pollable;
use downcast_rs::{DowncastSync, impl_downcast};
use flatten_objects::FlattenObjects;
use linux_raw_sys::general::{RLIMIT_NOFILE, stat, statx, statx_timestamp};
use spin::RwLock;

pub use self::{
    fs::{Directory, File, resolve_at, with_fs},
    net::Socket,
    pidfd::PidFd,
    pipe::Pipe,
};
use crate::task::{AX_FILE_LIMIT, AsThread};

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
        stat {
            st_dev: value.dev as _,
            st_ino: value.ino as _,
            st_nlink: value.nlink as _,
            st_mode: value.mode as _,
            st_uid: value.uid as _,
            st_gid: value.gid as _,
            st_rdev: value.rdev.0 as _,
            st_size: value.size as _,
            st_blksize: value.blksize as _,
            st_blocks: value.blocks as _,
            st_atime: value.atime.as_secs() as _,
            st_atime_nsec: value.atime.subsec_nanos() as _,
            st_mtime: value.mtime.as_secs() as _,
            st_mtime_nsec: value.mtime.subsec_nanos() as _,
            st_ctime: value.ctime.as_secs() as _,
            st_ctime_nsec: value.ctime.subsec_nanos() as _,
            ..Default::default()
        }
    }
}

impl From<Kstat> for statx {
    fn from(value: Kstat) -> Self {
        fn time_to_statx(time: &Duration) -> statx_timestamp {
            statx_timestamp {
                tv_sec: time.as_secs() as _,
                tv_nsec: time.subsec_nanos() as _,
                __reserved: 0,
            }
        }

        statx {
            stx_blksize: value.blksize as _,
            stx_attributes: value.mode as _,
            stx_nlink: value.nlink as _,
            stx_uid: value.uid as _,
            stx_gid: value.gid as _,
            stx_mode: value.mode as _,
            stx_ino: value.ino as _,
            stx_size: value.size as _,
            stx_blocks: value.blocks as _,
            stx_rdev_major: value.rdev.major(),
            stx_rdev_minor: value.rdev.minor(),
            stx_atime: time_to_statx(&value.atime),
            stx_ctime: time_to_statx(&value.ctime),
            stx_mtime: time_to_statx(&value.mtime),
            stx_dev_major: (value.dev >> 32) as _,
            stx_dev_minor: value.dev as _,
            ..Default::default()
        }
    }
}

pub trait WriteBuf: Write + IoBufMut {}
impl<T: Write + IoBufMut> WriteBuf for T {}
pub type IoDst<'a> = dyn WriteBuf + 'a;

pub trait ReadBuf: Read + IoBuf {}
impl<T: Read + IoBuf> ReadBuf for T {}
pub type IoSrc<'a> = dyn ReadBuf + 'a;

#[allow(dead_code)]
pub trait FileLike: Pollable + DowncastSync {
    fn read(&self, _dst: &mut IoDst) -> AxResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn write(&self, _src: &mut IoSrc) -> AxResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat::default())
    }

    fn path(&self) -> Cow<'_, str>;

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
            .downcast_arc()
            .map_err(|_| AxError::InvalidInput)
    }

    fn add_to_fd_table(self, cloexec: bool) -> AxResult<c_int>
    where
        Self: Sized + 'static,
    {
        add_file_like(Arc::new(self), cloexec)
    }
}
impl_downcast!(sync FileLike);

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
