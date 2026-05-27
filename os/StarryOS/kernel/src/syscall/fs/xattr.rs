use ax_errno::{AxError, AxResult};

pub fn sys_getxattr(
    _path: *const core::ffi::c_char,
    _name: *const core::ffi::c_char,
    _value: *mut u8,
    _size: usize,
) -> AxResult<isize> {
    Err(AxError::Unsupported)
}

pub fn sys_lgetxattr(
    _path: *const core::ffi::c_char,
    _name: *const core::ffi::c_char,
    _value: *mut u8,
    _size: usize,
) -> AxResult<isize> {
    Err(AxError::Unsupported)
}

pub fn sys_fgetxattr(
    _fd: i32,
    _name: *const core::ffi::c_char,
    _value: *mut u8,
    _size: usize,
) -> AxResult<isize> {
    Err(AxError::Unsupported)
}

pub fn sys_setxattr(
    _path: *const core::ffi::c_char,
    _name: *const core::ffi::c_char,
    _value: *const u8,
    _size: usize,
    _flags: i32,
) -> AxResult<isize> {
    Err(AxError::Unsupported)
}

pub fn sys_lsetxattr(
    _path: *const core::ffi::c_char,
    _name: *const core::ffi::c_char,
    _value: *const u8,
    _size: usize,
    _flags: i32,
) -> AxResult<isize> {
    Err(AxError::Unsupported)
}

pub fn sys_fsetxattr(
    _fd: i32,
    _name: *const core::ffi::c_char,
    _value: *const u8,
    _size: usize,
    _flags: i32,
) -> AxResult<isize> {
    Err(AxError::Unsupported)
}

pub fn sys_listxattr(
    _path: *const core::ffi::c_char,
    _list: *mut u8,
    _size: usize,
) -> AxResult<isize> {
    Ok(0)
}

pub fn sys_llistxattr(
    _path: *const core::ffi::c_char,
    _list: *mut u8,
    _size: usize,
) -> AxResult<isize> {
    Ok(0)
}

pub fn sys_flistxattr(_fd: i32, _list: *mut u8, _size: usize) -> AxResult<isize> {
    Ok(0)
}

pub fn sys_removexattr(
    _path: *const core::ffi::c_char,
    _name: *const core::ffi::c_char,
) -> AxResult<isize> {
    Err(AxError::Unsupported)
}

pub fn sys_lremovexattr(
    _path: *const core::ffi::c_char,
    _name: *const core::ffi::c_char,
) -> AxResult<isize> {
    Err(AxError::Unsupported)
}

pub fn sys_fremovexattr(_fd: i32, _name: *const core::ffi::c_char) -> AxResult<isize> {
    Err(AxError::Unsupported)
}
