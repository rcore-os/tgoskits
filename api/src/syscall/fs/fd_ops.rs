use alloc::{format, string::ToString, sync::Arc};
use core::{
    ffi::{c_char, c_int},
    mem,
    ops::{Deref, DerefMut},
};

use axerrno::{AxError, AxResult};
use axfs_ng::{FS_CONTEXT, FileBackend, OpenOptions, OpenResult};
use axfs_ng_vfs::{DirEntry, FileNode, Location, NodePermission, NodeType, Reference};
use axtask::current;
use bitflags::bitflags;
use linux_raw_sys::general::*;
use starry_core::{task::AsThread, vfs::Device};

use crate::{
    file::{
        Directory, FD_TABLE, File, FileLike, Pipe, add_file_like, close_file_like, get_file_like,
        with_fs,
    },
    mm::{UserPtr, vm_load_string},
    syscall::sys::{sys_getegid, sys_geteuid},
    vfs::dev::tty,
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
    if flags & O_EXCL != 0 {
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
                let inner = device.inner().as_any();
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
                    file = axfs_ng::File::new(FileBackend::Direct(loc), file.flags());
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
                    file = axfs_ng::File::new(FileBackend::Direct(loc), file.flags());
                }
            }
            Arc::new(File::new(file))
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

    let options = flags_to_options(flags, mode, (sys_geteuid()? as _, sys_getegid()? as _));
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
        F_DUPFD => dup_fd(fd, false),
        F_DUPFD_CLOEXEC => dup_fd(fd, true),
        F_SETLK | F_SETLKW => Ok(0),
        F_OFD_SETLK | F_OFD_SETLKW => Ok(0),
        F_GETLK | F_OFD_GETLK => {
            let arg = UserPtr::<flock64>::from(arg);
            arg.get_as_mut()?.l_type = F_UNLCK as _;
            Ok(0)
        }
        F_SETFL => {
            get_file_like(fd)?.set_nonblocking(arg & (O_NONBLOCK as usize) > 0)?;
            Ok(0)
        }
        F_GETFL => {
            let f = get_file_like(fd)?;

            let mut ret = 0;
            if f.nonblocking() {
                ret |= O_NONBLOCK;
            }

            let perm = NodePermission::from_bits_truncate(f.stat()?.mode as _);
            if perm.contains(NodePermission::OWNER_WRITE) {
                if perm.contains(NodePermission::OWNER_READ) {
                    ret |= O_RDWR;
                } else {
                    ret |= O_WRONLY;
                }
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
            Ok(0)
        }
    }
}

pub fn sys_flock(fd: c_int, operation: c_int) -> AxResult<isize> {
    debug!("flock <= fd: {fd}, operation: {operation}");
    // TODO: flock
    Ok(0)
}
