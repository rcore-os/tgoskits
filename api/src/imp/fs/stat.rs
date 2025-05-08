use core::ffi::{c_char, c_int};

use axerrno::{LinuxError, LinuxResult};
use axfs_ng::FS_CONTEXT;
use linux_raw_sys::general::{AT_EMPTY_PATH, stat, statx};

use crate::{
    file::{Kstat, get_file_like, metadata_to_kstat, with_fs},
    ptr::{UserConstPtr, UserPtr, nullable},
};

/// Get the file metadata by `path` and write into `statbuf`.
///
/// Return 0 if success.
pub fn sys_stat(path: UserConstPtr<c_char>, statbuf: UserPtr<stat>) -> LinuxResult<isize> {
    let path = path.get_as_str()?;
    let statbuf = statbuf.get_as_mut()?;

    let metadata = FS_CONTEXT.lock().metadata(path)?;
    *statbuf = metadata_to_kstat(&metadata).into();

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

fn kstat_at(dirfd: i32, path: Option<&str>, flags: u32) -> LinuxResult<Kstat> {
    Ok(match path {
        Some("") | None => {
            if flags & AT_EMPTY_PATH == 0 {
                return Err(LinuxError::ENOENT);
            }
            let f = get_file_like(dirfd)?;
            f.stat()?
        }
        Some(path) => {
            let metadata = with_fs(dirfd, |fs| fs.metadata(path))?;
            metadata_to_kstat(&metadata)
        }
    })
}

pub fn sys_fstatat(
    dirfd: i32,
    path: UserConstPtr<c_char>,
    statbuf: UserPtr<stat>,
    flags: u32,
) -> LinuxResult<isize> {
    let path = nullable!(path.get_as_str())?;
    let statbuf = statbuf.get_as_mut()?;
    *statbuf = kstat_at(dirfd, path, flags)?.into();
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

    let statxbuf = statxbuf.get_as_mut()?;
    *statxbuf = kstat_at(dirfd, path, flags)?.into();

    Ok(0)
}
