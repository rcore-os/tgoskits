use alloc::{collections::BTreeMap, collections::BTreeSet, format, string::ToString, sync::Arc};
use core::{
    ffi::{c_char, c_int},
    mem,
    ops::{Deref, DerefMut},
};

use ax_errno::{AxError, AxResult};
use ax_fs::{FS_CONTEXT, FileBackend, OpenOptions, OpenResult};
use ax_task::current;
use spin::Mutex;
use axfs_ng_vfs::{DirEntry, FileNode, Location, NodeOps, NodeType, Reference};
use bitflags::bitflags;
use linux_raw_sys::general::*;

use crate::{
    file::{
        Directory, FD_TABLE, File, FileDescriptor, FileLike, Pipe, add_file_like,
        close_file_like, get_file_like, with_fs,
    },
    mm::{UserPtr, vm_load_string},
    pseudofs::{Device, dev::tty},
    task::AsThread,
};

/// Convert open flags to [`OpenOptions`].
fn flags_to_options(flags: c_int, mode: __kernel_mode_t, (uid, gid): (u32, u32)) -> OpenOptions {
    let flags = flags as u32;
    let mut options = OpenOptions::new();
    options.mode(mode).user(uid, gid);
    match flags & 0b11 {
        O_RDONLY => options.read(true),
        O_WRONLY => options.write(true),
        _ => options.read(true).write(true),
    };
    if flags & O_APPEND != 0 {
        options.append(true);
    }
    if flags & O_TRUNC != 0 {
        options.truncate(true);
    }
    if flags & O_CREAT != 0 {
        options.create(true);
    }
    if flags & O_PATH != 0 {
        options.path(true);
    }
    // O_EXCL only makes sense with O_CREAT (POSIX). Without O_CREAT, Linux
    // ignores O_EXCL for existing files — busybox blkdiscard opens block
    // devices with O_RDWR|O_EXCL (no O_CREAT).
    if flags & O_EXCL != 0 && flags & O_CREAT != 0 {
        options.create_new(true);
    }
    if flags & O_DIRECTORY != 0 {
        options.directory(true);
    }
    if flags & O_NOFOLLOW != 0 {
        options.no_follow(true);
    }
    if flags & O_DIRECT != 0 {
        options.direct(true);
    }
    options
}

fn add_to_fd(result: OpenResult, flags: u32) -> AxResult<i32> {
    let f: Arc<dyn FileLike> = match result {
        OpenResult::File(mut file) => {
            // /dev/xx handling
            if let Ok(device) = file.location().entry().downcast::<Device>() {
                // Block device exclusive open (O_EXCL without O_CREAT).
                if let Ok(meta) = device.metadata()
                    && meta.node_type == NodeType::BlockDevice
                    && flags & O_EXCL != 0
                {
                    device.inner().open(true)?;
                }
                let inner = device.inner().as_any();
                #[cfg(feature = "plat-dyn")]
                if crate::pseudofs::usbfs::is_usbfs_device(inner) {
                    let wrapped = crate::pseudofs::usbfs::open_usbfs_file(inner, file, flags)?;
                    if flags & O_NONBLOCK != 0 {
                        wrapped.set_nonblocking(true)?;
                    }
                    return add_file_like(wrapped, flags & O_CLOEXEC != 0);
                }
                if let Some(ptmx) = inner.downcast_ref::<tty::Ptmx>() {
                    // Opening /dev/ptmx creates a new pseudo-terminal
                    let (master, pty_number) = ptmx.create_pty()?;
                    // TODO: this is cursed
                    let pts = FS_CONTEXT.lock().resolve("/dev/pts")?;
                    let entry = DirEntry::new_file(
                        FileNode::new(master),
                        NodeType::CharacterDevice,
                        Reference::new(Some(pts.entry().clone()), pty_number.to_string()),
                    );
                    let loc = Location::new(file.location().mountpoint().clone(), entry);
                    file = ax_fs::File::new(FileBackend::Direct(loc), file.flags());
                } else if inner.is::<tty::CurrentTty>() {
                    let term = current()
                        .as_thread()
                        .proc_data
                        .proc
                        .group()
                        .session()
                        .terminal()
                        .ok_or(AxError::NotFound)?;
                    let path = if term.is::<tty::NTtyDriver>() {
                        "/dev/console".to_string()
                    } else if let Some(pts) = term.downcast_ref::<tty::PtyDriver>() {
                        format!("/dev/pts/{}", pts.pty_number())
                    } else {
                        panic!("unknown terminal type")
                    };
                    let loc = FS_CONTEXT.lock().resolve(&path)?;
                    file = ax_fs::File::new(FileBackend::Direct(loc), file.flags());
                }
            }
            Arc::new(File::new(file, flags))
        }
        OpenResult::Dir(dir) => Arc::new(Directory::new(dir)),
    };
    if flags & O_NONBLOCK != 0 {
        f.set_nonblocking(true)?;
    }
    add_file_like(f, flags & O_CLOEXEC != 0)
}

/// Open or create a file.
/// fd: file descriptor
/// filename: file path to be opened or created
/// flags: open flags
/// mode: see man 7 inode
/// return new file descriptor if succeed, or return -1.
pub fn sys_openat(
    dirfd: c_int,
    path: *const c_char,
    flags: i32,
    mode: __kernel_mode_t,
) -> AxResult<isize> {
    let path = vm_load_string(path)?;
    debug!("sys_openat <= {dirfd} {path:?} {flags:#o} {mode:#o}");

    let mode = mode & !current().as_thread().proc_data.umask();

    let cred = current().as_thread().cred();
    let options = flags_to_options(flags, mode, (cred.fsuid, cred.fsgid));
    with_fs(dirfd, |fs| options.open(fs, path))
        .and_then(|it| add_to_fd(it, flags as _))
        .map(|fd| fd as isize)
}

/// Open a file by `filename` and insert it into the file descriptor table.
///
/// Return its index in the file table (`fd`). Return `EMFILE` if it already
/// has the maximum number of files open.
#[cfg(target_arch = "x86_64")]
pub fn sys_open(path: *const c_char, flags: i32, mode: __kernel_mode_t) -> AxResult<isize> {
    sys_openat(AT_FDCWD as _, path, flags, mode)
}

pub fn sys_close(fd: c_int) -> AxResult<isize> {
    debug!("sys_close <= {fd}");
    close_file_like(fd)?;
    Ok(0)
}

bitflags! {
    #[derive(Debug, Clone, Copy)]
    struct CloseRangeFlags: u32 {
        const UNSHARE = 1 << 1;
        const CLOEXEC = 1 << 2;
    }
}

pub fn sys_close_range(first: i32, last: i32, flags: u32) -> AxResult<isize> {
    if first < 0 || last < first {
        return Err(AxError::InvalidInput);
    }
    let flags = CloseRangeFlags::from_bits(flags).ok_or(AxError::InvalidInput)?;
    debug!("sys_close_range <= fds: [{first}, {last}], flags: {flags:?}");
    if flags.contains(CloseRangeFlags::UNSHARE) {
        // TODO: optimize
        let curr = current();
        let mut scope = curr.as_thread().proc_data.scope.write();
        let mut guard = FD_TABLE.scope_mut(&mut scope);
        let old_files = mem::take(guard.deref_mut());
        old_files.write().clone_from(old_files.read().deref());
    }

    let cloexec = flags.contains(CloseRangeFlags::CLOEXEC);
    let mut fd_table = FD_TABLE.write();
    if let Some(max_index) = fd_table.ids().next_back() {
        for fd in first..=last.min(max_index as i32) {
            if cloexec {
                if let Some(f) = fd_table.get_mut(fd as _) {
                    f.cloexec = true;
                }
            } else {
                fd_table.remove(fd as _);
            }
        }
    }

    Ok(0)
}

fn dup_fd(old_fd: c_int, cloexec: bool) -> AxResult<isize> {
    let f = get_file_like(old_fd)?;
    let new_fd = add_file_like(f, cloexec)?;
    Ok(new_fd as _)
}

fn dup_fd_min(old_fd: c_int, min_fd: c_int, cloexec: bool) -> AxResult<isize> {
    if min_fd < 0 {
        return Err(AxError::InvalidInput);
    }
    let f = get_file_like(old_fd)?;
    let max_nofile = current().as_thread().proc_data.rlim.read()[RLIMIT_NOFILE].current as i32;
    let mut fd_table = FD_TABLE.write();
    for candidate in min_fd..max_nofile {
        let entry = FileDescriptor {
            inner: f.clone(),
            cloexec,
        };
        if fd_table.add_at(candidate as _, entry).is_ok() {
            return Ok(candidate as isize);
        }
    }
    Err(AxError::TooManyOpenFiles)
}

pub fn sys_dup(old_fd: c_int) -> AxResult<isize> {
    debug!("sys_dup <= {old_fd}");
    dup_fd(old_fd, false)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_dup2(old_fd: c_int, new_fd: c_int) -> AxResult<isize> {
    if old_fd == new_fd {
        get_file_like(new_fd)?;
        return Ok(new_fd as _);
    }
    sys_dup3(old_fd, new_fd, 0)
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Dup3Flags: c_int {
        const O_CLOEXEC = O_CLOEXEC as _; // Close on exec
    }
}

pub fn sys_dup3(old_fd: c_int, new_fd: c_int, flags: c_int) -> AxResult<isize> {
    let flags = Dup3Flags::from_bits(flags).ok_or(AxError::InvalidInput)?;
    debug!("sys_dup3 <= old_fd: {old_fd}, new_fd: {new_fd}, flags: {flags:?}");

    if old_fd == new_fd {
        return Err(AxError::InvalidInput);
    }

    let mut fd_table = FD_TABLE.write();
    let mut f = fd_table
        .get(old_fd as _)
        .cloned()
        .ok_or(AxError::BadFileDescriptor)?;
    f.cloexec = flags.contains(Dup3Flags::O_CLOEXEC);

    fd_table.remove(new_fd as _);
    fd_table
        .add_at(new_fd as _, f)
        .map_err(|_| AxError::BadFileDescriptor)?;

    Ok(new_fd as _)
}

pub fn sys_fcntl(fd: c_int, cmd: c_int, arg: usize) -> AxResult<isize> {
    debug!("sys_fcntl <= fd: {fd} cmd: {cmd} arg: {arg}");

    match cmd as u32 {
        F_DUPFD => dup_fd_min(fd, arg as _, false),
        F_DUPFD_CLOEXEC => dup_fd_min(fd, arg as _, true),
        F_SETLK | F_SETLKW => {
            let arg = UserPtr::<flock64>::from(arg);
            let flk = arg.get_as_mut()?;
            let f = get_file_like(fd)?;
            let ino = f.stat()?.ino;
            let pid = current().id().as_u64();
            let is_blocking = cmd as u32 == F_SETLKW;

            loop {
                let mut table = FCNTL_LOCK_TABLE.lock();
                let locked = table.get(&ino);
                let can_acquire = match locked {
                    None => true,
                    Some(lock) => lock.pid == pid,
                };
                if can_acquire {
                    table.insert(ino, FcntlLockState {
                        pid,
                        lock_type: flk.l_type as u32,
                    });
                    return Ok(0);
                }
                if !is_blocking {
                    return Err(AxError::WouldBlock);
                }
                drop(table);
            }
        }
        F_OFD_SETLK | F_OFD_SETLKW => Ok(0),
        F_GETLK | F_OFD_GETLK => {
            let arg = UserPtr::<flock64>::from(arg);
            let flk = arg.get_as_mut()?;
            let f = get_file_like(fd)?;
            let ino = f.stat()?.ino;
            let pid = current().id().as_u64();

            let table = FCNTL_LOCK_TABLE.lock();
            match table.get(&ino) {
                Some(lock) if lock.pid != pid => {
                    flk.l_type = lock.lock_type as _;
                    flk.l_pid = lock.pid as _;
                }
                _ => {
                    flk.l_type = F_UNLCK as _;
                }
            }
            Ok(0)
        }
        F_SETFL => {
            let f = get_file_like(fd)?;
            f.set_nonblocking(arg & (O_NONBLOCK as usize) > 0)?;
            f.set_append(arg & (O_APPEND as usize) > 0)?;
            Ok(0)
        }
        F_GETFL => {
            let f = get_file_like(fd)?;

            let mut ret = f.open_flags() & !O_APPEND;
            if f.nonblocking() {
                ret |= O_NONBLOCK;
            }
            if f.append() {
                ret |= O_APPEND;
            }

            Ok(ret as _)
        }
        F_GETFD => {
            let cloexec = FD_TABLE
                .read()
                .get(fd as _)
                .ok_or(AxError::BadFileDescriptor)?
                .cloexec;
            Ok(if cloexec { FD_CLOEXEC as _ } else { 0 })
        }
        F_SETFD => {
            let cloexec = arg & FD_CLOEXEC as usize != 0;
            FD_TABLE
                .write()
                .get_mut(fd as _)
                .ok_or(AxError::BadFileDescriptor)?
                .cloexec = cloexec;
            Ok(0)
        }
        F_GETPIPE_SZ => {
            let pipe = Pipe::from_fd(fd)?;
            Ok(pipe.capacity() as _)
        }
        F_SETPIPE_SZ => {
            let pipe = Pipe::from_fd(fd)?;
            pipe.resize(arg)?;
            Ok(0)
        }
        _ => {
            warn!("unsupported fcntl parameters: cmd: {cmd}");
            Err(AxError::InvalidInput)
        }
    }
}

struct FlockState {
    sh_fds: BTreeSet<c_int>,
    ex_fd: Option<c_int>,
}

static FLOCK_TABLE: Mutex<BTreeMap<u64, FlockState>> = Mutex::new(BTreeMap::new());

struct FcntlLockState {
    pid: u64,
    lock_type: u32,
}

static FCNTL_LOCK_TABLE: Mutex<BTreeMap<u64, FcntlLockState>> = Mutex::new(BTreeMap::new());

pub fn sys_flock(fd: c_int, operation: c_int) -> AxResult<isize> {
    debug!("flock <= fd: {fd}, operation: {operation}");

    let f = get_file_like(fd)?;
    let ino = f.stat()?.ino;

    let is_nonblock = (operation as u32) & LOCK_NB != 0;
    let op = (operation as u32) & !LOCK_NB;

    let mut table = FLOCK_TABLE.lock();
    let state = table.entry(ino).or_insert_with(|| FlockState {
        sh_fds: BTreeSet::new(),
        ex_fd: None,
    });

    match op {
        LOCK_SH => {
            if let Some(ex) = state.ex_fd
                && ex != fd {
                    if is_nonblock {
                        return Err(AxError::WouldBlock);
                    }
                    return Err(AxError::WouldBlock);
                }
            state.sh_fds.insert(fd);
            Ok(0)
        }
        LOCK_EX => {
            if let Some(ex) = state.ex_fd
                && ex != fd {
                    if is_nonblock {
                        return Err(AxError::WouldBlock);
                    }
                    return Err(AxError::WouldBlock);
                }
            if !state.sh_fds.is_empty() && !state.sh_fds.contains(&fd) {
                if is_nonblock {
                    return Err(AxError::WouldBlock);
                }
                return Err(AxError::WouldBlock);
            }
            state.sh_fds.clear();
            state.ex_fd = Some(fd);
            Ok(0)
        }
        LOCK_UN => {
            state.sh_fds.remove(&fd);
            if state.ex_fd == Some(fd) {
                state.ex_fd = None;
            }
            Ok(0)
        }
        _ => Err(AxError::InvalidInput),
    }
}
