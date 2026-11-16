//! Metadata syscalls retain their input intent until the inode owner's attempt.

use core::ffi::c_char;
#[cfg(target_arch = "x86_64")]
use core::time::Duration;

use ax_fs_ng::vfs::FsContext;
use axfs_ng_vfs::{MetadataChange, MutationCredentials, NodePermission, PathHandle, TimeChange};
use linux_raw_sys::general::{
    AT_EMPTY_PATH, AT_FDCWD, AT_SYMLINK_NOFOLLOW, O_PATH, UTIME_NOW, UTIME_OMIT, timespec,
};

use super::mutation_credentials;
use crate::{
    StarryError, StarryResult,
    file::{descriptor_path, get_file_like, resolve_at_checked, with_fs},
    mm::{VmPtr, vm_load_path_string},
    task::{FsCredentialsSnapshot, UserTaskRef},
    time::TimeValueLike,
};

enum DescriptorMode {
    Ordinary,
    Path,
}

fn fd_target(fd: i32, mode: DescriptorMode) -> StarryResult<PathHandle> {
    let file = get_file_like(fd)?;
    if matches!(mode, DescriptorMode::Ordinary) && file.open_flags() & O_PATH != 0 {
        return Err(StarryError::BadFileDescriptor);
    }
    Ok(descriptor_path(&file)?)
}

fn at_target(
    current: &UserTaskRef,
    dirfd: i32,
    path: *const c_char,
    flags: u32,
    credentials: &MutationCredentials<'_>,
) -> StarryResult<PathHandle> {
    let path = if path.is_null() && flags & AT_EMPTY_PATH != 0 {
        None
    } else {
        Some(vm_load_path_string(current, path)?)
    };
    if path.as_deref().is_none_or(str::is_empty) && flags & AT_EMPTY_PATH != 0 {
        if dirfd == AT_FDCWD {
            return with_fs(AT_FDCWD, |fs| {
                Ok(PathHandle::acquire(fs.current_dir().clone())?)
            });
        }
        return fd_target(dirfd, DescriptorMode::Path);
    }
    let target = resolve_at_checked(dirfd, path.as_deref(), flags, |directory| {
        FsContext::check_permission(directory, credentials, NodePermission::OTHER_EXEC)
    })?
    .into_file()
    .ok_or(StarryError::BadFileDescriptor)?;
    Ok(PathHandle::acquire(target)?)
}

fn change_fd(current: &UserTaskRef, fd: i32, change: MetadataChange) -> StarryResult<isize> {
    let target = fd_target(fd, DescriptorMode::Ordinary)?;
    let identity = FsCredentialsSnapshot::capture(current.as_thread());
    target
        .location()
        .change_metadata(change, &mutation_credentials(&identity))?;
    Ok(0)
}

fn change_at(
    current: &UserTaskRef,
    dirfd: i32,
    path: *const c_char,
    flags: u32,
    change: MetadataChange,
) -> StarryResult<isize> {
    let identity = FsCredentialsSnapshot::capture(current.as_thread());
    let credentials = mutation_credentials(&identity);
    let target = at_target(current, dirfd, path, flags, &credentials)?;
    target.location().change_metadata(change, &credentials)?;
    Ok(0)
}

fn ownership_change(uid: i32, gid: i32) -> MetadataChange {
    MetadataChange::Chown {
        uid: (uid != -1).then_some(uid as u32),
        gid: (gid != -1).then_some(gid as u32),
    }
}

#[cfg(target_arch = "x86_64")]
pub fn sys_chown(
    current: &UserTaskRef,
    path: *const c_char,
    uid: i32,
    gid: i32,
) -> StarryResult<isize> {
    sys_fchownat(current, AT_FDCWD, path, uid, gid, 0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_lchown(
    current: &UserTaskRef,
    path: *const c_char,
    uid: i32,
    gid: i32,
) -> StarryResult<isize> {
    sys_fchownat(current, AT_FDCWD, path, uid, gid, AT_SYMLINK_NOFOLLOW)
}

pub fn sys_fchown(current: &UserTaskRef, fd: i32, uid: i32, gid: i32) -> StarryResult<isize> {
    change_fd(current, fd, ownership_change(uid, gid))
}

pub fn sys_fchownat(
    current: &UserTaskRef,
    dirfd: i32,
    path: *const c_char,
    uid: i32,
    gid: i32,
    flags: u32,
) -> StarryResult<isize> {
    if flags & !(AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) != 0 {
        return Err(StarryError::InvalidInput);
    }
    change_at(current, dirfd, path, flags, ownership_change(uid, gid))
}

#[cfg(target_arch = "x86_64")]
pub fn sys_chmod(current: &UserTaskRef, path: *const c_char, mode: u32) -> StarryResult<isize> {
    sys_fchmodat(current, AT_FDCWD, path, mode, 0)
}

pub fn sys_fchmod(current: &UserTaskRef, fd: i32, mode: u32) -> StarryResult<isize> {
    change_fd(
        current,
        fd,
        MetadataChange::Chmod(NodePermission::from_bits_truncate(mode as u16)),
    )
}

pub fn sys_fchmodat(
    current: &UserTaskRef,
    dirfd: i32,
    path: *const c_char,
    mode: u32,
    flags: u32,
) -> StarryResult<isize> {
    if flags & !(AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) != 0 {
        return Err(StarryError::InvalidInput);
    }
    change_at(
        current,
        dirfd,
        path,
        flags,
        MetadataChange::Chmod(NodePermission::from_bits_truncate(mode as u16)),
    )
}

#[cfg(target_arch = "x86_64")]
#[repr(C)]
#[derive(Clone, Copy, bytemuck::AnyBitPattern)]
pub struct UtimeBuf {
    actime: linux_raw_sys::general::__kernel_old_time_t,
    modtime: linux_raw_sys::general::__kernel_old_time_t,
}

#[cfg(target_arch = "x86_64")]
pub fn sys_utime(
    current: &UserTaskRef,
    path: *const c_char,
    times: *const UtimeBuf,
) -> StarryResult<isize> {
    let (atime, mtime) = if let Some(times) = times.nullable() {
        // SAFETY: the user-memory provider initializes the complete repr(C)
        // pair of integer fields, for which every bit pattern is valid.
        let times = unsafe { times.vm_read_uninit(current)?.assume_init() };
        (
            TimeChange::Set(Duration::from_secs(times.actime as _)),
            TimeChange::Set(Duration::from_secs(times.modtime as _)),
        )
    } else {
        (TimeChange::Now, TimeChange::Now)
    };
    change_at(
        current,
        AT_FDCWD,
        path,
        0,
        MetadataChange::Times { atime, mtime },
    )
}

#[cfg(target_arch = "x86_64")]
pub fn sys_utimes(
    current: &UserTaskRef,
    path: *const c_char,
    times: *const [linux_raw_sys::general::timeval; 2],
) -> StarryResult<isize> {
    let (atime, mtime) = if let Some(times) = times.nullable() {
        // SAFETY: validated user memory initializes both repr(C) timeval
        // structures, which consist entirely of integer fields.
        let [atime, mtime] = unsafe { times.vm_read_uninit(current)?.assume_init() };
        (
            TimeChange::Set(atime.try_into_time_value()?),
            TimeChange::Set(mtime.try_into_time_value()?),
        )
    } else {
        (TimeChange::Now, TimeChange::Now)
    };
    change_at(
        current,
        AT_FDCWD,
        path,
        0,
        MetadataChange::Times { atime, mtime },
    )
}

fn time_change(time: &timespec) -> StarryResult<TimeChange> {
    match time.tv_nsec {
        value if value == UTIME_OMIT as _ => Ok(TimeChange::Keep),
        value if value == UTIME_NOW as _ => Ok(TimeChange::Now),
        _ => Ok(TimeChange::Set(time.try_into_time_value()?)),
    }
}

pub fn sys_utimensat(
    current: &UserTaskRef,
    dirfd: i32,
    path: *const c_char,
    times: *const [timespec; 2],
    flags: u32,
) -> StarryResult<isize> {
    let times = times
        .nullable()
        .map(|times| -> StarryResult<[timespec; 2]> {
            // SAFETY: validated user memory initializes the complete pair of
            // repr(C) timespec structures; all their integer patterns are valid.
            Ok(unsafe { times.vm_read_uninit(current)?.assume_init() })
        })
        .transpose()?;
    if let Some([atime, mtime]) = &times
        && atime.tv_nsec == UTIME_OMIT as _
        && mtime.tv_nsec == UTIME_OMIT as _
    {
        return Ok(0);
    }

    let descriptor = path.is_null() && dirfd != AT_FDCWD;
    if (descriptor && flags != 0) || flags & !(AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) != 0 {
        return Err(StarryError::InvalidInput);
    }
    let identity = FsCredentialsSnapshot::capture(current.as_thread());
    let credentials = mutation_credentials(&identity);
    let target = if descriptor {
        fd_target(dirfd, DescriptorMode::Ordinary)?
    } else {
        at_target(current, dirfd, path, flags, &credentials)?
    };
    // Linux validates nanoseconds in vfs_utimes, after fd/path lookup, while
    // the all-OMIT shortcut above precedes flags and even an invalid target.
    let (atime, mtime) = if let Some([atime, mtime]) = &times {
        (time_change(atime)?, time_change(mtime)?)
    } else {
        (TimeChange::Now, TimeChange::Now)
    };
    target
        .location()
        .change_metadata(MetadataChange::Times { atime, mtime }, &credentials)?;
    Ok(0)
}
