use alloc::{collections::BTreeMap, format, string::ToString, sync::Arc};
use core::{
    ffi::{c_char, c_int},
    mem,
    ops::{Deref, DerefMut},
};
use spin::Once;

use ax_errno::{AxError, AxResult};
use ax_fs::{FS_CONTEXT, FileBackend, OpenOptions, OpenResult};
use ax_task::current;
use axfs_ng_vfs::{DirEntry, FileNode, Location, NodePermission, NodeType, Reference};
use bitflags::bitflags;
use linux_raw_sys::general::*;

use crate::{
    file::{
        Directory, FD_TABLE, File, FileDescriptor, FileLike, Pipe, add_file_like, close_file_like,
        get_file_like, with_fs,
    },
    mm::{UserPtr, vm_load_string},
    pseudofs::{Device, dev::tty},
    syscall::sys::{sys_getegid, sys_geteuid},
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

    // Empty path should return ENOENT
    if path.is_empty() {
        return Err(AxError::NotFound);
    }

    let mode = mode & !current().as_thread().proc_data.umask();

    let options = flags_to_options(flags, mode, (sys_geteuid()? as _, sys_getegid()? as _));

    // Check for O_DIRECTORY flag and validate it BEFORE opening
    // According to Linux: O_DIRECTORY requires the target to be a directory
    use linux_raw_sys::general::{O_ACCMODE, O_DIRECTORY, O_WRONLY, S_IFDIR, S_IFMT};
    let has_o_directory = (flags as u32) & O_DIRECTORY != 0;

    // If O_DIRECTORY is specified, we need to check if the path exists and is a directory
    // This needs to be done before the actual open to return ENOTDIR correctly
    if has_o_directory {
        // First check if the path exists and what type it is
        match with_fs(dirfd, |fs| fs.metadata(&path)) {
            Ok(metadata) => {
                use crate::file::metadata_to_kstat;
                let kstat = metadata_to_kstat(&metadata);
                if (kstat.mode & S_IFMT) != S_IFDIR {
                    info!("openat: O_DIRECTORY flag on non-directory file, returning ENOTDIR");
                    return Err(AxError::NotADirectory);
                }
            }
            Err(e) => {
                // If metadata check fails, let the open proceed and handle errors there
                // This handles both ENOENT (file doesn't exist) and other cases
            }
        }
    }

    let file = with_fs(dirfd, |fs| options.open(fs, &path))?;

    // Check if opening a directory with write access (O_WRONLY or O_RDWR)
    // According to Linux open(2) man page and kernel implementation:
    // "For directories it's -EISDIR, for other non-regulars - -EINVAL"
    use crate::file::metadata_to_kstat;

    // Check both OpenResult::File (directory opened without O_DIRECTORY)
    // and OpenResult::Dir (directory opened with O_DIRECTORY flag)
    match &file {
        OpenResult::File(f) => {
            let metadata = f.location().metadata()?;
            let kstat = metadata_to_kstat(&metadata);
            let is_dir = (kstat.mode & S_IFMT) == S_IFDIR;

            // Directory with write access -> EISDIR
            if is_dir {
                let access_mode = flags as u32 & O_ACCMODE;
                if access_mode == O_WRONLY as u32 || access_mode == O_RDWR as u32 {
                    return Err(AxError::IsADirectory);
                }
            }
        }
        OpenResult::Dir(_) => {
            // Directory opened with O_DIRECTORY flag
            // This should only happen if the file is actually a directory
            let access_mode = flags as u32 & O_ACCMODE;
            if access_mode == O_WRONLY as u32 || access_mode == O_RDWR as u32 {
                return Err(AxError::IsADirectory);
            }
        }
    }

    add_to_fd(file, flags as _).map(|fd| fd as isize)
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

fn dup_fd_with_min(old_fd: c_int, min_fd: c_int, cloexec: bool) -> AxResult<isize> {
    // F_DUPFD with negative arg should return EINVAL
    if min_fd < 0 {
        return Err(ax_errno::AxError::InvalidInput);
    }
    let f = get_file_like(old_fd)?;
    let new_fd = add_file_like_with_min(f, min_fd, cloexec)?;
    Ok(new_fd as _)
}

/// Add a file to the file descriptor table with minimum fd requirement
fn add_file_like_with_min(
    f: alloc::sync::Arc<dyn FileLike>,
    min_fd: c_int,
    cloexec: bool,
) -> AxResult<c_int> {
    let max_nofile =
        current().as_thread().proc_data.rlim.read()[linux_raw_sys::general::RLIMIT_NOFILE].current;
    let mut table = FD_TABLE.write();
    if table.count() as u64 >= max_nofile {
        return Err(ax_errno::AxError::TooManyOpenFiles);
    }

    // Find the next available fd >= min_fd
    let mut target_fd = min_fd.max(0) as usize;
    while table.get(target_fd).is_some() {
        target_fd += 1;
        if target_fd as u64 >= max_nofile {
            return Err(ax_errno::AxError::TooManyOpenFiles);
        }
    }

    let fd = FileDescriptor { inner: f, cloexec };
    table
        .add_at(target_fd, fd)
        .map_err(|_| ax_errno::AxError::TooManyOpenFiles)?;
    Ok(target_fd as c_int)
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
        F_DUPFD => dup_fd_with_min(fd, arg as c_int, false),
        F_DUPFD_CLOEXEC => dup_fd_with_min(fd, arg as c_int, true),
        F_SETLK | F_SETLKW => Ok(0),
        F_OFD_SETLK | F_OFD_SETLKW => Ok(0),
        F_GETLK | F_OFD_GETLK => {
            let arg = UserPtr::<flock64>::from(arg);
            arg.get_as_mut()?.l_type = F_UNLCK as _;
            Ok(0)
        }
        F_SETFL => {
            let f = get_file_like(fd)?;
            f.set_nonblocking(arg & (O_NONBLOCK as usize) > 0)?;

            // Handle O_APPEND flag changes
            if let Ok(file) = f.downcast_arc::<crate::file::File>() {
                // O_APPEND and O_NONBLOCK are the only flags that can be changed via F_SETFL
                let append_mask = linux_raw_sys::general::O_APPEND as u32;
                let append_value = (arg & (O_APPEND as usize)) as u32;
                file.set_flags(append_mask, append_value);
            }

            Ok(0)
        }
        F_GETFL => {
            let f = get_file_like(fd)?;

            let mut ret = 0;
            if f.nonblocking() {
                ret |= O_NONBLOCK;
            }

            // Try to get file flags if it's a regular File
            if let Ok(file) = f.clone().downcast_arc::<crate::file::File>() {
                let file_flags = file.flags();
                // The flags are already converted to Linux format in File::flags()
                ret |= file_flags;
            } else {
                // Fallback to permission-based detection for other file types
                let perm = NodePermission::from_bits_truncate(f.stat()?.mode as _);
                if perm.contains(NodePermission::OWNER_WRITE) {
                    if perm.contains(NodePermission::OWNER_READ) {
                        ret |= O_RDWR;
                    } else {
                        ret |= O_WRONLY;
                    }
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
            Err(AxError::InvalidInput)
        }
    }
}

// flock operation constants
const LOCK_SH: i32 = 1;  // Shared lock
const LOCK_EX: i32 = 2;  // Exclusive lock
const LOCK_UN: i32 = 8;  // Unlock
const LOCK_NB: i32 = 4;  // Non-blocking

// Global flock table mapping inode -> lock type
static FLOCK_TABLE: Once<spin::Mutex<BTreeMap<u64, i32>>> = Once::new();

fn get_flock_table() -> &'static spin::Mutex<BTreeMap<u64, i32>> {
    FLOCK_TABLE.call_once(|| spin::Mutex::new(BTreeMap::new()))
}

// Get file key for flock (use inode number)
fn get_file_key_for_flock(fd: c_int) -> AxResult<u64> {
    let file = File::from_fd(fd)?;
    let kstat = file.stat()?;
    Ok(kstat.ino)
}

pub fn sys_flock(fd: c_int, operation: c_int) -> AxResult<isize> {
    debug!("flock <= fd: {fd}, operation: {operation}");

    let file_key = get_file_key_for_flock(fd)?;

    // Extract lock operation
    let is_unlock = (operation & LOCK_UN) != 0;
    let is_shared = (operation & LOCK_SH) != 0;
    let is_exclusive = (operation & LOCK_EX) != 0;
    let is_nonblocking = (operation & LOCK_NB) != 0;

    let mut flock_table = get_flock_table().lock();

    if is_unlock {
        // Release lock
        flock_table.remove(&file_key);
        return Ok(0);
    }

    // Check for existing lock conflicts
    if let Some(&existing_lock) = flock_table.get(&file_key) {
        let conflict = if is_exclusive {
            // Exclusive lock conflicts with any existing lock
            true
        } else {
            // Shared lock conflicts with exclusive lock
            (existing_lock & LOCK_EX) != 0
        };

        if conflict {
            if is_nonblocking {
                return Err(AxError::WouldBlock);
            }
            // TODO: Implement proper blocking flock behavior
        }
    }

    // Set the lock
    let lock_type = if is_exclusive { LOCK_EX } else { LOCK_SH };
    flock_table.insert(file_key, lock_type);

    Ok(0)
}
