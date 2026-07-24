//! `timerfd_*` syscall handlers.  See `kernel/src/file/timerfd.rs` for the
//! backing object.

use core::time::Duration;

use ax_errno::{AxError, AxResult};
use linux_raw_sys::general::{__kernel_itimerspec, __kernel_timespec, O_CLOEXEC, O_NONBLOCK};
use starry_vm::{VmMutPtr, VmPtr};

use crate::file::{
    FileLike, add_file_like,
    timerfd::{TFD_TIMER_ABSTIME, TFD_TIMER_CANCEL_ON_SET, Timerfd},
};

// linux-raw-sys does not export these under their `TFD_*` names, so alias.
const TFD_CLOEXEC: u32 = O_CLOEXEC;
const TFD_NONBLOCK: u32 = O_NONBLOCK;

fn timespec_to_duration(ts: &__kernel_timespec) -> AxResult<Duration> {
    if ts.tv_sec < 0 || !(0..1_000_000_000).contains(&ts.tv_nsec) {
        return Err(AxError::InvalidInput);
    }
    Ok(Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32))
}

fn duration_to_timespec(d: Duration) -> __kernel_timespec {
    __kernel_timespec {
        tv_sec: d.as_secs() as i64,
        tv_nsec: d.subsec_nanos() as i64,
    }
}

/// `timerfd_create(clockid, flags)`.
pub fn sys_timerfd_create(clockid: i32, flags: i32) -> AxResult<isize> {
    if clockid < 0 {
        return Err(AxError::InvalidInput);
    }
    let flags = flags as u32;
    if flags & !(TFD_CLOEXEC | TFD_NONBLOCK) != 0 {
        return Err(AxError::InvalidInput);
    }

    let tfd = Timerfd::new(clockid as u32)?;
    if flags & TFD_NONBLOCK != 0 {
        tfd.set_nonblocking(true)?;
    }
    let cloexec = flags & TFD_CLOEXEC != 0;
    add_file_like(tfd, cloexec).map(|fd| fd as _)
}

/// `timerfd_settime(fd, flags, new, old)`.
///
/// `new` and `old` are user pointers to `struct itimerspec`.  `old` may be
/// NULL to skip reporting the previous state.
pub fn sys_timerfd_settime(
    fd: i32,
    flags: i32,
    new_value: *const __kernel_itimerspec,
    old_value: *mut __kernel_itimerspec,
) -> AxResult<isize> {
    let flags = flags as u32;
    if flags & !(TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET) != 0 {
        return Err(AxError::InvalidInput);
    }

    let tfd = Timerfd::from_fd(fd)?;

    // SAFETY: `vm_read_uninit` on `Ok(..)` has copied a full
    // `__kernel_itimerspec` from validated user memory into the
    // `MaybeUninit`. `__kernel_itimerspec` is `timespec { i64, i64 }`
    // × 2 — every bit pattern is a valid inhabitant, so `assume_init`
    // is sound regardless of what the user wrote. Range-check happens
    // afterward in `timespec_to_duration`.
    let new = unsafe { new_value.vm_read_uninit()?.assume_init() };
    let new_ival = timespec_to_duration(&new.it_interval)?;
    let new_val = timespec_to_duration(&new.it_value)?;

    let abstime = flags & TFD_TIMER_ABSTIME != 0;
    let (old_ival, old_rem) = tfd.settime(abstime, new_val, new_ival)?;

    if let Some(old_ptr) = old_value.nullable() {
        let old = __kernel_itimerspec {
            it_interval: duration_to_timespec(old_ival),
            it_value: duration_to_timespec(old_rem),
        };
        old_ptr.vm_write(old)?;
    }
    Ok(0)
}

/// `timerfd_gettime(fd, curr)`.
pub fn sys_timerfd_gettime(fd: i32, curr_value: *mut __kernel_itimerspec) -> AxResult<isize> {
    let tfd = Timerfd::from_fd(fd)?;
    let (ival, rem) = tfd.gettime();
    let out = __kernel_itimerspec {
        it_interval: duration_to_timespec(ival),
        it_value: duration_to_timespec(rem),
    };
    curr_value.vm_write(out)?;
    Ok(0)
}

#[cfg(axtest)]
pub(crate) fn timerfd_timespec_conversion_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::__kernel_timespec;
    // Test timespec_to_duration validation
    let valid_ts = __kernel_timespec {
        tv_sec: 1,
        tv_nsec: 500_000_000,
    };
    assert!(timespec_to_duration(&valid_ts).is_ok());

    let zero_ts = __kernel_timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert!(timespec_to_duration(&zero_ts).is_ok());

    // Negative tv_sec should fail
    let neg_sec = __kernel_timespec {
        tv_sec: -1,
        tv_nsec: 0,
    };
    assert!(timespec_to_duration(&neg_sec).is_err());

    // tv_nsec out of range should fail
    let bad_nsec = __kernel_timespec {
        tv_sec: 0,
        tv_nsec: 1_000_000_000,
    };
    assert!(timespec_to_duration(&bad_nsec).is_err());

    let neg_nsec = __kernel_timespec {
        tv_sec: 0,
        tv_nsec: -1,
    };
    assert!(timespec_to_duration(&neg_nsec).is_err());

    // Test duration_to_timespec roundtrip
    let d = core::time::Duration::new(5, 123_456_789);
    let ts = duration_to_timespec(d);
    assert!(ts.tv_sec == 5);
    assert!(ts.tv_nsec == 123_456_789);

    true
}
