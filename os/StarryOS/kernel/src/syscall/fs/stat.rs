use core::{
    ffi::{c_char, c_int},
    mem::{offset_of, size_of},
};

use ax_fs_ng::vfs::current_fs_context;
use axfs_ng_vfs::Location;
use linux_raw_sys::general::{
    __kernel_fsid_t, AT_EACCESS, AT_EMPTY_PATH, AT_NO_AUTOMOUNT, AT_STATX_SYNC_TYPE,
    AT_SYMLINK_FOLLOW, AT_SYMLINK_NOFOLLOW, CAP_DAC_READ_SEARCH, R_OK, S_IFBLK, S_IFCHR, S_IFDIR,
    S_IFIFO, S_IFLNK, S_IFMT, S_IFREG, S_IFSOCK, STATX__RESERVED, W_OK, X_OK, stat, statfs, statx,
};

use crate::{
    Errno, StarryError, StarryResult,
    file::{
        Directory, File, ResolveAtResult, get_file_like, memfd::Memfd, metadata_to_kstat,
        resolve_at, resolve_at_checked, resolve_fd,
    },
    mm::{UserPtr, VmMutPtr, VmPtr, vm_load_path_string},
};

const FILE_HANDLE_BYTES: usize = size_of::<u64>() * 2;

const FILE_HANDLE_TYPE_DEV_INO: i32 = 1;

const MS_NOSUID: u32 = 1 << 1;

const MS_NODEV: u32 = 1 << 2;

const MS_NOEXEC: u32 = 1 << 3;

const MS_NOATIME: u32 = 1 << 10;

const MS_RELATIME: u32 = 1 << 21;

const ST_RDONLY: u32 = 1;

const ST_RELATIME: u32 = 1 << 12;

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
pub fn sys_stat(
    current: &crate::task::UserTaskRef,
    path: *const c_char,
    statbuf: *mut stat,
) -> crate::StarryResult<isize> {
    use linux_raw_sys::general::AT_FDCWD;

    sys_fstatat(current, AT_FDCWD, path, statbuf, 0)
}

/// Get file metadata by `fd` and write into `statbuf`.
///
/// Return 0 if success.
pub fn sys_fstat(
    current: &crate::task::UserTaskRef,
    fd: i32,
    statbuf: *mut stat,
) -> crate::StarryResult<isize> {
    write_stat(current, statbuf, resolve_fd(fd)?.stat()?.into())?;
    Ok(0)
}

/// Get the metadata of the symbolic link and write into `buf`.
///
/// Return 0 if success.
#[cfg(target_arch = "x86_64")]
pub fn sys_lstat(
    current: &crate::task::UserTaskRef,
    path: *const c_char,
    statbuf: *mut stat,
) -> crate::StarryResult<isize> {
    use linux_raw_sys::general::{AT_FDCWD, AT_SYMLINK_NOFOLLOW};

    sys_fstatat(current, AT_FDCWD, path, statbuf, AT_SYMLINK_NOFOLLOW)
}

pub fn sys_fstatat(
    current: &crate::task::UserTaskRef,
    dirfd: i32,
    path: *const c_char,
    statbuf: *mut stat,
    flags: u32,
) -> StarryResult<isize> {
    // man 2 fstatat: flags may contain AT_EMPTY_PATH, AT_NO_AUTOMOUNT,
    // AT_SYMLINK_NOFOLLOW. Any other bit is EINVAL.
    const FSTATAT_VALID: u32 = AT_EMPTY_PATH | AT_NO_AUTOMOUNT | AT_SYMLINK_NOFOLLOW;
    if flags & !FSTATAT_VALID != 0 {
        return Err(StarryError::InvalidInput);
    }

    let path = path
        .nullable()
        .map(|path| vm_load_path_string(current, path))
        .transpose()?;

    debug!("sys_fstatat <= dirfd: {dirfd}, path: {path:?}, flags: {flags}");

    let loc = resolve_at(dirfd, path.as_deref(), flags)?;
    write_stat(current, statbuf, loc.stat()?.into())?;

    Ok(0)
}

pub fn sys_statx(
    current: &crate::task::UserTaskRef,
    dirfd: c_int,
    path: *const c_char,
    flags: u32,
    mask: u32,
    statxbuf: *mut statx,
) -> StarryResult<isize> {
    // man 2 statx: reject reserved mask bits and the invalid sync-type
    // combination FORCE_SYNC|DONT_SYNC. flags must fit within AT_* and the
    // sync-type field.
    if mask & STATX__RESERVED != 0 {
        return Err(StarryError::InvalidInput);
    }
    if flags & AT_STATX_SYNC_TYPE == AT_STATX_SYNC_TYPE {
        return Err(StarryError::InvalidInput);
    }
    const STATX_VALID_FLAGS: u32 =
        AT_EMPTY_PATH | AT_NO_AUTOMOUNT | AT_SYMLINK_NOFOLLOW | AT_STATX_SYNC_TYPE;
    if flags & !STATX_VALID_FLAGS != 0 {
        return Err(StarryError::InvalidInput);
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

    let path = path
        .nullable()
        .map(|path| vm_load_path_string(current, path))
        .transpose()?;
    debug!("sys_statx <= dirfd: {dirfd}, path: {path:?}, flags: {flags}");

    let resolved = resolve_at(dirfd, path.as_deref(), flags)?;
    let mut status: statx = resolved.stat()?.into();
    if let ResolveAtResult::File(location) = &resolved {
        status.stx_mask |= linux_raw_sys::general::STATX_MNT_ID;
        status.stx_mnt_id = location.mountpoint().mount_id();
        if location.is_root_of_mount() {
            status.stx_attributes |= linux_raw_sys::general::STATX_ATTR_MOUNT_ROOT as u64;
        }
    }
    write_statx(current, statxbuf, status)?;

    Ok(0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_access(
    current: &crate::task::UserTaskRef,
    path: *const c_char,
    mode: u32,
) -> crate::StarryResult<isize> {
    use linux_raw_sys::general::AT_FDCWD;

    sys_faccessat2(current, AT_FDCWD, path, mode, 0)
}

pub fn sys_faccessat2(
    current: &crate::task::UserTaskRef,
    dirfd: c_int,
    path: *const c_char,
    mode: u32,
    flags: u32,
) -> crate::StarryResult<isize> {
    // man 2 access: mode is a mask of F_OK(0), R_OK, W_OK, and X_OK;
    // faccessat2 flags are limited to AT_EACCESS, AT_EMPTY_PATH, and
    // AT_SYMLINK_NOFOLLOW. Linux rejects invalid bits before path resolution.
    const FACCESSAT2_VALID_FLAGS: u32 = AT_EACCESS | AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW;
    const FACCESSAT2_VALID_MODE: u32 = R_OK | W_OK | X_OK;
    if mode & !FACCESSAT2_VALID_MODE != 0 || flags & !FACCESSAT2_VALID_FLAGS != 0 {
        return Err(StarryError::InvalidInput);
    }

    let path = path
        .nullable()
        .map(|path| vm_load_path_string(current, path))
        .transpose()?;
    debug!("sys_faccessat2 <= dirfd: {dirfd}, path: {path:?}, mode: {mode}, flags: {flags}");

    let current_cred = current.as_thread().cred();
    let cred = if flags & AT_EACCESS != 0 {
        (*current_cred).clone()
    } else {
        current_cred.for_real_id_access()
    };
    let file = resolve_at_checked(dirfd, path.as_deref(), flags, |directory| {
        let metadata = metadata_to_kstat(&directory.metadata()?);
        check_dac_access(&cred, &metadata, X_OK).map_err(axfs_ng_vfs::VfsError::from)
    })?;
    if mode == 0 {
        return Ok(0);
    }
    let metadata = file.stat()?;
    let node_type = metadata.mode & S_IFMT;
    if let ResolveAtResult::File(location) = &file {
        if mode & X_OK != 0
            && node_type == S_IFREG
            && location.mountpoint().mount_flags() & MS_NOEXEC != 0
        {
            return Err(StarryError::PermissionDenied);
        }
        // Linux sb_permission precedes DAC; a bind-only restriction does not.
        if mode & W_OK != 0
            && matches!(node_type, S_IFREG | S_IFDIR | S_IFLNK)
            && location.mountpoint().is_filesystem_readonly()
        {
            return Err(StarryError::ReadOnlyFilesystem);
        }
    }
    check_dac_access(&cred, &metadata, mode)?;
    if mode & W_OK != 0
        && !matches!(node_type, S_IFBLK | S_IFCHR | S_IFIFO | S_IFSOCK)
        && let ResolveAtResult::File(location) = &file
        && location.is_readonly()
    {
        return Err(StarryError::ReadOnlyFilesystem);
    }
    Ok(0)
}

fn check_dac_access(
    cred: &crate::task::Cred,
    kstat: &crate::file::Kstat,
    mode: u32,
) -> StarryResult<()> {
    let permission = if cred.fsuid == kstat.uid {
        (kstat.mode >> 6) & 0o7
    } else if cred.in_group(kstat.gid) {
        (kstat.mode >> 3) & 0o7
    } else {
        kstat.mode & 0o7
    };
    if mode & !permission == 0 {
        return Ok(());
    }
    let read_search = cred.has_cap(CAP_DAC_READ_SEARCH);
    if kstat.mode & S_IFMT == S_IFDIR {
        if cred.has_cap_dac_override() || (mode & W_OK == 0 && read_search) {
            return Ok(());
        }
    } else if (mode == R_OK && read_search)
        || (cred.has_cap_dac_override() && (mode & X_OK == 0 || kstat.mode & 0o111 != 0))
    {
        return Ok(());
    }
    Err(StarryError::PermissionDenied)
}

fn statfs(loc: &Location) -> StarryResult<statfs> {
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
    result.f_flags = (stat.mount_flags | statfs_mount_flags(loc)) as _;
    Ok(result)
}

fn statfs_mount_flags(loc: &Location) -> u32 {
    let mountpoint = loc.mountpoint();
    let mount_flags = mountpoint.mount_flags();
    let mut statfs_flags = mount_flags & (MS_NOSUID | MS_NODEV | MS_NOEXEC | MS_NOATIME);
    if loc.is_readonly() {
        statfs_flags |= ST_RDONLY;
    }
    if mount_flags & MS_RELATIME != 0 {
        statfs_flags |= ST_RELATIME;
    }
    statfs_flags
}

pub fn sys_statfs(
    current: &crate::task::UserTaskRef,
    path: *const c_char,
    buf: *mut statfs,
) -> crate::StarryResult<isize> {
    let path = vm_load_path_string(current, path)?;
    debug!("sys_statfs <= path: {path:?}");

    let location = current_fs_context().lock().resolve(path)?;
    write_statfs(
        current,
        buf,
        statfs(&location.mountpoint().root_location())?,
    )?;
    Ok(0)
}

pub fn sys_fstatfs(
    current: &crate::task::UserTaskRef,
    fd: i32,
    buf: *mut statfs,
) -> crate::StarryResult<isize> {
    debug!("sys_fstatfs <= fd: {fd}");

    let file_like = get_file_like(fd)?;
    let location = if let Some(directory) = file_like.downcast_ref::<Directory>() {
        directory.inner()
    } else if let Some(file) = file_like.downcast_ref::<File>() {
        file.inner().location()
    } else if let Some(memfd) = file_like.downcast_ref::<Memfd>() {
        memfd.inner().inner().location()
    } else {
        return Err(StarryError::InvalidInput);
    };
    write_statfs(current, buf, statfs(location)?)?;
    Ok(0)
}

fn write_stat(
    current: &crate::task::UserTaskRef,
    user: *mut stat,
    value: stat,
) -> crate::StarryResult<()> {
    let mut bytes = [0_u8; size_of::<stat>()];
    UserPtr::from(user).write_abi_fields(current, &mut bytes, |fields| {
        fields.put_field(offset_of!(stat, st_dev), &value.st_dev)?;
        fields.put_field(offset_of!(stat, st_ino), &value.st_ino)?;
        fields.put_field(offset_of!(stat, st_nlink), &value.st_nlink)?;
        fields.put_field(offset_of!(stat, st_mode), &value.st_mode)?;
        fields.put_field(offset_of!(stat, st_uid), &value.st_uid)?;
        fields.put_field(offset_of!(stat, st_gid), &value.st_gid)?;
        #[cfg(target_arch = "x86_64")]
        fields.put_field(offset_of!(stat, __pad0), &value.__pad0)?;
        fields.put_field(offset_of!(stat, st_rdev), &value.st_rdev)?;
        #[cfg(not(target_arch = "x86_64"))]
        fields.put_field(offset_of!(stat, __pad1), &value.__pad1)?;
        fields.put_field(offset_of!(stat, st_size), &value.st_size)?;
        fields.put_field(offset_of!(stat, st_blksize), &value.st_blksize)?;
        #[cfg(not(target_arch = "x86_64"))]
        fields.put_field(offset_of!(stat, __pad2), &value.__pad2)?;
        fields.put_field(offset_of!(stat, st_blocks), &value.st_blocks)?;
        fields.put_field(offset_of!(stat, st_atime), &value.st_atime)?;
        fields.put_field(offset_of!(stat, st_atime_nsec), &value.st_atime_nsec)?;
        fields.put_field(offset_of!(stat, st_mtime), &value.st_mtime)?;
        fields.put_field(offset_of!(stat, st_mtime_nsec), &value.st_mtime_nsec)?;
        fields.put_field(offset_of!(stat, st_ctime), &value.st_ctime)?;
        fields.put_field(offset_of!(stat, st_ctime_nsec), &value.st_ctime_nsec)?;
        #[cfg(target_arch = "x86_64")]
        fields.put_field(offset_of!(stat, __unused), &value.__unused)?;
        #[cfg(not(target_arch = "x86_64"))]
        {
            fields.put_field(offset_of!(stat, __unused4), &value.__unused4)?;
            fields.put_field(offset_of!(stat, __unused5), &value.__unused5)?;
        }
        Ok(())
    })
}

fn write_statx_timestamp(
    current: &crate::task::UserTaskRef,
    user: UserPtr<statx>,
    offset: usize,
    value: linux_raw_sys::general::statx_timestamp,
) -> crate::StarryResult<()> {
    use linux_raw_sys::general::statx_timestamp;

    user.write_field(
        current,
        offset + offset_of!(statx_timestamp, tv_sec),
        value.tv_sec,
    )?;
    user.write_field(
        current,
        offset + offset_of!(statx_timestamp, tv_nsec),
        value.tv_nsec,
    )?;
    user.write_field(
        current,
        offset + offset_of!(statx_timestamp, __reserved),
        value.__reserved,
    )
}

fn write_statx(
    current: &crate::task::UserTaskRef,
    user: *mut statx,
    value: statx,
) -> crate::StarryResult<()> {
    let user = UserPtr::from(user);
    user.write_field(current, offset_of!(statx, stx_mask), value.stx_mask)?;
    user.write_field(current, offset_of!(statx, stx_blksize), value.stx_blksize)?;
    user.write_field(
        current,
        offset_of!(statx, stx_attributes),
        value.stx_attributes,
    )?;
    user.write_field(current, offset_of!(statx, stx_nlink), value.stx_nlink)?;
    user.write_field(current, offset_of!(statx, stx_uid), value.stx_uid)?;
    user.write_field(current, offset_of!(statx, stx_gid), value.stx_gid)?;
    user.write_field(current, offset_of!(statx, stx_mode), value.stx_mode)?;
    user.write_field(current, offset_of!(statx, __spare0), value.__spare0)?;
    user.write_field(current, offset_of!(statx, stx_ino), value.stx_ino)?;
    user.write_field(current, offset_of!(statx, stx_size), value.stx_size)?;
    user.write_field(current, offset_of!(statx, stx_blocks), value.stx_blocks)?;
    user.write_field(
        current,
        offset_of!(statx, stx_attributes_mask),
        value.stx_attributes_mask,
    )?;
    write_statx_timestamp(current, user, offset_of!(statx, stx_atime), value.stx_atime)?;
    write_statx_timestamp(current, user, offset_of!(statx, stx_btime), value.stx_btime)?;
    write_statx_timestamp(current, user, offset_of!(statx, stx_ctime), value.stx_ctime)?;
    write_statx_timestamp(current, user, offset_of!(statx, stx_mtime), value.stx_mtime)?;
    user.write_field(
        current,
        offset_of!(statx, stx_rdev_major),
        value.stx_rdev_major,
    )?;
    user.write_field(
        current,
        offset_of!(statx, stx_rdev_minor),
        value.stx_rdev_minor,
    )?;
    user.write_field(
        current,
        offset_of!(statx, stx_dev_major),
        value.stx_dev_major,
    )?;
    user.write_field(
        current,
        offset_of!(statx, stx_dev_minor),
        value.stx_dev_minor,
    )?;
    user.write_field(current, offset_of!(statx, stx_mnt_id), value.stx_mnt_id)?;
    user.write_field(
        current,
        offset_of!(statx, stx_dio_mem_align),
        value.stx_dio_mem_align,
    )?;
    user.write_field(
        current,
        offset_of!(statx, stx_dio_offset_align),
        value.stx_dio_offset_align,
    )?;
    user.write_field(current, offset_of!(statx, stx_subvol), value.stx_subvol)?;
    user.write_field(
        current,
        offset_of!(statx, stx_atomic_write_unit_min),
        value.stx_atomic_write_unit_min,
    )?;
    user.write_field(
        current,
        offset_of!(statx, stx_atomic_write_unit_max),
        value.stx_atomic_write_unit_max,
    )?;
    user.write_field(
        current,
        offset_of!(statx, stx_atomic_write_segments_max),
        value.stx_atomic_write_segments_max,
    )?;
    user.write_field(
        current,
        offset_of!(statx, stx_dio_read_offset_align),
        value.stx_dio_read_offset_align,
    )?;
    user.write_field(
        current,
        offset_of!(statx, stx_atomic_write_unit_max_opt),
        value.stx_atomic_write_unit_max_opt,
    )?;
    user.write_field(current, offset_of!(statx, __spare2), value.__spare2)?;
    user.write_field(current, offset_of!(statx, __spare3), value.__spare3)
}

fn write_statfs(
    current: &crate::task::UserTaskRef,
    user: *mut statfs,
    value: statfs,
) -> crate::StarryResult<()> {
    let user = UserPtr::from(user);
    user.write_field(current, offset_of!(statfs, f_type), value.f_type)?;
    user.write_field(current, offset_of!(statfs, f_bsize), value.f_bsize)?;
    user.write_field(current, offset_of!(statfs, f_blocks), value.f_blocks)?;
    user.write_field(current, offset_of!(statfs, f_bfree), value.f_bfree)?;
    user.write_field(current, offset_of!(statfs, f_bavail), value.f_bavail)?;
    user.write_field(current, offset_of!(statfs, f_files), value.f_files)?;
    user.write_field(current, offset_of!(statfs, f_ffree), value.f_ffree)?;
    user.write_field(
        current,
        offset_of!(statfs, f_fsid) + offset_of!(__kernel_fsid_t, val),
        value.f_fsid.val,
    )?;
    user.write_field(current, offset_of!(statfs, f_namelen), value.f_namelen)?;
    user.write_field(current, offset_of!(statfs, f_frsize), value.f_frsize)?;
    user.write_field(current, offset_of!(statfs, f_flags), value.f_flags)?;
    user.write_field(current, offset_of!(statfs, f_spare), value.f_spare)
}

pub fn sys_name_to_handle_at(
    current: &crate::task::UserTaskRef,
    dirfd: c_int,
    path: *const c_char,
    handle: *mut FileHandleHeader,
    mount_id: *mut c_int,
    flags: u32,
) -> StarryResult<isize> {
    const VALID_FLAGS: u32 = AT_EMPTY_PATH | AT_SYMLINK_FOLLOW;
    if flags & !VALID_FLAGS != 0 {
        return Err(StarryError::InvalidInput);
    }

    let path = path
        .nullable()
        .map(|path| vm_load_path_string(current, path))
        .transpose()?;
    debug!("sys_name_to_handle_at <= dirfd: {dirfd}, path: {path:?}, flags: {flags}");

    let resolve_flags = if flags & AT_SYMLINK_FOLLOW != 0 {
        flags & AT_EMPTY_PATH
    } else {
        (flags & AT_EMPTY_PATH) | AT_SYMLINK_NOFOLLOW
    };
    let loc = resolve_at(dirfd, path.as_deref(), resolve_flags)?
        .into_file()
        .ok_or(StarryError::InvalidInput)?;
    let stat = loc.metadata()?;

    let header_ptr = UserPtr::<FileHandleHeader>::from(handle);
    let mut header = header_ptr.read(current)?;
    let capacity = header.handle_bytes as usize;
    header.handle_bytes = FILE_HANDLE_BYTES as u32;
    handle.vm_write(current, header)?;
    if capacity < FILE_HANDLE_BYTES {
        header_ptr.write(current, header)?;
        return Err(crate::StarryError::from(crate::Errno::EOVERFLOW));
    }

    header.handle_type = FILE_HANDLE_TYPE_DEV_INO;
    header_ptr.write(current, header)?;
    let mut bytes = [0u8; FILE_HANDLE_BYTES];
    bytes[..size_of::<u64>()].copy_from_slice(&stat.device.to_ne_bytes());
    bytes[size_of::<u64>()..].copy_from_slice(&stat.inode.to_ne_bytes());
    let data_ptr = (handle as usize)
        .checked_add(size_of::<FileHandleHeader>())
        .ok_or(crate::StarryError::InvalidInput)? as *mut u8;
    UserPtr::<u8>::from(data_ptr).write_slice(current, &bytes)?;

    let resolved_mount_id = c_int::try_from(loc.mountpoint().mount_id())
        .map_err(|_| StarryError::from(Errno::EOVERFLOW))?;
    (mount_id as *mut c_int).vm_write(current, resolved_mount_id)?;
    Ok(0)
}

#[cfg(all(test, not(axtest)))]
mod access_tests {
    use linux_raw_sys::general::{CAP_DAC_READ_SEARCH, R_OK, S_IFDIR, S_IFREG, X_OK};

    use super::check_dac_access;
    use crate::{StarryError, file::Kstat, task::Cred};

    #[test]
    fn uid_zero_without_dac_capability_obeys_file_mode() {
        let mut cred = Cred::root();
        cred.cap_effective = 0;
        let target = Kstat {
            mode: S_IFREG,
            uid: 1000,
            ..Kstat::default()
        };
        assert!(matches!(
            check_dac_access(&cred, &target, R_OK),
            Err(StarryError::PermissionDenied)
        ));
    }

    #[test]
    fn directory_search_capability_does_not_require_execute_bits() {
        let mut cred = Cred::root();
        cred.fsuid = 1000;
        cred.cap_effective = 1 << CAP_DAC_READ_SEARCH;
        let target = Kstat {
            mode: S_IFDIR,
            uid: 2000,
            ..Kstat::default()
        };
        assert!(check_dac_access(&cred, &target, X_OK).is_ok());
        assert!(check_dac_access(&Cred::root(), &target, X_OK).is_ok());
    }

    #[test]
    fn file_read_search_capability_only_overrides_read_access() {
        let mut cred = Cred::root();
        cred.fsuid = 1000;
        cred.cap_effective = 1 << CAP_DAC_READ_SEARCH;
        let target = Kstat {
            mode: S_IFREG,
            uid: 2000,
            ..Kstat::default()
        };
        assert!(check_dac_access(&cred, &target, R_OK).is_ok());
        assert!(matches!(
            check_dac_access(&cred, &target, X_OK),
            Err(StarryError::PermissionDenied)
        ));
    }
}
