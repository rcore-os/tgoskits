// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Linux 7.2.3 configuration values and errno/GFP/PAGE constants used by DRM.
// These values are architecture-neutral in the supported 64-bit kernel ABIs.

#![allow(non_snake_case, non_upper_case_globals)]

pub const CONFIG_PM: bool = true;
pub const CONFIG_LOCKDEP: bool = false;
pub const CONFIG_DEBUG_MUTEXES: bool = false;
pub const CONFIG_PREEMPT_RT: bool = false;
pub const BITS_PER_LONG: u32 = 64;
pub const CONFIG_HZ: u32 = 100;
pub const HZ: u32 = CONFIG_HZ;
pub const GFP_KERNEL: u32 = 0x0cc0;
pub const GFP_ATOMIC: u32 = 0x0820;
pub const GFP_NOWAIT: u32 = 0x2800;
pub const __GFP_NOWARN: u32 = 1 << 13;
pub const __GFP_NORETRY: u32 = 1 << 16;
// Linux 7.2.3 errno, page, and scheduler constants used by the generic shims.
pub const PAGE_SIZE: usize = 4096;
pub const PAGE_SHIFT: u32 = 12;
pub type PgoffT = u64;
pub const MAX_SCHEDULE_TIMEOUT: u64 = i64::MAX as u64 >> 1;
pub const EAGAIN: i32 = 11;
pub const EWOULDBLOCK: i32 = EAGAIN;
pub const CAP_SYS_ADMIN: i32 = 21;
pub const EPERM: i32 = 1;
pub const EBADF: i32 = 9;
pub const EACCES: i32 = 13;
pub const EEXIST: i32 = 17;
pub const EBADSLT: i32 = 57;
pub const ERESTARTSYS: i32 = 512;
pub const EINTR: i32 = 4;
pub const ENOMEM: i32 = 12;
pub const ENOEXEC: i32 = 8;
pub const EFAULT: i32 = 14;
pub const EBUSY: i32 = 16;
pub const ENODEV: i32 = 19;
pub const EINVAL: i32 = 22;
pub const E2BIG: i32 = 7;
pub const EPIPE: i32 = 32;
pub const EDEADLK: i32 = 35;
pub const EALREADY: i32 = 114;
pub const EPROTO: i32 = 71;
pub const EOVERFLOW: i32 = 75;
pub const ETIME: i32 = 62;
pub const ETIMEDOUT: i32 = 110;
pub const EOPNOTSUPP: i32 = 95;
pub const EBADMSG: i32 = 74;
pub const EMSGSIZE: i32 = 90;
pub const EIO: i32 = 5;
pub const ENOENT: i32 = 2;
pub const ENXIO: i32 = 6;
pub const ERANGE: i32 = 34;
pub const ENOSPC: i32 = 28;
#[allow(non_snake_case)]
pub const fn IS_ENABLED(enabled: bool) -> bool {
    enabled
}
#[allow(non_snake_case)]
pub fn IS_ERR<T>(pointer: *const T) -> bool {
    pointer as usize >= usize::MAX - 4094
}
#[allow(non_snake_case)]
pub fn PTR_ERR<T>(pointer: *const T) -> i32 {
    pointer as isize as i32
}
/// `PTR_ERR_OR_ZERO()` from include/linux/err.h.
#[inline]
pub fn PTR_ERR_OR_ZERO<T>(pointer: *const T) -> i32 {
    if IS_ERR(pointer) { PTR_ERR(pointer) } else { 0 }
}
#[allow(non_snake_case)]
pub fn ERR_PTR<T>(error: i32) -> *mut T {
    error as isize as usize as *mut T
}
