use core::ffi::{c_int, c_long};

use crate::backend::sys_sysconf;

/// Return system configuration infomation
///
/// Notice: currently only support what unikraft covers
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sysconf(name: c_int) -> c_long {
    sys_sysconf(name)
}
