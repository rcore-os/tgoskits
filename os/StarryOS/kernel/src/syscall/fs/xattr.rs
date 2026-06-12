use alloc::{
    collections::{BTreeMap, btree_map::Entry},
    string::String,
    sync::Arc,
    vec::Vec,
};
use core::ffi::c_char;

use ax_errno::{AxError, AxResult, LinuxError};
use ax_sync::Mutex;
use axfs_ng_vfs::Location;
use linux_raw_sys::general::{
    AT_EMPTY_PATH, AT_FDCWD, AT_SYMLINK_NOFOLLOW, XATTR_CREATE, XATTR_LIST_MAX, XATTR_NAME_MAX,
    XATTR_REPLACE, XATTR_SIZE_MAX,
};
use starry_vm::{vm_read_slice, vm_write_slice};

use crate::{
    file::{fd_is_path, resolve_at},
    mm::vm_load_string,
    pseudofs::overlay,
};

type XattrMap = BTreeMap<String, Vec<u8>>;

#[derive(Default)]
struct XattrStore {
    attrs: Mutex<XattrMap>,
}

fn linux_errno(errno: LinuxError) -> AxError {
    AxError::from(errno)
}

fn existing_store(loc: &Location) -> Option<Arc<XattrStore>> {
    loc.user_data().get::<XattrStore>()
}

fn store_for_update(loc: &Location) -> Arc<XattrStore> {
    loc.user_data().get_or_insert_with(XattrStore::default)
}

fn existing_attrs(loc: &Location) -> Option<XattrMap> {
    existing_store(loc).map(|store| store.attrs.lock().clone())
}

fn read_name(name: *const c_char) -> AxResult<String> {
    let name = vm_load_string(name)?;
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > XATTR_NAME_MAX as usize {
        return Err(AxError::InvalidInput);
    }
    if !name.starts_with("user.") {
        return Err(AxError::OperationNotSupported);
    }
    Ok(name)
}

fn read_value(value: *const u8, size: usize) -> AxResult<Vec<u8>> {
    if size > XATTR_SIZE_MAX as usize {
        return Err(AxError::ArgumentListTooLong);
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

fn resolve_path(path: *const c_char, nofollow: bool) -> AxResult<Location> {
    let path = vm_load_string(path)?;
    let flags = if nofollow { AT_SYMLINK_NOFOLLOW } else { 0 };
    resolve_at(AT_FDCWD, Some(&path), flags)?
        .into_file()
        .ok_or(AxError::BadFileDescriptor)
}

fn resolve_fd(fd: i32) -> AxResult<Location> {
    if fd_is_path(fd) {
        return Err(AxError::BadFileDescriptor);
    }
    resolve_at(fd, None, AT_EMPTY_PATH)?
        .into_file()
        .ok_or(AxError::BadFileDescriptor)
}

fn copy_value_to_user(value: &[u8], user_value: *mut u8, size: usize) -> AxResult<isize> {
    if size == 0 {
        return Ok(value.len() as isize);
    }
    if size < value.len() {
        return Err(AxError::OutOfRange);
    }
    if !value.is_empty() {
        vm_write_slice(user_value, value)?;
    }
    Ok(value.len() as isize)
}

fn serialize_names(attrs: Option<&XattrMap>) -> AxResult<Vec<u8>> {
    let mut names = Vec::new();
    if let Some(attrs) = attrs {
        for name in attrs.keys() {
            names.extend_from_slice(name.as_bytes());
            names.push(0);
        }
    }
    if names.len() > XATTR_LIST_MAX as usize {
        return Err(AxError::ArgumentListTooLong);
    }
    Ok(names)
}

fn copy_list_to_user(names: &[u8], list: *mut u8, size: usize) -> AxResult<isize> {
    if size == 0 {
        return Ok(names.len() as isize);
    }
    if size < names.len() {
        return Err(AxError::OutOfRange);
    }
    if !names.is_empty() {
        vm_write_slice(list, names)?;
    }
    Ok(names.len() as isize)
}

fn get_xattr(
    loc: Location,
    name: *const c_char,
    user_value: *mut u8,
    size: usize,
) -> AxResult<isize> {
    let name = read_name(name)?;
    let loc = overlay::visible_target(&loc)?;
    let value = {
        let store = existing_store(&loc).ok_or_else(|| linux_errno(LinuxError::ENODATA))?;
        store
            .attrs
            .lock()
            .get(&name)
            .cloned()
            .ok_or_else(|| linux_errno(LinuxError::ENODATA))?
    };
    copy_value_to_user(&value, user_value, size)
}

fn list_xattr(loc: Location, list: *mut u8, size: usize) -> AxResult<isize> {
    let loc = overlay::visible_target(&loc)?;
    let names = {
        let Some(store) = existing_store(&loc) else {
            return copy_list_to_user(&[], list, size);
        };
        serialize_names(Some(&store.attrs.lock()))?
    };
    copy_list_to_user(&names, list, size)
}

fn set_xattr(
    loc: Location,
    name: *const c_char,
    value: *const u8,
    size: usize,
    flags: i32,
) -> AxResult<isize> {
    let flags = flags as u32;
    if flags & !(XATTR_CREATE | XATTR_REPLACE) != 0
        || flags & XATTR_CREATE != 0 && flags & XATTR_REPLACE != 0
    {
        return Err(AxError::InvalidInput);
    }

    let name = read_name(name)?;
    let value = read_value(value, size)?;
    let old_attrs = existing_attrs(&overlay::visible_target(&loc)?);

    if let Some(attrs) = &old_attrs {
        let exists = attrs.contains_key(&name);
        if exists && flags & XATTR_CREATE != 0 {
            return Err(AxError::AlreadyExists);
        }
        if !exists && flags & XATTR_REPLACE != 0 {
            return Err(linux_errno(LinuxError::ENODATA));
        }
    } else if flags & XATTR_REPLACE != 0 {
        return Err(linux_errno(LinuxError::ENODATA));
    }

    let loc = overlay::ensure_copy_up_target(&loc)?;
    let store = store_for_update(&loc);
    let mut attrs = store.attrs.lock();
    if attrs.is_empty()
        && let Some(old_attrs) = old_attrs
    {
        *attrs = old_attrs;
    }
    match attrs.entry(name) {
        Entry::Occupied(mut entry) => {
            if flags & XATTR_CREATE != 0 {
                return Err(AxError::AlreadyExists);
            }
            entry.insert(value);
        }
        Entry::Vacant(entry) => {
            if flags & XATTR_REPLACE != 0 {
                return Err(linux_errno(LinuxError::ENODATA));
            }
            entry.insert(value);
        }
    }
    Ok(0)
}

fn remove_xattr(loc: Location, name: *const c_char) -> AxResult<isize> {
    let name = read_name(name)?;
    let old_attrs = existing_attrs(&overlay::visible_target(&loc)?)
        .ok_or_else(|| linux_errno(LinuxError::ENODATA))?;
    if !old_attrs.contains_key(&name) {
        return Err(linux_errno(LinuxError::ENODATA));
    }

    let loc = overlay::ensure_copy_up_target(&loc)?;
    let store = store_for_update(&loc);
    let mut attrs = store.attrs.lock();
    if attrs.is_empty() {
        *attrs = old_attrs;
    }
    attrs.remove(&name);
    Ok(0)
}

pub fn sys_listxattr(path: *const c_char, list: *mut u8, size: usize) -> AxResult<isize> {
    list_xattr(resolve_path(path, false)?, list, size)
}

pub fn sys_llistxattr(path: *const c_char, list: *mut u8, size: usize) -> AxResult<isize> {
    list_xattr(resolve_path(path, true)?, list, size)
}

pub fn sys_flistxattr(fd: i32, list: *mut u8, size: usize) -> AxResult<isize> {
    list_xattr(resolve_fd(fd)?, list, size)
}

pub fn sys_getxattr(
    path: *const c_char,
    name: *const c_char,
    value: *mut u8,
    size: usize,
) -> AxResult<isize> {
    get_xattr(resolve_path(path, false)?, name, value, size)
}

pub fn sys_lgetxattr(
    path: *const c_char,
    name: *const c_char,
    value: *mut u8,
    size: usize,
) -> AxResult<isize> {
    get_xattr(resolve_path(path, true)?, name, value, size)
}

pub fn sys_fgetxattr(fd: i32, name: *const c_char, value: *mut u8, size: usize) -> AxResult<isize> {
    get_xattr(resolve_fd(fd)?, name, value, size)
}

pub fn sys_setxattr(
    path: *const c_char,
    name: *const c_char,
    value: *const u8,
    size: usize,
    flags: i32,
) -> AxResult<isize> {
    set_xattr(resolve_path(path, false)?, name, value, size, flags)
}

pub fn sys_lsetxattr(
    path: *const c_char,
    name: *const c_char,
    value: *const u8,
    size: usize,
    flags: i32,
) -> AxResult<isize> {
    set_xattr(resolve_path(path, true)?, name, value, size, flags)
}

pub fn sys_fsetxattr(
    fd: i32,
    name: *const c_char,
    value: *const u8,
    size: usize,
    flags: i32,
) -> AxResult<isize> {
    set_xattr(resolve_fd(fd)?, name, value, size, flags)
}

pub fn sys_removexattr(path: *const c_char, name: *const c_char) -> AxResult<isize> {
    remove_xattr(resolve_path(path, false)?, name)
}

pub fn sys_lremovexattr(path: *const c_char, name: *const c_char) -> AxResult<isize> {
    remove_xattr(resolve_path(path, true)?, name)
}

pub fn sys_fremovexattr(fd: i32, name: *const c_char) -> AxResult<isize> {
    remove_xattr(resolve_fd(fd)?, name)
}
