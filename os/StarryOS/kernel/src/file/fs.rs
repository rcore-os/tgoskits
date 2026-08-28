use alloc::{borrow::Cow, boxed::Box, string::ToString, sync::Arc};
use core::{
    ffi::c_int,
    hint::likely,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_fs_ng::vfs::{FileBackend, FileFlags, FsContext};
use ax_io::{Seek, SeekFrom};
use ax_task::future::{block_on, poll_io};
use axfs_ng_vfs::{
    DirectoryCursor, DirectoryReadState, FsIoEvents, FsPollable, Location, Metadata, NodeFlags,
};
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::{
    general::{AT_EMPTY_PATH, AT_FDCWD, AT_SYMLINK_NOFOLLOW, O_APPEND, O_EXCL},
    ioctl::TIOCSCTTY,
};
use starry_vm::VmPtr;

use super::{FileLike, Kstat, get_file_like};
use crate::{
    StarryError, StarryResult,
    file::{IoDst, IoSrc},
    pseudofs::Device,
    sync::Mutex,
};

// FusionIO/directFS atomic-write toggle used by MySQL.
const DFS_IOCTL_ATOMIC_WRITE_SET: u32 = 0x4004_9502;

pub fn with_fs<R>(
    dirfd: c_int,
    f: impl FnOnce(&mut FsContext) -> StarryResult<R>,
) -> StarryResult<R> {
    let fs_context = ax_fs_ng::vfs::current_fs_context();
    let mut fs = fs_context.lock();
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

    pub fn stat(&self) -> StarryResult<Kstat> {
        match self {
            Self::File(file) => Ok(metadata_to_kstat(&file.metadata()?)),
            Self::Other(file_like) => file_like.stat(),
        }
    }
}

pub fn resolve_at(dirfd: c_int, path: Option<&str>, flags: u32) -> StarryResult<ResolveAtResult> {
    match path {
        Some("") | None => {
            if flags & AT_EMPTY_PATH == 0 {
                return Err(StarryError::NotFound);
            }
            let file_like = get_file_like(dirfd)?;
            let f = file_like.clone();
            Ok(if let Some(file) = f.downcast_ref::<File>() {
                // Use location() directly: backend() rejects PATH-only fds
                // (BadFileDescriptor) which would break fstat(O_PATH-fd).
                // man "O_PATH": fstat(2) is in the allowed-operations list.
                // Fixes bug-open-path-fstat-ebadf.
                ResolveAtResult::File(file.inner().location().clone())
            } else if let Some(dir) = f.downcast_ref::<Directory>() {
                ResolveAtResult::File(dir.inner().clone())
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
                Ok(if flags & AT_SYMLINK_NOFOLLOW != 0 {
                    fs.resolve_no_follow(path)
                } else {
                    fs.resolve(path)
                }
                .map(ResolveAtResult::File)?)
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
        if let Ok(device) = self.inner.location().entry().downcast::<Device>() {
            device.inner().close(self.open_flags & O_EXCL != 0);
        }
    }
}

impl File {
    fn is_blocking(&self) -> bool {
        self.inner.location().flags().contains(NodeFlags::BLOCKING)
    }
}

fn path_for(loc: &Location) -> Cow<'static, str> {
    loc.absolute_path()
        .map_or_else(|_| "<error>".into(), |f| Cow::Owned(f.to_string()))
}

fn fs_events_to_io(events: FsIoEvents) -> IoEvents {
    IoEvents::from_bits_truncate(events.bits())
}

fn io_events_to_fs(events: IoEvents) -> FsIoEvents {
    FsIoEvents::from_bits_truncate(events.bits())
}

impl FileLike for File {
    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        let inner = self.inner();
        if likely(self.is_blocking()) {
            Ok(inner.read(dst)?)
        } else {
            block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
                Ok(inner.read(&mut *dst)?)
            }))
        }
    }

    fn write(&self, src: &mut IoSrc) -> StarryResult<usize> {
        let mut inner = self.inner();
        if self.append() {
            inner.seek(SeekFrom::End(0))?;
        }
        let result: StarryResult<usize> = if likely(self.is_blocking()) {
            Ok(inner.write(src)?)
        } else {
            block_on(poll_io(self, IoEvents::OUT, self.nonblocking(), || {
                Ok(inner.write(&mut *src)?)
            }))
        };
        if let Ok(bytes) = result
            && bytes > 0
        {
            let path = path_for(inner.location()).into_owned();
            crate::file::inotify::notify_modify_path(&path);
        }
        result
    }

    fn stat(&self) -> StarryResult<Kstat> {
        Ok(metadata_to_kstat(&self.inner().location().metadata()?))
    }

    fn inode_key(&self) -> Option<(u64, u64)> {
        let m = self.inner().location().metadata().ok()?;
        Some((m.device, m.inode))
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> StarryResult<usize> {
        let loc = self.inner().backend()?.location();
        if cmd == TIOCSCTTY
            && let Some(result) = crate::pseudofs::dev::tty::bind_pty_at_location(loc.clone())
        {
            return result;
        }
        match cmd {
            DFS_IOCTL_ATOMIC_WRITE_SET => {
                let _enabled: u32 = (arg as *const u32).vm_read()?;
                Ok(0)
            }
            _ => Ok(loc.ioctl(cmd, arg)?),
        }
    }

    fn file_mmap(&self) -> StarryResult<(FileBackend, FileFlags)> {
        Ok((self.inner().backend()?.clone(), self.inner().flags()))
    }

    fn set_nonblocking(&self, flag: bool) -> StarryResult {
        self.nonblock.store(flag, Ordering::Release);
        Ok(())
    }

    fn nonblocking(&self) -> bool {
        self.nonblock.load(Ordering::Acquire)
    }

    fn append(&self) -> bool {
        self.append.load(Ordering::Acquire)
    }

    fn set_append(&self, flag: bool) -> StarryResult {
        self.append.store(flag, Ordering::Release);
        self.inner().set_flag(FileFlags::APPEND, flag);
        Ok(())
    }

    fn open_flags(&self) -> u32 {
        self.open_flags
    }

    fn path(&self) -> Cow<'_, str> {
        path_for(self.inner.location())
    }

    fn from_fd(fd: c_int) -> StarryResult<Arc<Self>>
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
        if let Ok(mount_table) = any.clone().downcast_arc::<crate::file::MountTableFile>() {
            return Ok(mount_table.inner().clone());
        }
        Err(if any.is::<Directory>() {
            StarryError::IsADirectory
        } else {
            StarryError::InvalidInput
        })
    }
}
impl Pollable for File {
    fn poll(&self) -> IoEvents {
        fs_events_to_io(self.inner().location().poll())
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.inner()
            .location()
            .register(context, io_events_to_fs(events));
    }
}

/// Directory wrapper for `ax_fs_ng::fops::Directory`.
pub struct Directory {
    inner: Location,
    // Serialize the complete getdents/lseek transition for one open file
    // description. This is a sleepable mutex because filesystem reads may
    // block; dup/fork share the Directory and therefore this position.
    pub(crate) position: Mutex<DirectoryPosition>,
    /// Original open flags (used by fd_is_path / sys_fchmodat to detect
    /// O_PATH on directory descriptors — open(dir, O_PATH|O_DIRECTORY)
    /// must reject fchmod just like O_PATH on a regular file).
    open_flags: u32,
    /// Whether this is the original handle returned by fsmount(2).
    /// Reopening it as a normal directory deliberately drops this authority.
    detached_mount_handle: bool,
}

pub(crate) struct DirectoryPosition {
    pub(crate) cursor: DirectoryCursor,
    pub(crate) read_state: Option<Box<dyn DirectoryReadState>>,
}

impl Directory {
    pub fn new(inner: Location, open_flags: u32) -> Self {
        Self {
            inner,
            position: Mutex::new(DirectoryPosition {
                cursor: DirectoryCursor::START,
                read_state: None,
            }),
            open_flags,
            detached_mount_handle: false,
        }
    }

    pub(crate) fn new_detached_mount(inner: Location, open_flags: u32) -> Self {
        Self {
            inner,
            position: Mutex::new(DirectoryPosition {
                cursor: DirectoryCursor::START,
                read_state: None,
            }),
            open_flags,
            detached_mount_handle: true,
        }
    }

    /// Get the inner node of the directory.
    pub fn inner(&self) -> &Location {
        &self.inner
    }

    pub(crate) fn is_detached_mount_handle(&self) -> bool {
        self.detached_mount_handle
    }
}

impl FileLike for Directory {
    fn read(&self, _dst: &mut IoDst) -> StarryResult<usize> {
        Err(StarryError::IsADirectory)
    }

    fn write(&self, _src: &mut IoSrc) -> StarryResult<usize> {
        // Directories cannot be opened for writing, so any write attempt
        // means the fd is not open for writing → EBADF.
        // Linux VFS checks FMODE_WRITE before reaching the filesystem layer.
        Err(StarryError::BadFileDescriptor)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        Ok(metadata_to_kstat(&self.inner.metadata()?))
    }

    fn inode_key(&self) -> Option<(u64, u64)> {
        let m = self.inner.metadata().ok()?;
        Some((m.device, m.inode))
    }

    fn open_flags(&self) -> u32 {
        self.open_flags
    }

    fn path(&self) -> Cow<'_, str> {
        path_for(&self.inner)
    }

    fn from_fd(fd: c_int) -> StarryResult<Arc<Self>> {
        get_file_like(fd)?
            .downcast_arc()
            .map_err(|_| StarryError::NotADirectory)
    }
}
impl Pollable for Directory {
    fn poll(&self) -> IoEvents {
        fs_events_to_io(FsIoEvents::IN | FsIoEvents::OUT)
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}
#[cfg(all(test, not(axtest)))]
fn metadata_to_kstat_conversion_rules_hold_for_test() -> bool {
    use core::time::Duration;

    use axfs_ng_vfs::{DeviceId, Metadata};

    // Create a Metadata with known values.
    let meta = Metadata {
        device: 42,
        inode: 100,
        nlink: 3,
        mode: axfs_ng_vfs::NodePermission::from_bits_truncate(0o644),
        node_type: axfs_ng_vfs::NodeType::RegularFile,
        uid: 1000,
        gid: 1000,
        size: 4096,
        block_size: 512,
        blocks: 8,
        rdev: DeviceId::default(),
        atime: Duration::from_secs(1000),
        mtime: Duration::from_millis(2000500),
        ctime: Duration::from_nanos(3000999999000),
    };

    let kstat = metadata_to_kstat(&meta);

    // Verify key fields are correctly transferred.
    kstat.dev == 42
        && kstat.ino == 100
        && kstat.nlink == 3
        && kstat.uid == 1000
        && kstat.gid == 1000
        && kstat.size == 4096
        && kstat.blksize == 512
        && kstat.blocks == 8
        // mode should have type bits (S_IFREG=0100000) OR'd with 0644.
        && (kstat.mode >> 12) == (axfs_ng_vfs::NodeType::RegularFile as u32)
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn metadata_to_kstat_conversion_rules_hold() {
        assert!(super::metadata_to_kstat_conversion_rules_hold_for_test());
    }
}
