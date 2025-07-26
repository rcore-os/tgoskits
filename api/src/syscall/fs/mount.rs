use core::ffi::{c_char, c_void};

use axerrno::LinuxResult;
use axfs_ng::FS_CONTEXT;

use crate::{mm::UserConstPtr, vfs::MemoryFs};

pub fn sys_mount(
    source: UserConstPtr<c_char>,
    target: UserConstPtr<c_char>,
    fs_type: UserConstPtr<c_char>,
    _flags: i32,
    _data: UserConstPtr<c_void>,
) -> LinuxResult<isize> {
    let source = source.get_as_str()?;
    let target = target.get_as_str()?;
    let fs_type = fs_type.get_as_str()?;
    info!(
        "sys_mount <= source: {:?}, target: {:?}, fs_type: {:?}",
        source, target, fs_type
    );

    if fs_type != "tmpfs" {
        return Err(axerrno::LinuxError::ENODEV);
    }

    let fs = MemoryFs::new();

    let target = FS_CONTEXT.lock().resolve(target)?;
    target.mount(&fs)?;

    Ok(0)
}

pub fn sys_umount2(target: UserConstPtr<c_char>, _flags: i32) -> LinuxResult<isize> {
    let target = target.get_as_str()?;
    info!("sys_umount2 <= target: {:?}", target);
    let target = FS_CONTEXT.lock().resolve(target)?;
    target.unmount()?;
    Ok(0)
}
