use core::{
    ffi::{c_char, c_int},
    panic,
};

use alloc::string::ToString;
use axerrno::{AxError, LinuxError, LinuxResult};
use axfs::fops::OpenOptions;
use linux_raw_sys::general::{
    __kernel_mode_t, AT_FDCWD, F_DUPFD, F_DUPFD_CLOEXEC, F_SETFL, O_APPEND, O_CREAT, O_DIRECTORY,
    O_NONBLOCK, O_PATH, O_RDONLY, O_TRUNC, O_WRONLY,
};

use crate::{
    file::{Directory, FD_TABLE, File, FileLike, add_file_like, close_file_like, get_file_like},
    path::handle_file_path,
    ptr::UserConstPtr,
};

const O_EXEC: u32 = O_PATH;

/// Convert open flags to [`OpenOptions`].
fn flags_to_options(flags: c_int, _mode: __kernel_mode_t) -> OpenOptions {
    let flags = flags as u32;
    let mut options = OpenOptions::new();
    match flags & 0b11 {
        O_RDONLY => options.read(true),
        O_WRONLY => options.write(true),
        _ => {
            options.read(true);
            options.write(true);
        }
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
    if flags & O_EXEC != 0 {
        //options.create_new(true);
        options.execute(true);
    }
    if flags & O_DIRECTORY != 0 {
        options.directory(true);
    }
    options
}

/// Open or create a file.
/// fd: file descriptor
/// filename: file path to be opened or created
/// flags: open flags
/// mode: see man 7 inode
/// return new file descriptor if succeed, or return -1.
pub fn sys_openat(
    dirfd: c_int,
    path: UserConstPtr<c_char>,
    flags: i32,
    mode: __kernel_mode_t,
) -> LinuxResult<isize> {
    let path = path.get_as_str()?;
    let opts = flags_to_options(flags, mode);
    debug!("sys_openat <= {} {} {:?}", dirfd, path, opts);

    let dir = if path.starts_with('/') || dirfd == AT_FDCWD {
        None
    } else {
        Some(Directory::from_fd(dirfd)?)
    };
    let real_path = handle_file_path(dirfd, path)?;

    if !opts.has_directory() {
        match dir.as_ref().map_or_else(
            || axfs::fops::File::open(path, &opts),
            |dir| dir.inner().open_file_at(path, &opts),
        ) {
            Err(AxError::IsADirectory) => {}
            r => {
                let fd = File::new(r?, real_path.to_string()).add_to_fd_table()?;
                return Ok(fd as _);
            }
        }
    }

    let fd = Directory::new(
        dir.map_or_else(
            || axfs::fops::Directory::open_dir(path, &opts),
            |dir| dir.inner().open_dir_at(path, &opts),
        )?,
        real_path.to_string(),
    )
    .add_to_fd_table()?;
    Ok(fd as _)
}

/// Open a file by `filename` and insert it into the file descriptor table.
///
/// Return its index in the file table (`fd`). Return `EMFILE` if it already
/// has the maximum number of files open.
pub fn sys_open(
    path: UserConstPtr<c_char>,
    flags: i32,
    mode: __kernel_mode_t,
) -> LinuxResult<isize> {
    sys_openat(AT_FDCWD as _, path, flags, mode)
}

pub fn sys_close(fd: c_int) -> LinuxResult<isize> {
    debug!("sys_close <= {}", fd);
    close_file_like(fd)?;
    Ok(0)
}

fn dup_fd(old_fd: c_int) -> LinuxResult<isize> {
    let f = get_file_like(old_fd)?;
    let new_fd = add_file_like(f)?;
    Ok(new_fd as _)
}

pub fn sys_dup(old_fd: c_int) -> LinuxResult<isize> {
    debug!("sys_dup <= {}", old_fd);
    dup_fd(old_fd)
}

pub fn sys_dup2(old_fd: c_int, new_fd: c_int) -> LinuxResult<isize> {
    debug!("sys_dup2 <= old_fd: {}, new_fd: {}", old_fd, new_fd);
    let mut fd_table = FD_TABLE.write();
    let f = fd_table
        .get(old_fd as _)
        .cloned()
        .ok_or(LinuxError::EBADF)?;

    if old_fd != new_fd {
        fd_table.remove(new_fd as _);
        fd_table
            .add_at(new_fd as _, f)
            .unwrap_or_else(|_| panic!("new_fd should be valid"));
    }

    Ok(new_fd as _)
}

pub fn sys_fcntl(fd: c_int, cmd: c_int, arg: usize) -> LinuxResult<isize> {
    debug!("sys_fcntl <= fd: {} cmd: {} arg: {}", fd, cmd, arg);

    match cmd as u32 {
        F_DUPFD => dup_fd(fd),
        F_DUPFD_CLOEXEC => {
            warn!("sys_fcntl: treat F_DUPFD_CLOEXEC as F_DUPFD");
            dup_fd(fd)
        }
        F_SETFL => {
            if fd == 0 || fd == 1 || fd == 2 {
                return Ok(0);
            }
            get_file_like(fd)?.set_nonblocking(arg & (O_NONBLOCK as usize) > 0)?;
            Ok(0)
        }
        _ => {
            warn!("unsupported fcntl parameters: cmd: {}", cmd);
            Ok(0)
        }
    }
}
