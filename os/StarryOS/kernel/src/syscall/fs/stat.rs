use core::{
    ffi::{c_char, c_int},
    mem::{offset_of, size_of},
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs_ng::vfs::current_fs_context;
use axfs_ng_vfs::{Location, NodePermission};
use linux_raw_sys::general::{
    __kernel_fsid_t, AT_EACCESS, AT_EMPTY_PATH, AT_NO_AUTOMOUNT, AT_STATX_SYNC_TYPE,
    AT_SYMLINK_FOLLOW, AT_SYMLINK_NOFOLLOW, R_OK, STATX__RESERVED, W_OK, X_OK, stat, statfs, statx,
};
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    file::{File, FileLike, resolve_at},
    mm::{UserPtr, vm_load_path_string},
    task::current_user_task,
};

const FILE_HANDLE_BYTES: usize = size_of::<u64>() * 2;
const FILE_HANDLE_TYPE_DEV_INO: i32 = 1;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
pub struct FileHandleHeader {
    handle_bytes: u32,
    handle_type: i32,
}

/// Get the file metadata by `path` and write into `statbuf`.
///
/// Return 0 if success.
#[cfg(target_arch = "x86_64")]
pub fn sys_stat(path: *const c_char, statbuf: *mut stat) -> AxResult<isize> {
    use linux_raw_sys::general::AT_FDCWD;

    sys_fstatat(AT_FDCWD, path, statbuf, 0)
}

/// Get file metadata by `fd` and write into `statbuf`.
///
/// Return 0 if success.
pub fn sys_fstat(fd: i32, statbuf: *mut stat) -> AxResult<isize> {
    sys_fstatat(fd, core::ptr::null(), statbuf, AT_EMPTY_PATH)
}

/// Get the metadata of the symbolic link and write into `buf`.
///
/// Return 0 if success.
#[cfg(target_arch = "x86_64")]
pub fn sys_lstat(path: *const c_char, statbuf: *mut stat) -> AxResult<isize> {
    use linux_raw_sys::general::{AT_FDCWD, AT_SYMLINK_NOFOLLOW};

    sys_fstatat(AT_FDCWD, path, statbuf, AT_SYMLINK_NOFOLLOW)
}

pub fn sys_fstatat(
    dirfd: i32,
    path: *const c_char,
    statbuf: *mut stat,
    flags: u32,
) -> AxResult<isize> {
    // man 2 fstatat: flags may contain AT_EMPTY_PATH, AT_NO_AUTOMOUNT,
    // AT_SYMLINK_NOFOLLOW. Any other bit is EINVAL.
    const FSTATAT_VALID: u32 = AT_EMPTY_PATH | AT_NO_AUTOMOUNT | AT_SYMLINK_NOFOLLOW;
    if flags & !FSTATAT_VALID != 0 {
        return Err(AxError::InvalidInput);
    }

    let path = path.nullable().map(vm_load_path_string).transpose()?;

    debug!("sys_fstatat <= dirfd: {dirfd}, path: {path:?}, flags: {flags}");

    let loc = resolve_at(dirfd, path.as_deref(), flags)?;
    write_stat(statbuf, loc.stat()?.into())?;

    Ok(0)
}

pub fn sys_statx(
    dirfd: c_int,
    path: *const c_char,
    flags: u32,
    mask: u32,
    statxbuf: *mut statx,
) -> AxResult<isize> {
    // man 2 statx: reject reserved mask bits and the invalid sync-type
    // combination FORCE_SYNC|DONT_SYNC. flags must fit within AT_* and the
    // sync-type field.
    if mask & STATX__RESERVED != 0 {
        return Err(AxError::InvalidInput);
    }
    if flags & AT_STATX_SYNC_TYPE == AT_STATX_SYNC_TYPE {
        return Err(AxError::InvalidInput);
    }
    const STATX_VALID_FLAGS: u32 =
        AT_EMPTY_PATH | AT_NO_AUTOMOUNT | AT_SYMLINK_NOFOLLOW | AT_STATX_SYNC_TYPE;
    if flags & !STATX_VALID_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }
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

    let path = path.nullable().map(vm_load_path_string).transpose()?;
    debug!("sys_statx <= dirfd: {dirfd}, path: {path:?}, flags: {flags}");

    write_statx(
        statxbuf,
        resolve_at(dirfd, path.as_deref(), flags)?.stat()?.into(),
    )?;

    Ok(0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_access(path: *const c_char, mode: u32) -> AxResult<isize> {
    use linux_raw_sys::general::AT_FDCWD;

    sys_faccessat2(AT_FDCWD, path, mode, 0)
}

// Note: AT_EACCESS is not explicitly handled. This is functionally correct
// because fsuid/fsgid track euid/egid by default in our credential model,
// so the real-ID vs effective-ID distinction AT_EACCESS controls is a no-op.
pub fn sys_faccessat2(dirfd: c_int, path: *const c_char, mode: u32, flags: u32) -> AxResult<isize> {
    // man 2 access: mode is a mask of F_OK(0), R_OK, W_OK, and X_OK;
    // faccessat2 flags are limited to AT_EACCESS, AT_EMPTY_PATH, and
    // AT_SYMLINK_NOFOLLOW. Linux rejects invalid bits before path resolution.
    const FACCESSAT2_VALID_FLAGS: u32 = AT_EACCESS | AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW;
    const FACCESSAT2_VALID_MODE: u32 = R_OK | W_OK | X_OK;
    if mode & !FACCESSAT2_VALID_MODE != 0 || flags & !FACCESSAT2_VALID_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }

    let path = path.nullable().map(vm_load_path_string).transpose()?;
    debug!("sys_faccessat2 <= dirfd: {dirfd}, path: {path:?}, mode: {mode}, flags: {flags}");

    let file = resolve_at(dirfd, path.as_deref(), flags)?;

    if mode == 0 {
        return Ok(0);
    }

    let cred = current_user_task().as_thread().cred();

    // Root (fsuid == 0) bypasses R_OK and W_OK checks.
    // For X_OK, at least one execute bit must be set (owner, group, or other).
    if cred.fsuid == 0 {
        if mode & X_OK != 0 {
            let perm_bits = file.stat()?.mode as u16;
            let any_exec = NodePermission::OWNER_EXEC.bits()
                | NodePermission::GROUP_EXEC.bits()
                | NodePermission::OTHER_EXEC.bits();
            if perm_bits & any_exec == 0 {
                return Err(AxError::PermissionDenied);
            }
        }
        return Ok(0);
    }

    let kstat = file.stat()?;
    let file_uid = kstat.uid;
    let file_gid = kstat.gid;
    let file_mode = kstat.mode;

    // Select effective permission bits based on owner/group/other matching.
    let effective_bits = if cred.fsuid == file_uid {
        (file_mode >> 6) & 0o7
    } else if cred.fsgid == file_gid || cred.groups.contains(&file_gid) {
        (file_mode >> 3) & 0o7
    } else {
        file_mode & 0o7
    };

    if (mode & R_OK != 0) && (effective_bits & 4 == 0) {
        return Err(AxError::PermissionDenied);
    }
    if (mode & W_OK != 0) && (effective_bits & 2 == 0) {
        return Err(AxError::PermissionDenied);
    }
    if (mode & X_OK != 0) && (effective_bits & 1 == 0) {
        return Err(AxError::PermissionDenied);
    }

    Ok(0)
}

fn statfs(loc: &Location) -> AxResult<statfs> {
    let stat = loc.filesystem().stat()?;
    // FIXME: Zeroable
    let mut result: statfs = unsafe { core::mem::zeroed() };
    result.f_type = stat.fs_type as _;
    result.f_bsize = stat.block_size as _;
    result.f_blocks = stat.blocks as _;
    result.f_bfree = stat.blocks_free as _;
    result.f_bavail = stat.blocks_available as _;
    result.f_files = stat.file_count as _;
    result.f_ffree = stat.free_file_count as _;
    // TODO: fsid
    result.f_fsid = __kernel_fsid_t {
        val: [0, loc.mountpoint().device() as _],
    };
    result.f_namelen = stat.name_length as _;
    result.f_frsize = stat.fragment_size as _;
    result.f_flags = stat.mount_flags as _;
    Ok(result)
}

pub fn sys_statfs(path: *const c_char, buf: *mut statfs) -> AxResult<isize> {
    let path = vm_load_path_string(path)?;
    debug!("sys_statfs <= path: {path:?}");

    let location = current_fs_context().lock().resolve(path)?;
    write_statfs(buf, statfs(&location.mountpoint().root_location())?)?;
    Ok(0)
}

pub fn sys_fstatfs(fd: i32, buf: *mut statfs) -> AxResult<isize> {
    debug!("sys_fstatfs <= fd: {fd}");

    write_statfs(buf, statfs(File::from_fd(fd)?.inner().location())?)?;
    Ok(0)
}

fn write_stat(user: *mut stat, value: stat) -> AxResult<()> {
    let user = UserPtr::from(user);
    user.write_field(offset_of!(stat, st_dev), value.st_dev)?;
    user.write_field(offset_of!(stat, st_ino), value.st_ino)?;
    user.write_field(offset_of!(stat, st_nlink), value.st_nlink)?;
    user.write_field(offset_of!(stat, st_mode), value.st_mode)?;
    user.write_field(offset_of!(stat, st_uid), value.st_uid)?;
    user.write_field(offset_of!(stat, st_gid), value.st_gid)?;
    #[cfg(target_arch = "x86_64")]
    user.write_field(offset_of!(stat, __pad0), value.__pad0)?;
    user.write_field(offset_of!(stat, st_rdev), value.st_rdev)?;
    #[cfg(not(target_arch = "x86_64"))]
    user.write_field(offset_of!(stat, __pad1), value.__pad1)?;
    user.write_field(offset_of!(stat, st_size), value.st_size)?;
    user.write_field(offset_of!(stat, st_blksize), value.st_blksize)?;
    #[cfg(not(target_arch = "x86_64"))]
    user.write_field(offset_of!(stat, __pad2), value.__pad2)?;
    user.write_field(offset_of!(stat, st_blocks), value.st_blocks)?;
    user.write_field(offset_of!(stat, st_atime), value.st_atime)?;
    user.write_field(offset_of!(stat, st_atime_nsec), value.st_atime_nsec)?;
    user.write_field(offset_of!(stat, st_mtime), value.st_mtime)?;
    user.write_field(offset_of!(stat, st_mtime_nsec), value.st_mtime_nsec)?;
    user.write_field(offset_of!(stat, st_ctime), value.st_ctime)?;
    user.write_field(offset_of!(stat, st_ctime_nsec), value.st_ctime_nsec)?;
    #[cfg(target_arch = "x86_64")]
    user.write_field(offset_of!(stat, __unused), value.__unused)?;
    #[cfg(not(target_arch = "x86_64"))]
    {
        user.write_field(offset_of!(stat, __unused4), value.__unused4)?;
        user.write_field(offset_of!(stat, __unused5), value.__unused5)?;
    }
    Ok(())
}

fn write_statx_timestamp(
    user: UserPtr<statx>,
    offset: usize,
    value: linux_raw_sys::general::statx_timestamp,
) -> AxResult<()> {
    use linux_raw_sys::general::statx_timestamp;

    user.write_field(offset + offset_of!(statx_timestamp, tv_sec), value.tv_sec)?;
    user.write_field(offset + offset_of!(statx_timestamp, tv_nsec), value.tv_nsec)?;
    user.write_field(
        offset + offset_of!(statx_timestamp, __reserved),
        value.__reserved,
    )
}

fn write_statx(user: *mut statx, value: statx) -> AxResult<()> {
    let user = UserPtr::from(user);
    user.write_field(offset_of!(statx, stx_mask), value.stx_mask)?;
    user.write_field(offset_of!(statx, stx_blksize), value.stx_blksize)?;
    user.write_field(offset_of!(statx, stx_attributes), value.stx_attributes)?;
    user.write_field(offset_of!(statx, stx_nlink), value.stx_nlink)?;
    user.write_field(offset_of!(statx, stx_uid), value.stx_uid)?;
    user.write_field(offset_of!(statx, stx_gid), value.stx_gid)?;
    user.write_field(offset_of!(statx, stx_mode), value.stx_mode)?;
    user.write_field(offset_of!(statx, __spare0), value.__spare0)?;
    user.write_field(offset_of!(statx, stx_ino), value.stx_ino)?;
    user.write_field(offset_of!(statx, stx_size), value.stx_size)?;
    user.write_field(offset_of!(statx, stx_blocks), value.stx_blocks)?;
    user.write_field(
        offset_of!(statx, stx_attributes_mask),
        value.stx_attributes_mask,
    )?;
    write_statx_timestamp(user, offset_of!(statx, stx_atime), value.stx_atime)?;
    write_statx_timestamp(user, offset_of!(statx, stx_btime), value.stx_btime)?;
    write_statx_timestamp(user, offset_of!(statx, stx_ctime), value.stx_ctime)?;
    write_statx_timestamp(user, offset_of!(statx, stx_mtime), value.stx_mtime)?;
    user.write_field(offset_of!(statx, stx_rdev_major), value.stx_rdev_major)?;
    user.write_field(offset_of!(statx, stx_rdev_minor), value.stx_rdev_minor)?;
    user.write_field(offset_of!(statx, stx_dev_major), value.stx_dev_major)?;
    user.write_field(offset_of!(statx, stx_dev_minor), value.stx_dev_minor)?;
    user.write_field(offset_of!(statx, stx_mnt_id), value.stx_mnt_id)?;
    user.write_field(
        offset_of!(statx, stx_dio_mem_align),
        value.stx_dio_mem_align,
    )?;
    user.write_field(
        offset_of!(statx, stx_dio_offset_align),
        value.stx_dio_offset_align,
    )?;
    user.write_field(offset_of!(statx, stx_subvol), value.stx_subvol)?;
    user.write_field(
        offset_of!(statx, stx_atomic_write_unit_min),
        value.stx_atomic_write_unit_min,
    )?;
    user.write_field(
        offset_of!(statx, stx_atomic_write_unit_max),
        value.stx_atomic_write_unit_max,
    )?;
    user.write_field(
        offset_of!(statx, stx_atomic_write_segments_max),
        value.stx_atomic_write_segments_max,
    )?;
    user.write_field(
        offset_of!(statx, stx_dio_read_offset_align),
        value.stx_dio_read_offset_align,
    )?;
    user.write_field(
        offset_of!(statx, stx_atomic_write_unit_max_opt),
        value.stx_atomic_write_unit_max_opt,
    )?;
    user.write_field(offset_of!(statx, __spare2), value.__spare2)?;
    user.write_field(offset_of!(statx, __spare3), value.__spare3)
}

fn write_statfs(user: *mut statfs, value: statfs) -> AxResult<()> {
    let user = UserPtr::from(user);
    user.write_field(offset_of!(statfs, f_type), value.f_type)?;
    user.write_field(offset_of!(statfs, f_bsize), value.f_bsize)?;
    user.write_field(offset_of!(statfs, f_blocks), value.f_blocks)?;
    user.write_field(offset_of!(statfs, f_bfree), value.f_bfree)?;
    user.write_field(offset_of!(statfs, f_bavail), value.f_bavail)?;
    user.write_field(offset_of!(statfs, f_files), value.f_files)?;
    user.write_field(offset_of!(statfs, f_ffree), value.f_ffree)?;
    user.write_field(
        offset_of!(statfs, f_fsid) + offset_of!(__kernel_fsid_t, val),
        value.f_fsid.val,
    )?;
    user.write_field(offset_of!(statfs, f_namelen), value.f_namelen)?;
    user.write_field(offset_of!(statfs, f_frsize), value.f_frsize)?;
    user.write_field(offset_of!(statfs, f_flags), value.f_flags)?;
    user.write_field(offset_of!(statfs, f_spare), value.f_spare)
}

pub fn sys_name_to_handle_at(
    dirfd: c_int,
    path: *const c_char,
    handle: *mut FileHandleHeader,
    mount_id: *mut c_int,
    flags: u32,
) -> AxResult<isize> {
    const VALID_FLAGS: u32 = AT_EMPTY_PATH | AT_SYMLINK_FOLLOW;
    if flags & !VALID_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }

    let path = path.nullable().map(vm_load_path_string).transpose()?;
    debug!("sys_name_to_handle_at <= dirfd: {dirfd}, path: {path:?}, flags: {flags}");

    let resolve_flags = if flags & AT_SYMLINK_FOLLOW != 0 {
        flags & AT_EMPTY_PATH
    } else {
        (flags & AT_EMPTY_PATH) | AT_SYMLINK_NOFOLLOW
    };
    let loc = resolve_at(dirfd, path.as_deref(), resolve_flags)?
        .into_file()
        .ok_or(AxError::InvalidInput)?;
    let stat = loc.metadata()?;

    let header_ptr = UserPtr::<FileHandleHeader>::from(handle);
    let mut header = header_ptr.read()?;
    let capacity = header.handle_bytes as usize;
    header.handle_bytes = FILE_HANDLE_BYTES as u32;
    if capacity < FILE_HANDLE_BYTES {
        header_ptr.write(header)?;
        return Err(AxError::from(LinuxError::EOVERFLOW));
    }

    header.handle_type = FILE_HANDLE_TYPE_DEV_INO;
    header_ptr.write(header)?;
    let mut bytes = [0u8; FILE_HANDLE_BYTES];
    bytes[..size_of::<u64>()].copy_from_slice(&stat.device.to_ne_bytes());
    bytes[size_of::<u64>()..].copy_from_slice(&stat.inode.to_ne_bytes());
    let data_ptr = (handle as usize)
        .checked_add(size_of::<FileHandleHeader>())
        .ok_or(AxError::InvalidInput)? as *mut u8;
    UserPtr::<u8>::from(data_ptr).write_slice(&bytes)?;

    (mount_id as *mut c_int).vm_write(loc.mountpoint().device() as c_int)?;
    Ok(0)
}
