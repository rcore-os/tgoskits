use core::ffi::c_int;
#[cfg(any(feature = "fs", feature = "net"))]
use core::ffi::{CStr, c_char};

#[cfg(any(feature = "fs", feature = "multitask", feature = "net"))]
use ax_errno::{LinuxError, LinuxResult};

pub fn e(ret: c_int) -> c_int {
    if ret < 0 {
        crate::errno::set_errno(ret.abs());
        -1
    } else {
        ret as _
    }
}

#[cfg(any(feature = "fs", feature = "net"))]
pub fn char_ptr_to_str<'a>(str: *const c_char) -> LinuxResult<&'a str> {
    if str.is_null() {
        Err(LinuxError::EFAULT)
    } else {
        unsafe { CStr::from_ptr(str) }
            .to_str()
            .map_err(|_| LinuxError::EINVAL)
    }
}

#[cfg(feature = "multitask")]
pub fn check_null_mut_ptr<T>(ptr: *mut T) -> LinuxResult {
    if ptr.is_null() {
        Err(LinuxError::EFAULT)
    } else {
        Ok(())
    }
}

macro_rules! syscall_body {
    ($fn: ident, $($stmt: tt)*) => {{
        let syscall = || -> ax_errno::LinuxResult<_> { $($stmt)* };
        let result = syscall();
        match result {
            Ok(_) | Err(ax_errno::LinuxError::EAGAIN) => {
                debug!(concat!(stringify!($fn), " => {:?}"), result)
            }
            Err(_) => info!(concat!(stringify!($fn), " => {:?}"), result),
        }
        match result {
            Ok(value) => value as _,
            Err(error) => -error.code() as _,
        }
    }};
}
