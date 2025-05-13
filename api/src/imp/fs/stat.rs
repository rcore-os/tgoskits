use core::ffi::{c_char, c_int};

use axerrno::{AxError, LinuxError, LinuxResult};
use axfs::fops::OpenOptions;
use linux_raw_sys::general::{AT_EMPTY_PATH, stat, statx};

use crate::{
    file::{Directory, File, FileLike, Kstat, get_file_like},
    path::handle_file_path,
    ptr::{UserConstPtr, UserPtr, nullable},
};

fn stat_at_path(path: &str) -> LinuxResult<Kstat> {
    let opts = OpenOptions::new().set_read(true);
    match axfs::fops::File::open(path, &opts) {
        Ok(file) => File::new(file, path.into()).stat(),
        Err(AxError::IsADirectory) => {
            let dir = axfs::fops::Directory::open_dir(path, &opts)?;
            Directory::new(dir, path.into()).stat()
        }
        Err(e) => Err(e.into()),
    }
}

/// Get the file metadata by `path` and write into `statbuf`.
///
/// Return 0 if success.
pub fn sys_stat(path: UserConstPtr<c_char>, statbuf: UserPtr<stat>) -> LinuxResult<isize> {
    let path = path.get_as_str()?;
    debug!("sys_stat <= path: {}", path);

    *statbuf.get_as_mut()? = stat_at_path(path)?.into();

    Ok(0)
}

/// Get file metadata by `fd` and write into `statbuf`.
///
/// Return 0 if success.
pub fn sys_fstat(fd: i32, statbuf: UserPtr<stat>) -> LinuxResult<isize> {
    debug!("sys_fstat <= fd: {}", fd);
    *statbuf.get_as_mut()? = get_file_like(fd)?.stat()?.into();
    Ok(0)
}

/// Get the metadata of the symbolic link and write into `buf`.
///
/// Return 0 if success.
pub fn sys_lstat(path: UserConstPtr<c_char>, statbuf: UserPtr<stat>) -> LinuxResult<isize> {
    // TODO: symlink
    sys_stat(path, statbuf)
}

pub fn sys_fstatat(
    dirfd: c_int,
    path: UserConstPtr<c_char>,
    statbuf: UserPtr<stat>,
    flags: u32,
) -> LinuxResult<isize> {
    let path = nullable!(path.get_as_str())?;
    debug!(
        "sys_fstatat <= dirfd: {}, path: {:?}, flags: {}",
        dirfd, path, flags
    );

    *statbuf.get_as_mut()? = if path.is_none_or(|s| s.is_empty()) {
        if (flags & AT_EMPTY_PATH) == 0 {
            return Err(LinuxError::ENOENT);
        }
        let f = get_file_like(dirfd)?;
        f.stat()?.into()
    } else {
        let path = handle_file_path(dirfd, path.unwrap_or_default())?;
        stat_at_path(path.as_str())?.into()
    };

    Ok(0)
}

pub fn sys_statx(
    dirfd: c_int,
    path: UserConstPtr<c_char>,
    flags: u32,
    _mask: u32,
    statxbuf: UserPtr<statx>,
) -> LinuxResult<isize> {
    // `statx()` uses pathname, dirfd, and flags to identify the target
    // file in one of the following ways:

    // An absolute pathname(situation 1)
    //        If pathname begins with a slash, then it is an absolute
    //        pathname that identifies the target file.  In this case,
    //        dirfd is ignored.

    // A relative pathname(situation 2)
    //        If pathname is a string that begins with a character other
    //        than a slash and dirfd is AT_FDCWD, then pathname is a
    //        relative pathname that is interpreted relative to the
    //        process's current working directory.

    // A directory-relative pathname(situation 3)
    //        If pathname is a string that begins with a character other
    //        than a slash and dirfd is a file descriptor that refers to
    //        a directory, then pathname is a relative pathname that is
    //        interpreted relative to the directory referred to by dirfd.
    //        (See openat(2) for an explanation of why this is useful.)

    // By file descriptor(situation 4)
    //        If pathname is an empty string (or NULL since Linux 6.11)
    //        and the AT_EMPTY_PATH flag is specified in flags (see
    //        below), then the target file is the one referred to by the
    //        file descriptor dirfd.

    let path = nullable!(path.get_as_str())?;
    debug!(
        "sys_statx <= dirfd: {}, path: {:?}, flags: {}",
        dirfd, path, flags
    );

    *statxbuf.get_as_mut()? = if path.is_none_or(|s| s.is_empty()) {
        if (flags & AT_EMPTY_PATH) == 0 {
            return Err(LinuxError::ENOENT);
        }
        let f = get_file_like(dirfd)?;
        f.stat()?.into()
    } else {
        let path = handle_file_path(dirfd, path.unwrap_or_default())?;
        stat_at_path(path.as_str())?.into()
    };

    Ok(0)
}
