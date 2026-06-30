pub mod epoll;
pub mod event;
mod fs;
pub mod inotify;
pub mod io_uring;
#[cfg(feature = "sg2002")]
pub mod ion;
pub mod memfd;
mod net;
pub mod netlink;
mod nsfd;
mod packet;
mod pidfd;
mod pipe;
pub mod signalfd;
pub mod timerfd;
mod wext;

use alloc::{borrow::Cow, sync::Arc};
use core::{ffi::c_int, time::Duration};

use ax_errno::{AxError, AxResult};
use ax_fs_ng::vfs::{FS_CONTEXT, FileBackend, FileFlags, OpenOptions};
use ax_io::prelude::*;
use ax_kspin::SpinRwLock as RwLock;
use ax_task::{TaskState, current};
use axfs_ng_vfs::DeviceId;
use axpoll::Pollable;
use downcast_rs::{DowncastSync, impl_downcast};
use flatten_objects::FlattenObjects;
use linux_raw_sys::general::{
    O_ACCMODE, O_PATH, O_RDONLY, O_RDWR, O_WRONLY, RLIMIT_NOFILE, STATX_BASIC_STATS, stat, statx,
    statx_timestamp,
};
use starry_process::Pid;

pub use self::{
    fs::{Directory, File, ResolveAtResult, resolve_at, with_fs},
    io_uring::IoUring,
    net::Socket,
    nsfd::NsFd,
    packet::{PacketSocket, SockAddrLl},
    pidfd::PidFd,
    pipe::Pipe,
};
use crate::{
    pseudofs::DeviceMmap,
    task::{AX_FILE_LIMIT, AsThread, tasks},
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
        // We always populate the basic stats; Linux returns the same mask.
        // `stx_attributes` is left zero — it reports FS-specific flags we do
        // not track.
        statx.stx_mask = STATX_BASIC_STATS;
        statx.stx_blksize = value.blksize as _;
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

    fn file_mmap(&self) -> AxResult<(FileBackend, FileFlags)> {
        // man 2 mmap ENODEV: "The underlying filesystem of the specified file
        // does not support memory mapping." This is the right errno for fd
        // kinds that do not back onto a mappable file (directory, pipe,
        // socket, epoll, eventfd, etc.).
        Err(AxError::NoSuchDevice)
    }

    fn device_mmap(&self, _offset: u64, _length: u64) -> AxResult<DeviceMmap> {
        Err(AxError::BadFileDescriptor)
    }

    fn ioctl(&self, _cmd: u32, _arg: usize) -> AxResult<usize> {
        Err(AxError::NotATty)
    }

    fn open_flags(&self) -> u32 {
        0
    }

    fn nonblocking(&self) -> bool {
        false
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> AxResult {
        Ok(())
    }

    fn async_mode(&self) -> bool {
        false
    }

    fn supports_async_mode(&self) -> bool {
        false
    }

    fn set_async_mode(&self, _async_mode: bool) -> AxResult {
        Err(AxError::NotATty)
    }

    fn owner(&self) -> AxResult<i32> {
        Err(AxError::NotATty)
    }

    fn set_owner(&self, _owner: i32) -> AxResult {
        Err(AxError::NotATty)
    }

    /// (device, inode) identity used as the key for advisory file locks
    /// (fcntl POSIX/OFD locks and flock(2)).
    ///
    /// Returns `None` for fd kinds that have no inode and are therefore
    /// not lockable (pipes, sockets, epoll, eventfd, ...). Regular files
    /// and directories override this — Linux allows both kinds to carry
    /// advisory locks.
    fn inode_key(&self) -> Option<(u64, u64)> {
        None
    }

    fn append(&self) -> bool {
        false
    }

    fn set_append(&self, _append: bool) -> AxResult {
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

/// Returns true iff `fd` was opened with `O_PATH`.
///
/// Used by syscalls that man explicitly forbids on PATH file descriptors
/// (fchmod / fchown / fsetxattr / ioctl / mmap / fallocate / ...). Per
/// man 2 open §"O_PATH": "other file operations ... fail with the error
/// EBADF."
pub fn fd_is_path(fd: c_int) -> bool {
    get_file_like(fd)
        .map(|f| f.open_flags() & O_PATH != 0)
        .unwrap_or(false)
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
    let removed = FD_TABLE.write().remove(fd as usize);
    if let Some(f) = removed {
        debug!("close_file_like <= count: {}", Arc::strong_count(&f.inner));
        release_locks_on_close(f);
        return Ok(());
    }
    Err(AxError::BadFileDescriptor)
}

pub(crate) fn fd_tables_contain_file(file: &Arc<dyn FileLike>) -> bool {
    !fd_table_file_refs(file).is_empty()
}

pub(crate) fn fd_table_file_refs(file: &Arc<dyn FileLike>) -> alloc::vec::Vec<(Pid, usize)> {
    let mut refs = alloc::vec::Vec::new();
    for task in tasks() {
        if task.state() == TaskState::Exited {
            continue;
        }
        let pid = task.as_thread().proc_data.proc.pid();
        let scope = task.as_thread().proc_data.scope.read();
        let scoped_fd_table = FD_TABLE.scope(&scope);
        let table = scoped_fd_table.read();
        for id in table.ids() {
            if table.get(id).is_some_and(|fd| Arc::ptr_eq(&fd.inner, file)) {
                refs.push((pid, id));
            }
        }
    }
    refs
}

fn notify_close_write(fd: &FileDescriptor) {
    let access = fd.inner.open_flags() & O_ACCMODE;
    if (access == O_WRONLY || access == O_RDWR) && fd.inner.is::<File>() {
        let path = fd.inner.path();
        inotify::notify_close_write_path(path.as_ref());
    }
}

/// Close-time advisory-lock cleanup (the kernel side of POSIX
/// "close-eats-locks", plus OFD release-on-last-close):
///
///   1. Drop every POSIX record lock the calling pid owns on the inode
///      (Linux `locks_remove_posix()` driven by `filp_close()`).
///   2. Drop the `FileDescriptor` so the `Arc<dyn FileLike>` ref
///      count goes down — if this was the last reference, any OFD locks
///      held against the now-dead OFD are released (their entries are
///      pruned the next time something walks the table).
///   3. Wake `F_SETLKW`/`F_OFD_SETLKW` waiters parked on this inode so
///      they can re-check whether the freed range now lets them through.
///
/// `fd` is taken by value so the `Arc` actually drops before step 3 — a
/// pre-drop wake would leave the waiter to re-check, see the OFD's
/// `Weak` still alive, and sleep forever.
pub fn release_locks_on_close(fd: FileDescriptor) {
    let key = fd.inner.inode_key();
    notify_close_write(&fd);
    if let Some(k) = key {
        let pid = current().as_thread().proc_data.proc.pid();
        crate::syscall::release_inode_posix_locks(pid, k);
        if !fd_tables_contain_file(&fd.inner) {
            crate::syscall::release_flock_lock(k, &fd.inner);
        }
    }
    drop(fd);
    if let Some(k) = key {
        crate::syscall::wake_lock_waiters(k);
        crate::syscall::wake_flock_waiters(k);
    }
}

/// Close all open file descriptors for the current process.
///
/// This must be called when a process exits, so that pipe write ends and other
/// resources are properly released. Without this, parent processes blocking on
/// pipe reads will never receive EOF.
pub fn close_all_fds() {
    // Acquire the write lock before checking strong_count. The clone(CLONE_FILES)
    // path in syscall/task/clone.rs also acquires FD_TABLE.read() before cloning
    // the Arc, creating a shared synchronization boundary. This ensures:
    // - If close_all_fds acquires the write lock first, clone blocks on read lock
    //   until we release, so strong_count cannot change during our check.
    // - If clone holds the read lock first, we block on write lock, and by the
    //   time we proceed strong_count already reflects the clone.
    let mut table = FD_TABLE.write();

    // CLONE_FILES may share the same fd table across multiple tasks/processes.
    // In that case, an exiting sharer must not clear the whole table, or other
    // live sharers (including the parent) will lose stdout/stderr unexpectedly.
    if Arc::strong_count(&FD_TABLE) > 1 {
        return;
    }

    let ids: alloc::vec::Vec<usize> = table.ids().collect();
    let mut removed = alloc::vec::Vec::with_capacity(ids.len());
    for id in ids {
        match table.remove(id) {
            Some(fd) => removed.push(fd),
            None => warn!("close_all_fds: fd {id} disappeared during close sweep"),
        }
    }
    drop(table);

    for fd in removed {
        release_locks_on_close(fd);
    }
}

pub fn add_stdio(fd_table: &mut FlattenObjects<FileDescriptor, AX_FILE_LIMIT>) -> AxResult<()> {
    assert_eq!(fd_table.count(), 0);
    let cx = FS_CONTEXT.lock();
    let open = |options: &mut OpenOptions, flags| {
        AxResult::Ok(Arc::new(File::new(
            options.open(&cx, "/dev/console")?.into_file()?,
            flags,
        )))
    };

    let tty_in = open(OpenOptions::new().read(true).write(false), O_RDONLY as _)?;
    let tty_out = open(OpenOptions::new().read(false).write(true), O_WRONLY as _)?;
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
