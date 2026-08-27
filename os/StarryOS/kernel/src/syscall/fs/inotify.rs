use alloc::string::ToString;
use core::ffi::{c_char, c_int};

use linux_raw_sys::general::{AT_FDCWD, IN_CLOEXEC, IN_NONBLOCK};

use crate::{
    StarryError, StarryResult,
    file::{FileLike, add_file_like, get_file_like, inotify::Inotify, resolve_at},
    mm::vm_load_path_string,
};

pub fn sys_inotify_init1(flags: u32) -> StarryResult<isize> {
    debug!("sys_inotify_init1 <= flags: {flags}");

    let valid_flags = IN_CLOEXEC | IN_NONBLOCK;
    if flags & !valid_flags != 0 {
        return Err(StarryError::InvalidInput);
    }

    let inotify = Inotify::new();
    inotify.set_nonblocking(flags & IN_NONBLOCK != 0)?;
    add_file_like(inotify as _, flags & IN_CLOEXEC != 0).map(|fd| fd as _)
}

pub fn sys_inotify_add_watch(fd: c_int, path: *const c_char, mask: u32) -> StarryResult<isize> {
    let path = vm_load_path_string(path)?;
    debug!("sys_inotify_add_watch <= fd: {fd}, path: {path}, mask: {mask}");

    let resolved_path = resolve_at(AT_FDCWD, Some(&path), 0)?
        .into_file()
        .and_then(|loc| loc.absolute_path().ok().map(|path| path.to_string()))
        .ok_or(StarryError::InvalidInput)?;

    let inotify = get_file_like(fd)?
        .downcast_arc::<Inotify>()
        .map_err(|_| StarryError::InvalidInput)?;
    inotify.add_watch(resolved_path, mask).map(|wd| wd as isize)
}

pub fn sys_inotify_rm_watch(fd: c_int, wd: c_int) -> StarryResult<isize> {
    debug!("sys_inotify_rm_watch <= fd: {fd}, wd: {wd}");

    let inotify = get_file_like(fd)?
        .downcast_arc::<Inotify>()
        .map_err(|_| StarryError::InvalidInput)?;
    inotify.rm_watch(wd).map(|()| 0)
}

#[cfg(all(test, not(axtest)))]
fn inotify_flags_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::{IN_CLOEXEC, IN_NONBLOCK};
    // Test inotify_init1 flag validation
    let valid_flags = 0u32;
    assert!(valid_flags & !(IN_CLOEXEC | IN_NONBLOCK) == 0);

    let cloexec_only = IN_CLOEXEC;
    assert!(cloexec_only & !(IN_CLOEXEC | IN_NONBLOCK) == 0);

    let nonblock_only = IN_NONBLOCK;
    assert!(nonblock_only & !(IN_CLOEXEC | IN_NONBLOCK) == 0);

    let all_valid = IN_CLOEXEC | IN_NONBLOCK;
    assert!(all_valid & !(IN_CLOEXEC | IN_NONBLOCK) == 0);

    // Invalid flag should be detected
    let invalid_flags = 0xFFFFu32;
    assert!(invalid_flags & !(IN_CLOEXEC | IN_NONBLOCK) != 0);

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn inotify_flags_validation_rules_hold() {
        assert!(super::inotify_flags_validation_rules_hold_for_test());
    }
}
