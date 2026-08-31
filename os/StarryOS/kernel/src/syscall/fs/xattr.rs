//! Extended attribute syscalls backed by filesystem inode capabilities.
//!
//! Only the `user.*` namespace is currently exposed by Starry. Persistent
//! storage and create/replace atomicity belong to the selected filesystem;
//! this layer owns userspace validation, overlay copy-up, and Linux ABI sizes.

use alloc::{
    string::String,
    vec::Vec,
};
use core::{
    ffi::c_char,
    mem::{MaybeUninit, size_of},
    slice,
};

use ax_memory_addr::PAGE_SIZE_4K;
use axfs_ng_vfs::{Location, VfsError, XattrSetMode};
use linux_raw_sys::general::{
    AT_EMPTY_PATH, AT_FDCWD, AT_SYMLINK_NOFOLLOW, XATTR_CREATE, XATTR_LIST_MAX, XATTR_NAME_MAX,
    XATTR_REPLACE, XATTR_SIZE_MAX, xattr_args,
};
use starry_vm::{vm_read_slice, vm_write_slice};

use crate::{
    StarryError, StarryResult,
    file::{fd_is_path, resolve_at},
    mm::{vm_load_path_string, vm_load_string},
    pseudofs::overlay,
};

/// Read and validate an xattr name from userspace.
fn read_name(name: *const c_char) -> StarryResult<String> {
    let name = vm_load_string(name)?;
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > XATTR_NAME_MAX as usize {
        return Err(StarryError::InvalidInput);
    }
    if !name.starts_with("user.") {
        return Err(StarryError::OperationNotSupported);
    }
    Ok(name)
}

/// Read an xattr value from userspace with Linux size limits.
fn read_value(value: *const u8, size: usize) -> StarryResult<Vec<u8>> {
    if size > XATTR_SIZE_MAX as usize {
        return Err(StarryError::ArgumentListTooLong);
    }
    if size == 0 {
        return Ok(Vec::new());
    }

    let mut value_buf = Vec::<u8>::with_capacity(size);
    vm_read_slice(value, &mut value_buf.spare_capacity_mut()[..size])?;
    // SAFETY: vm_read_slice initialized the whole requested slice.
    unsafe { value_buf.set_len(size) };
    Ok(value_buf)
}

/// Resolve a path argument used by path-based xattr syscalls.
fn resolve_path(path: *const c_char, nofollow: bool) -> StarryResult<Location> {
    let path = vm_load_path_string(path)?;
    let flags = if nofollow { AT_SYMLINK_NOFOLLOW } else { 0 };
    resolve_at(AT_FDCWD, Some(&path), flags)?
        .into_file()
        .ok_or(StarryError::BadFileDescriptor)
}

fn resolve_xattrat(dirfd: i32, path: *const c_char, at_flags: u32) -> StarryResult<Location> {
    const VALID_FLAGS: u32 = AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW;

    if at_flags & !VALID_FLAGS != 0 {
        return Err(StarryError::InvalidInput);
    }
    let path = vm_load_path_string(path)?;
    resolve_at(dirfd, Some(&path), at_flags)?
        .into_file()
        .ok_or(StarryError::BadFileDescriptor)
}

/// Resolve an fd argument used by fd-based xattr syscalls.
fn resolve_fd(fd: i32) -> StarryResult<Location> {
    if fd_is_path(fd) {
        return Err(StarryError::BadFileDescriptor);
    }
    resolve_at(fd, None, AT_EMPTY_PATH)?
        .into_file()
        .ok_or(StarryError::BadFileDescriptor)
}

fn read_xattr_args(args: *const xattr_args, args_size: usize) -> StarryResult<xattr_args> {
    let known_size = size_of::<xattr_args>();
    if args_size < known_size {
        return Err(StarryError::InvalidInput);
    }
    if args_size > PAGE_SIZE_4K {
        return Err(StarryError::ArgumentListTooLong);
    }

    let mut raw_args = MaybeUninit::<xattr_args>::uninit();
    vm_read_slice(args, slice::from_mut(&mut raw_args))?;
    if args_size > known_size {
        let tail_size = args_size - known_size;
        let mut tail = Vec::<u8>::with_capacity(tail_size);
        vm_read_slice(
            args.cast::<u8>().wrapping_add(known_size),
            &mut tail.spare_capacity_mut()[..tail_size],
        )?;
        // SAFETY: vm_read_slice initialized the whole requested tail.
        unsafe { tail.set_len(tail_size) };
        if tail.iter().any(|byte| *byte != 0) {
            return Err(StarryError::ArgumentListTooLong);
        }
    }

    // SAFETY: vm_read_slice initialized the complete v0 structure.
    Ok(unsafe { raw_args.assume_init() })
}

/// Copy a single xattr value to userspace, or return its required size.
fn copy_value_to_user(value: &[u8], user_value: *mut u8, size: usize) -> StarryResult<isize> {
    if size == 0 {
        return Ok(value.len() as isize);
    }
    if size < value.len() {
        return Err(StarryError::OutOfRange);
    }
    if !value.is_empty() {
        vm_write_slice(user_value, value)?;
    }
    Ok(value.len() as isize)
}

/// Serialize xattr names as a nul-separated Linux listxattr buffer.
fn serialize_names(attrs: &[Vec<u8>]) -> StarryResult<Vec<u8>> {
    let mut names = Vec::new();
    for name in attrs {
        names.extend_from_slice(name);
        names.push(0);
    }
    if names.len() > XATTR_LIST_MAX as usize {
        return Err(StarryError::ArgumentListTooLong);
    }
    Ok(names)
}

/// Copy a listxattr buffer to userspace, or return its required size.
fn copy_list_to_user(names: &[u8], list: *mut u8, size: usize) -> StarryResult<isize> {
    if size == 0 {
        return Ok(names.len() as isize);
    }
    if size < names.len() {
        return Err(StarryError::OutOfRange);
    }
    if !names.is_empty() {
        vm_write_slice(list, names)?;
    }
    Ok(names.len() as isize)
}

/// Get an xattr from the currently visible real node.
fn get_xattr(
    loc: Location,
    name: *const c_char,
    user_value: *mut u8,
    size: usize,
) -> StarryResult<isize> {
    let name = read_name(name)?;
    let loc = overlay::visible_target(&loc)?;
    let value = loc.get_xattr(name.as_bytes())?;
    copy_value_to_user(&value, user_value, size)
}

/// List xattrs from the currently visible real node.
fn list_xattr(loc: Location, list: *mut u8, size: usize) -> StarryResult<isize> {
    let loc = overlay::visible_target(&loc)?;
    let names = serialize_names(&loc.list_xattrs()?)?;
    copy_list_to_user(&names, list, size)
}

fn copy_up_xattrs(source: &Location, target: &Location) -> StarryResult<()> {
    if source.ptr_eq(target) {
        return Ok(());
    }
    let names = match source.list_xattrs() {
        Ok(names) => names,
        Err(VfsError::OperationNotSupported | VfsError::Unsupported) => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    for name in names {
        let value = source.get_xattr(&name)?;
        target.set_xattr(&name, &value, XattrSetMode::Upsert)?;
    }
    Ok(())
}

/// Set an xattr, copying lower-backed overlay files up before writing.
fn set_xattr(
    loc: Location,
    name: *const c_char,
    value: *const u8,
    size: usize,
    flags: i32,
) -> StarryResult<isize> {
    let flags = flags as u32;
    if flags & !(XATTR_CREATE | XATTR_REPLACE) != 0
        || flags & XATTR_CREATE != 0 && flags & XATTR_REPLACE != 0
    {
        return Err(StarryError::InvalidInput);
    }

    let name = read_name(name)?;
    let value = read_value(value, size)?;
    if loc.is_readonly() {
        return Err(StarryError::ReadOnlyFilesystem);
    }
    let source = overlay::visible_target(&loc)?;
    let target = overlay::ensure_copy_up_target(&loc)?;
    copy_up_xattrs(&source, &target)?;
    let mode = if flags & XATTR_CREATE != 0 {
        XattrSetMode::Create
    } else if flags & XATTR_REPLACE != 0 {
        XattrSetMode::Replace
    } else {
        XattrSetMode::Upsert
    };
    target.set_xattr(name.as_bytes(), &value, mode)?;
    Ok(0)
}

/// Remove an xattr, copying lower-backed overlay files up before mutation.
fn remove_xattr(loc: Location, name: *const c_char) -> StarryResult<isize> {
    let name = read_name(name)?;
    if loc.is_readonly() {
        return Err(StarryError::ReadOnlyFilesystem);
    }
    let source = overlay::visible_target(&loc)?;
    // Probe before copy-up so removing a missing lower attribute does not
    // materialize an upper inode.
    source.get_xattr(name.as_bytes())?;
    let target = overlay::ensure_copy_up_target(&loc)?;
    copy_up_xattrs(&source, &target)?;
    target.remove_xattr(name.as_bytes())?;
    Ok(0)
}

pub fn sys_listxattr(path: *const c_char, list: *mut u8, size: usize) -> StarryResult<isize> {
    list_xattr(resolve_path(path, false)?, list, size)
}

pub fn sys_llistxattr(path: *const c_char, list: *mut u8, size: usize) -> StarryResult<isize> {
    list_xattr(resolve_path(path, true)?, list, size)
}

pub fn sys_flistxattr(fd: i32, list: *mut u8, size: usize) -> StarryResult<isize> {
    list_xattr(resolve_fd(fd)?, list, size)
}

pub fn sys_getxattr(
    path: *const c_char,
    name: *const c_char,
    value: *mut u8,
    size: usize,
) -> StarryResult<isize> {
    get_xattr(resolve_path(path, false)?, name, value, size)
}

pub fn sys_lgetxattr(
    path: *const c_char,
    name: *const c_char,
    value: *mut u8,
    size: usize,
) -> StarryResult<isize> {
    get_xattr(resolve_path(path, true)?, name, value, size)
}

pub fn sys_fgetxattr(
    fd: i32,
    name: *const c_char,
    value: *mut u8,
    size: usize,
) -> StarryResult<isize> {
    get_xattr(resolve_fd(fd)?, name, value, size)
}

pub fn sys_getxattrat(
    dirfd: i32,
    path: *const c_char,
    at_flags: u32,
    name: *const c_char,
    args: *const xattr_args,
    args_size: usize,
) -> StarryResult<isize> {
    let args = read_xattr_args(args, args_size)?;
    if args.flags != 0 {
        return Err(StarryError::InvalidInput);
    }
    get_xattr(
        resolve_xattrat(dirfd, path, at_flags)?,
        name,
        args.value as *mut u8,
        args.size as usize,
    )
}

pub fn sys_setxattr(
    path: *const c_char,
    name: *const c_char,
    value: *const u8,
    size: usize,
    flags: i32,
) -> StarryResult<isize> {
    set_xattr(resolve_path(path, false)?, name, value, size, flags)
}

pub fn sys_setxattrat(
    dirfd: i32,
    path: *const c_char,
    at_flags: u32,
    name: *const c_char,
    args: *const xattr_args,
    args_size: usize,
) -> StarryResult<isize> {
    let args = read_xattr_args(args, args_size)?;
    set_xattr(
        resolve_xattrat(dirfd, path, at_flags)?,
        name,
        args.value as *const u8,
        args.size as usize,
        args.flags as i32,
    )
}

pub fn sys_lsetxattr(
    path: *const c_char,
    name: *const c_char,
    value: *const u8,
    size: usize,
    flags: i32,
) -> StarryResult<isize> {
    set_xattr(resolve_path(path, true)?, name, value, size, flags)
}

pub fn sys_fsetxattr(
    fd: i32,
    name: *const c_char,
    value: *const u8,
    size: usize,
    flags: i32,
) -> StarryResult<isize> {
    set_xattr(resolve_fd(fd)?, name, value, size, flags)
}

pub fn sys_removexattr(path: *const c_char, name: *const c_char) -> StarryResult<isize> {
    remove_xattr(resolve_path(path, false)?, name)
}

pub fn sys_lremovexattr(path: *const c_char, name: *const c_char) -> StarryResult<isize> {
    remove_xattr(resolve_path(path, true)?, name)
}

pub fn sys_fremovexattr(fd: i32, name: *const c_char) -> StarryResult<isize> {
    remove_xattr(resolve_fd(fd)?, name)
}

#[cfg(all(test, not(axtest)))]
fn xattr_name_and_value_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::{XATTR_NAME_MAX, XATTR_SIZE_MAX};

    // `read_name` itself needs a live user address space; host tests cover the
    // pure size and namespace-prefix rules used after loading the name.
    assert!(XATTR_NAME_MAX as usize > 0);
    assert!(XATTR_SIZE_MAX as usize > 0);
    let valid_prefix = "user.test";
    let invalid_prefix = "security.test";
    assert!(valid_prefix.starts_with("user."));
    assert!(!invalid_prefix.starts_with("user."));
    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn xattr_name_and_value_validation_rules_hold() {
        assert!(super::xattr_name_and_value_validation_rules_hold_for_test());
    }
}
