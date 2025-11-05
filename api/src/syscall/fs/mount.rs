use core::ffi::{c_char, c_void};

use axerrno::{AxError, AxResult};
use axfs_ng::FS_CONTEXT;

use crate::{mm::vm_load_string, vfs::MemoryFs};

pub fn sys_mount(
    source: *const c_char,
    target: *const c_char,
    fs_type: *const c_char,
    _flags: i32,
    _data: *const c_void,
) -> AxResult<isize> {
    let source = vm_load_string(source)?;
    let target = vm_load_string(target)?;
    let fs_type = vm_load_string(fs_type)?;
    debug!("sys_mount <= source: {source:?}, target: {target:?}, fs_type: {fs_type:?}");

    if fs_type != "tmpfs" {
        return Err(AxError::NoSuchDevice);
    }

    let fs = MemoryFs::new();

    let target = FS_CONTEXT.lock().resolve(target)?;
    target.mount(&fs)?;

    Ok(0)
}

pub fn sys_umount2(target: *const c_char, _flags: i32) -> AxResult<isize> {
    let target = vm_load_string(target)?;
    debug!("sys_umount2 <= target: {target:?}");
    let target = FS_CONTEXT.lock().resolve(target)?;
    target.unmount()?;
    Ok(0)
}
