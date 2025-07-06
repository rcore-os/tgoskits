use core::ffi::{c_char, c_int};

use axerrno::{LinuxError, LinuxResult};
use axfs_ng::FS_CONTEXT;
use axfs_ng_vfs::{Location, NodePermission};
use axsync::RawMutex;
use linux_raw_sys::general::{
    __kernel_fsid_t, AT_EMPTY_PATH, R_OK, W_OK, X_OK, stat, statfs, statx,
};

use crate::{
    file::{File, FileLike, resolve_at},
    ptr::{UserConstPtr, UserPtr, nullable},
};

/// Get the file metadata by `path` and write into `statbuf`.
///
/// Return 0 if success.
#[cfg(target_arch = "x86_64")]
pub fn sys_stat(path: UserConstPtr<c_char>, statbuf: UserPtr<stat>) -> LinuxResult<isize> {
    use linux_raw_sys::general::AT_FDCWD;

    sys_fstatat(AT_FDCWD, path, statbuf, 0)
}

/// Get file metadata by `fd` and write into `statbuf`.
///
/// Return 0 if success.
pub fn sys_fstat(fd: i32, statbuf: UserPtr<stat>) -> LinuxResult<isize> {
    sys_fstatat(fd, 0.into(), statbuf, AT_EMPTY_PATH)
}

/// Get the metadata of the symbolic link and write into `buf`.
///
/// Return 0 if success.
#[cfg(target_arch = "x86_64")]
pub fn sys_lstat(path: UserConstPtr<c_char>, statbuf: UserPtr<stat>) -> LinuxResult<isize> {
    use linux_raw_sys::general::{AT_FDCWD, AT_SYMLINK_FOLLOW};

    sys_fstatat(AT_FDCWD, path, statbuf, AT_SYMLINK_FOLLOW)
}

pub fn sys_fstatat(
    dirfd: i32,
    path: UserConstPtr<c_char>,
    statbuf: UserPtr<stat>,
    flags: u32,
) -> LinuxResult<isize> {
    let path = nullable!(path.get_as_str())?;
    let statbuf = statbuf.get_as_mut()?;

    *statbuf = resolve_at(dirfd, path, flags)?.stat()?.into();

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
    *statxbuf = resolve_at(dirfd, path, flags)?.stat()?.into();

    Ok(0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_access(path: UserConstPtr<c_char>, mode: u32) -> LinuxResult<isize> {
    use linux_raw_sys::general::AT_FDCWD;

    sys_faccessat2(AT_FDCWD, path, mode, 0)
}

pub fn sys_faccessat2(
    dirfd: c_int,
    path: UserConstPtr<c_char>,
    mode: u32,
    flags: u32,
) -> LinuxResult<isize> {
    let path = nullable!(path.get_as_str())?;
    info!(
        "sys_faccessat2 <= dirfd: {}, path: {:?}, mode: {}, flags: {}",
        dirfd, path, mode, flags
    );

    let file = resolve_at(dirfd, path, flags)?;

    if mode == 0 {
        return Ok(0);
    }
    let mut required_mode = NodePermission::empty();
    if mode & R_OK != 0 {
        required_mode |= NodePermission::OWNER_READ;
    }
    if mode & W_OK != 0 {
        required_mode |= NodePermission::OWNER_WRITE;
    }
    if mode & X_OK != 0 {
        required_mode |= NodePermission::OWNER_EXEC;
    }
    let required_mode = required_mode.bits();
    if (file.stat()?.mode as u16 & required_mode) != required_mode {
        return Err(LinuxError::EACCES);
    }

    Ok(0)
}

fn statfs(loc: &Location<RawMutex>, buf: UserPtr<statfs>) -> LinuxResult<()> {
    let stat = loc.filesystem().stat()?;
    let dest = buf.get_as_mut()?;
    dest.f_type = stat.fs_type as _;
    dest.f_bsize = stat.block_size as _;
    dest.f_blocks = stat.blocks as _;
    dest.f_bfree = stat.blocks_free as _;
    dest.f_bavail = stat.blocks_available as _;
    dest.f_files = stat.file_count as _;
    dest.f_ffree = stat.free_file_count as _;
    // TODO: fsid
    dest.f_fsid = __kernel_fsid_t {
        val: [0, loc.mountpoint().device() as _],
    };
    dest.f_namelen = stat.name_length as _;
    dest.f_frsize = stat.fragment_size as _;
    dest.f_flags = stat.mount_flags as _;
    Ok(())
}

pub fn sys_statfs(path: UserConstPtr<c_char>, buf: UserPtr<statfs>) -> LinuxResult<isize> {
    statfs(
        &FS_CONTEXT
            .lock()
            .resolve(path.get_as_str()?)?
            .mountpoint()
            .root_location(),
        buf,
    )?;
    Ok(0)
}

pub fn sys_fstatfs(fd: i32, buf: UserPtr<statfs>) -> LinuxResult<isize> {
    statfs(File::from_fd(fd)?.inner().inner(), buf)?;
    Ok(0)
}
