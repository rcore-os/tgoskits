mod fs;
mod net;
mod pipe;
mod stdio;

use core::{any::Any, ffi::c_int, time::Duration};

use alloc::sync::Arc;
use axerrno::{LinuxError, LinuxResult};
use axfs_ng_vfs::{DeviceId, FileNodeOps};
use axio::PollState;
use axsync::RawMutex;
use axtask::current;
use flatten_objects::FlattenObjects;
use linux_raw_sys::general::{RLIMIT_NOFILE, stat, statx, statx_timestamp};
use spin::RwLock;
use starry_core::{resources::AX_FILE_LIMIT, task::StarryTaskExt, vfs::Device};

pub use self::{
    fs::{Directory, File, ResolveAtResult, metadata_to_kstat, resolve_at, with_fs},
    net::Socket,
    pipe::Pipe,
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

#[allow(dead_code)]
pub trait FileLike: Send + Sync {
    fn read(&self, buf: &mut [u8]) -> LinuxResult<usize>;
    fn write(&self, buf: &[u8]) -> LinuxResult<usize>;
    fn stat(&self) -> LinuxResult<Kstat>;
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
    fn poll(&self) -> LinuxResult<PollState>;

    fn is_nonblocking(&self) -> bool {
        false
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> LinuxResult {
        Ok(())
    }

    fn from_fd(fd: c_int) -> LinuxResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        get_file_like(fd)?
            .into_any()
            .downcast::<Self>()
            .map_err(|_| LinuxError::EINVAL)
    }

    fn add_to_fd_table(self) -> LinuxResult<c_int>
    where
        Self: Sized + 'static,
    {
        add_file_like(Arc::new(self))
    }
}

scope_local::scope_local! {
    /// The current file descriptor table.
    pub static FD_TABLE: Arc<RwLock<FlattenObjects<Arc<dyn FileLike>, AX_FILE_LIMIT>>> =
        Arc::new(RwLock::new({
            let mut fd_table = FlattenObjects::new();
            fd_table
                .add_at(0, Arc::new(stdio::stdin()) as _)
                .unwrap_or_else(|_| panic!()); // stdin
            fd_table
                .add_at(1, Arc::new(stdio::stdout()) as _)
                .unwrap_or_else(|_| panic!()); // stdout
            fd_table
                .add_at(2, Arc::new(stdio::stdout()) as _)
                .unwrap_or_else(|_| panic!()); // stderr
            fd_table
        }));
}

/// Get a file-like object by `fd`.
pub fn get_file_like(fd: c_int) -> LinuxResult<Arc<dyn FileLike>> {
    FD_TABLE
        .read()
        .get(fd as usize)
        .cloned()
        .ok_or(LinuxError::EBADF)
}

/// Add a file to the file descriptor table.
pub fn add_file_like(f: Arc<dyn FileLike>) -> LinuxResult<c_int> {
    let max_nofile =
        StarryTaskExt::of(&current()).process_data().rlim.read()[RLIMIT_NOFILE].current;
    let mut table = FD_TABLE.write();
    if table.count() as u64 >= max_nofile {
        return Err(LinuxError::EMFILE);
    }
    Ok(table.add(f).map_err(|_| LinuxError::EMFILE)? as c_int)
}

/// Close a file by `fd`.
pub fn close_file_like(fd: c_int) -> LinuxResult {
    let f = FD_TABLE
        .write()
        .remove(fd as usize)
        .ok_or(LinuxError::EBADF)?;
    debug!("close_file_like <= count: {}", Arc::strong_count(&f));
    Ok(())
}

pub fn cast_file_like_to_file<T>(file_like: Arc<dyn FileLike>) -> Option<Arc<T>>
where
    T: FileNodeOps<RawMutex> + 'static,
{
    let file = file_like.into_any().downcast::<File>().ok()?;
    let file_ops = file.inner().inner().entry().as_file().ok()?.inner().clone();
    file_ops.into_any().downcast::<T>().ok()
}
pub fn cast_file_like_to_device(file_like: Arc<dyn FileLike>) -> Option<Arc<Device<RawMutex>>> {
    cast_file_like_to_file::<Device<RawMutex>>(file_like)
}
