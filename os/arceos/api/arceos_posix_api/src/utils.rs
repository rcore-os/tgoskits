#![allow(dead_code)]
#![allow(unused_macros)]

use core::ffi::{CStr, c_char};

use crate::{PosixError, PosixResult};

pub fn char_ptr_to_str<'a>(str: *const c_char) -> PosixResult<&'a str> {
    if str.is_null() {
        Err(PosixError::EFAULT)
    } else {
        unsafe { CStr::from_ptr(str) }
            .to_str()
            .map_err(|_| PosixError::EINVAL)
    }
}

pub fn check_null_ptr<T>(ptr: *const T) -> PosixResult {
    if ptr.is_null() {
        Err(PosixError::EFAULT)
    } else {
        Ok(())
    }
}

pub fn check_null_mut_ptr<T>(ptr: *mut T) -> PosixResult {
    if ptr.is_null() {
        Err(PosixError::EFAULT)
    } else {
        Ok(())
    }
}

macro_rules! syscall_body {
    ($fn: ident, $($stmt: tt)*) => {{
        #[allow(clippy::redundant_closure_call)]
        let res = (|| -> crate::PosixResult<_> { $($stmt)* })();
        match res {
            Ok(_) | Err(crate::PosixError::EAGAIN) => debug!(concat!(stringify!($fn), " => {:?}"),  res),
            Err(_) => info!(concat!(stringify!($fn), " => {:?}"), res),
        }
        match res {
            Ok(v) => v as _,
            Err(e) => {
                -e.errno().into_raw() as _
            }
        }
    }};
}

macro_rules! syscall_body_no_debug {
    ($($stmt: tt)*) => {{
        #[allow(clippy::redundant_closure_call)]
        let res = (|| -> crate::PosixResult<_> { $($stmt)* })();
        match res {
            Ok(v) => v as _,
            Err(e) => {
                -e.errno().into_raw() as _
            }
        }
    }};
}
