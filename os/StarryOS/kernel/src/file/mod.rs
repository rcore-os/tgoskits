// Shared contiguous dma-buf primitive + resolver used by every accelerator that
// exchanges buffers (JPU / NPU / RGA).
#[cfg(any(feature = "jpeg", feature = "rknpu", feature = "rga"))]
pub mod dmabuf;
pub mod epoll;
#[cfg(test)]
mod epoll_axtest;
mod epoll_file;
mod epoll_topology;
pub mod event;
mod fs;
pub mod inotify;
pub mod io_uring;
#[cfg(feature = "sg2002")]
pub mod ion;
pub mod memfd;
mod mount_table;
mod net;
pub mod netlink;
mod nsfd;
mod packet;
mod pidfd;
mod pipe;
pub mod signalfd;
pub mod timerfd;
mod wext;

use alloc::{borrow::Cow, collections::BTreeSet, sync::Arc};
use core::{ffi::c_int, time::Duration};

use ax_fs_ng::vfs::{FileBackend, FileFlags, OpenOptions};
use ax_io::prelude::*;
use ax_task::{TaskState, current};
use axfs_ng_vfs::DeviceId;
use axpoll::Pollable;
use downcast_rs::{DowncastSync, impl_downcast};
use flatten_objects::FlattenObjects;
use linux_raw_sys::general::{
    O_ACCMODE, O_PATH, O_RDONLY, O_RDWR, O_WRONLY, RLIMIT_NOFILE, STATX_ATTR_MOUNT_ROOT,
    STATX_BASIC_STATS, stat, statx, statx_timestamp,
};

pub(crate) use self::mount_table::{MountTableFile, notify_mount_namespace_changed};
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
    StarryError, StarryResult,
    pseudofs::DeviceMmap,
    sync::RwLock,
    task::{AX_FILE_LIMIT, AsThread, PidIdentityId, tasks},
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
        // Mount-root state is a VFS attribute, so every statx result advertises
        // support for it. The syscall layer sets the value when it has a
        // resolved filesystem location.
        statx.stx_mask = STATX_BASIC_STATS;
        statx.stx_attributes_mask = STATX_ATTR_MOUNT_ROOT as u64;
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
    /// Validate a scalar write length before importing the user buffer.
    ///
    /// File types with count errors that take precedence over `EFAULT` can
    /// override this hook. The full write operation must repeat any invariant
    /// needed to remain correct for non-scalar callers.
    fn validate_write_len(&self, _len: usize) -> StarryResult {
        Ok(())
    }

    fn read(&self, _dst: &mut IoDst) -> StarryResult<usize> {
        Err(StarryError::InvalidInput)
    }

    fn write(&self, _src: &mut IoSrc) -> StarryResult<usize> {
        Err(StarryError::InvalidInput)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        Ok(Kstat::default())
    }

    fn path(&self) -> Cow<'_, str>;

    fn file_mmap(&self) -> StarryResult<(FileBackend, FileFlags)> {
        // man 2 mmap ENODEV: "The underlying filesystem of the specified file
        // does not support memory mapping." This is the right errno for fd
        // kinds that do not back onto a mappable file (directory, pipe,
        // socket, epoll, eventfd, etc.).
        Err(StarryError::NoSuchDevice)
    }

    fn device_mmap(&self, _offset: u64, _length: u64) -> StarryResult<DeviceMmap> {
        // `None` is the typed probe result for an ordinary file: `sys_mmap`
        // must continue through `file_mmap`. An error from an implementation
        // that owns a device mapping is committed and must reach userspace.
        Ok(DeviceMmap::None)
    }

    fn ioctl(&self, _cmd: u32, _arg: usize) -> StarryResult<usize> {
        Err(StarryError::NotATty)
    }

    fn open_flags(&self) -> u32 {
        0
    }

    fn nonblocking(&self) -> bool {
        false
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> StarryResult {
        Ok(())
    }

    fn async_mode(&self) -> bool {
        false
    }

    fn supports_async_mode(&self) -> bool {
        false
    }

    fn set_async_mode(&self, _async_mode: bool) -> StarryResult {
        Err(StarryError::NotATty)
    }

    fn owner(&self) -> StarryResult<i32> {
        Err(StarryError::NotATty)
    }

    fn set_owner(&self, _owner: i32) -> StarryResult {
        Err(StarryError::NotATty)
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

    fn set_append(&self, _append: bool) -> StarryResult {
        Ok(())
    }

    /// Per-close hook, invoked with the closing process's stable identity
    /// generation whenever a file
    /// descriptor referring to this object is dropped from an fd table -
    /// explicit `close`, `close_range`, `dup2`/`dup3` replacement, exec
    /// CLOEXEC, or process exit. This mirrors Linux `f_op->flush`
    /// (`filp_flush`, fs/open.c:1470), which runs on every fd-closing path
    /// rather than only on the last reference. The default is a no-op; POSIX
    /// message-queue descriptors override it to drop a matching `mq_notify`
    /// registration (`mqueue_flush_file`, ipc/mqueue.c:658).
    fn on_close(&self, _owner: PidIdentityId) {}

    fn from_fd(fd: c_int) -> StarryResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        get_file_like(fd)?
            .downcast_arc()
            .map_err(|_| StarryError::InvalidInput)
    }

    fn add_to_fd_table(self, cloexec: bool) -> StarryResult<c_int>
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

/// Installed file descriptors owned by one shared file table.
pub struct FileTable {
    entries: FlattenObjects<FileDescriptor, AX_FILE_LIMIT>,
    reserved: BTreeSet<usize>,
}

impl FileTable {
    pub const fn new() -> Self {
        Self {
            entries: FlattenObjects::new(),
            reserved: BTreeSet::new(),
        }
    }

    pub fn count(&self) -> usize {
        self.entries.count() + self.reserved.len()
    }

    pub fn get(&self, fd: usize) -> Option<&FileDescriptor> {
        self.entries.get(fd)
    }

    pub fn get_mut(&mut self, fd: usize) -> Option<&mut FileDescriptor> {
        self.entries.get_mut(fd)
    }

    pub fn add(&mut self, descriptor: FileDescriptor) -> Result<usize, FileDescriptor> {
        let Some(fd) = (0..AX_FILE_LIMIT)
            .find(|fd| !self.entries.is_assigned(*fd) && !self.reserved.contains(fd))
        else {
            return Err(descriptor);
        };
        self.entries.add_at(fd, descriptor)
    }

    pub fn add_at(
        &mut self,
        fd: usize,
        descriptor: FileDescriptor,
    ) -> Result<usize, FileDescriptor> {
        if self.reserved.contains(&fd) {
            return Err(descriptor);
        }
        self.entries.add_at(fd, descriptor)
    }

    pub fn remove(&mut self, fd: usize) -> Option<FileDescriptor> {
        self.entries.remove(fd)
    }

    pub fn ids(&self) -> impl DoubleEndedIterator<Item = usize> + '_ {
        self.entries.ids()
    }

    pub fn last_id(&self) -> Option<usize> {
        self.entries.ids().next_back()
    }

    pub(crate) fn is_reserved(&self, fd: usize) -> bool {
        self.reserved.contains(&fd)
    }

    fn reserve(&mut self) -> Option<usize> {
        let fd = (0..AX_FILE_LIMIT)
            .find(|fd| !self.entries.is_assigned(*fd) && !self.reserved.contains(fd))?;
        let inserted = self.reserved.insert(fd);
        debug_assert!(inserted);
        Some(fd)
    }

    fn install_reserved(
        &mut self,
        fd: usize,
        descriptor: FileDescriptor,
    ) -> Result<(), FileDescriptor> {
        if !self.reserved.remove(&fd) {
            return Err(descriptor);
        }
        self.entries.add_at(fd, descriptor).map(|_| ())
    }

    fn release_reserved(&mut self, fd: usize) {
        assert!(
            self.reserved.remove(&fd),
            "releasing an unreserved file descriptor"
        );
    }
}

impl Clone for FileTable {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            // An in-flight syscall owns each reservation. A copied fd table
            // inherits only descriptors that have reached install.
            reserved: BTreeSet::new(),
        }
    }
}

impl Default for FileTable {
    fn default() -> Self {
        Self::new()
    }
}

scope_local::scope_local! {
    /// The current file descriptor table.
    pub static FD_TABLE: Arc<RwLock<FileTable>> = Arc::default();
}

/// Returns an owned reference to the file table of the active scope.
///
/// The CPU pin is released after cloning the `Arc`, before callers acquire the
/// table lock or run descriptor destructors.
pub fn current_fd_table() -> Arc<RwLock<FileTable>> {
    FD_TABLE.clone_current()
}

/// A file descriptor number prepared by a fallible syscall transaction.
///
/// Dropping it before [`PreparedFileDescriptor::install`] rolls the descriptor
/// back from its originating table.
pub struct PreparedFileDescriptor {
    table: Arc<RwLock<FileTable>>,
    fd: usize,
    descriptor: Option<FileDescriptor>,
}

impl PreparedFileDescriptor {
    fn prepare_in(
        table: Arc<RwLock<FileTable>>,
        descriptor: FileDescriptor,
        max_entries: usize,
    ) -> StarryResult<Self> {
        let fd = {
            let mut table = table.write();
            if table.count() >= max_entries {
                return Err(StarryError::TooManyOpenFiles);
            }
            table.reserve().ok_or(StarryError::TooManyOpenFiles)?
        };
        Ok(Self {
            table,
            fd,
            descriptor: Some(descriptor),
        })
    }

    pub const fn fd(&self) -> c_int {
        self.fd as c_int
    }

    pub fn install(mut self) {
        let descriptor = self
            .descriptor
            .take()
            .expect("prepared descriptor installed twice");
        let install = self.table.write().install_reserved(self.fd, descriptor);
        if let Err(descriptor) = install {
            // Keep descriptor destruction outside the preemption-disabling
            // table lock even when an internal reservation invariant fails.
            drop(descriptor);
            panic!("prepared file descriptor lost its reservation before install");
        }
    }
}

impl Drop for PreparedFileDescriptor {
    fn drop(&mut self) {
        if let Some(descriptor) = self.descriptor.take() {
            self.table.write().release_reserved(self.fd);
            // File destructors may wake waiters, so the descriptor must drop
            // after the preemption-disabling table guard is gone.
            drop(descriptor);
        }
    }
}

/// Prepares a descriptor for a later transaction commit.
pub fn prepare_file_like(
    file: Arc<dyn FileLike>,
    cloexec: bool,
) -> StarryResult<PreparedFileDescriptor> {
    let max_nofile = current().as_thread().proc_data.rlim.read()[RLIMIT_NOFILE].current;
    let table = current_fd_table();
    PreparedFileDescriptor::prepare_in(
        table,
        FileDescriptor {
            inner: file,
            cloexec,
        },
        max_nofile as usize,
    )
}

/// Get a file-like object by `fd`.
pub fn get_file_like(fd: c_int) -> StarryResult<Arc<dyn FileLike>> {
    current_fd_table()
        .read()
        .get(fd as usize)
        .map(|fd| fd.inner.clone())
        .ok_or(StarryError::BadFileDescriptor)
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
pub fn add_file_like(f: Arc<dyn FileLike>, cloexec: bool) -> StarryResult<c_int> {
    let max_nofile = current().as_thread().proc_data.rlim.read()[RLIMIT_NOFILE].current;
    let fd_table = current_fd_table();
    let mut table = fd_table.write();
    if table.count() as u64 >= max_nofile {
        return Err(StarryError::TooManyOpenFiles);
    }
    let fd = FileDescriptor { inner: f, cloexec };
    Ok(table.add(fd).map_err(|_| StarryError::TooManyOpenFiles)? as c_int)
}

/// Close a file by `fd`.
pub fn close_file_like(fd: c_int) -> StarryResult {
    let removed = current_fd_table().write().remove(fd as usize);
    if let Some(f) = removed {
        debug!("close_file_like <= count: {}", Arc::strong_count(&f.inner));
        release_locks_on_close(f);
        return Ok(());
    }
    Err(StarryError::BadFileDescriptor)
}

pub(crate) fn fd_tables_contain_file(file: &Arc<dyn FileLike>) -> bool {
    !fd_table_file_refs(file).is_empty()
}

pub(crate) fn fd_table_file_refs(file: &Arc<dyn FileLike>) -> alloc::vec::Vec<(u32, usize)> {
    let mut refs = alloc::vec::Vec::new();
    for task in tasks() {
        if task.state() == TaskState::Exited {
            continue;
        }
        let thread = task.as_thread();
        let pid = thread.proc_data.proc.pid().get();
        let scope = thread.scope.read();
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
    let owner = current().as_thread().proc_data.identity().id();
    // Linux `filp_flush` runs `f_op->flush` on every fd-closing path (explicit
    // close, close_range, dup2/dup3 replacement, exec CLOEXEC, process exit),
    // all of which funnel through here. This is where an mq descriptor drops a
    // matching `mq_notify` registration (`mqueue_flush_file`).
    fd.inner.on_close(owner);
    notify_close_write(&fd);
    if let Some(k) = key {
        crate::syscall::release_inode_posix_locks(owner, k);
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

/// Close all descriptors in the current thread's fd table when it is the last
/// table sharer.
///
/// This must be called whenever a thread exits because `unshare(CLONE_FILES)`
/// can give one thread a private table. Shared tables are left intact until the
/// final thread or process using them exits.
pub fn close_all_fds() {
    // Acquire the write lock before checking strong_count. The clone(CLONE_FILES)
    // path in syscall/task/clone.rs also acquires FD_TABLE.read() before cloning
    // the Arc, creating a shared synchronization boundary. This ensures:
    // - If close_all_fds acquires the write lock first, clone blocks on read lock
    //   until we release, so strong_count cannot change during our check.
    // - If clone holds the read lock first, we block on write lock, and by the
    //   time we proceed strong_count already reflects the clone.
    let fd_table = current_fd_table();
    let mut table = fd_table.write();

    // CLONE_FILES may share the same fd table across multiple tasks/processes.
    // In that case, an exiting sharer must not clear the whole table, or other
    // live sharers (including the parent) will lose stdout/stderr unexpectedly.
    // One reference belongs to the scope slot and one is this owned snapshot.
    if Arc::strong_count(&fd_table) > 2 {
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

pub fn add_stdio(fd_table: &mut FileTable) -> StarryResult<()> {
    assert_eq!(fd_table.count(), 0);
    let fs_context = ax_fs_ng::vfs::current_fs_context();
    let cx = fs_context.lock();
    let open = |options: &mut OpenOptions, flags| {
        StarryResult::Ok(Arc::new(File::new(
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
        .map_err(|_| StarryError::TooManyOpenFiles)?;
    fd_table
        .add(FileDescriptor {
            inner: tty_out.clone(),
            cloexec: false,
        })
        .map_err(|_| StarryError::TooManyOpenFiles)?;
    fd_table
        .add(FileDescriptor {
            inner: tty_out,
            cloexec: false,
        })
        .map_err(|_| StarryError::TooManyOpenFiles)?;

    Ok(())
}

#[cfg(all(test, not(axtest)))]
fn prepared_descriptor_stays_hidden_until_install_for_test() -> bool {
    fn descriptor() -> FileDescriptor {
        let (read_end, _write_end) = Pipe::new();
        FileDescriptor {
            inner: Arc::new(read_end),
            cloexec: true,
        }
    }

    let table = Arc::new(RwLock::new(FileTable::new()));
    let prepared =
        PreparedFileDescriptor::prepare_in(table.clone(), descriptor(), AX_FILE_LIMIT).unwrap();
    let reserved_fd = prepared.fd;
    let hidden = table.read().get(reserved_fd).is_none();
    let counted_against_limit =
        PreparedFileDescriptor::prepare_in(table.clone(), descriptor(), 1).is_err();
    let installed_descriptor = descriptor();
    let Ok(installed_fd) = table.write().add(installed_descriptor) else {
        return false;
    };
    let allocation_skipped_reservation = installed_fd != reserved_fd;
    let cloned = table.read().clone();
    let clone_excluded_reservation = cloned.get(reserved_fd).is_none()
        && cloned.get(installed_fd).is_some()
        && cloned.count() == 1;
    drop(prepared);
    let reused_descriptor = descriptor();
    let Ok(reused_fd) = table.write().add(reused_descriptor) else {
        return false;
    };
    let rollback_released_number = reused_fd == reserved_fd;

    let install_table = Arc::new(RwLock::new(FileTable::new()));
    let prepared =
        PreparedFileDescriptor::prepare_in(install_table.clone(), descriptor(), AX_FILE_LIMIT)
            .unwrap();
    let installed_fd = prepared.fd;
    prepared.install();
    let install_made_visible = install_table.read().get(installed_fd).is_some();

    hidden
        && counted_against_limit
        && allocation_skipped_reservation
        && clone_excluded_reservation
        && rollback_released_number
        && install_made_visible
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[cfg(all(test, not(axtest)))]
    #[test]
    fn prepared_descriptor_stays_hidden_until_install() {
        assert!(super::prepared_descriptor_stays_hidden_until_install_for_test());
    }
}
