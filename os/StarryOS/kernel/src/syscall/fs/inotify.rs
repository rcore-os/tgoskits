use alloc::string::ToString;
use core::ffi::{c_char, c_int};

use ax_errno::{AxError, AxResult};
use linux_raw_sys::general::{AT_FDCWD, IN_CLOEXEC, IN_NONBLOCK};

use crate::{
    file::{FileLike, add_file_like, get_file_like, inotify::Inotify, resolve_at},
    mm::vm_load_path_string,
};

pub fn sys_inotify_init1(flags: u32) -> AxResult<isize> {
    debug!("sys_inotify_init1 <= flags: {flags}");

    let valid_flags = IN_CLOEXEC | IN_NONBLOCK;
    if flags & !valid_flags != 0 {
        return Err(AxError::InvalidInput);
    }

    let inotify = Inotify::new();
    inotify.set_nonblocking(flags & IN_NONBLOCK != 0)?;
    add_file_like(inotify as _, flags & IN_CLOEXEC != 0).map(|fd| fd as _)
}

pub fn sys_inotify_add_watch(fd: c_int, path: *const c_char, mask: u32) -> AxResult<isize> {
    let path = vm_load_path_string(path)?;
    debug!("sys_inotify_add_watch <= fd: {fd}, path: {path}, mask: {mask}");

    let resolved_path = resolve_at(AT_FDCWD, Some(&path), 0)?.with_operation(|view| {
        view.absolute_path()
            .map(|path| path.to_string())
            .map_err(|_| AxError::InvalidInput)
    })?;

    let inotify = get_file_like(fd)?
        .downcast_arc::<Inotify>()
        .map_err(|_| AxError::InvalidInput)?;
    inotify.add_watch(resolved_path, mask).map(|wd| wd as isize)
}

pub fn sys_inotify_rm_watch(fd: c_int, wd: c_int) -> AxResult<isize> {
    debug!("sys_inotify_rm_watch <= fd: {fd}, wd: {wd}");

    let inotify = get_file_like(fd)?
        .downcast_arc::<Inotify>()
        .map_err(|_| AxError::InvalidInput)?;
    inotify.rm_watch(wd).map(|()| 0)
}
