use core::ptr;

use crate::TestResult;

const FUTEX_WAIT: libc::c_long = 0;

pub fn run() -> TestResult {
    let mut word = 1_u32;

    for timeout in [
        libc::timespec {
            tv_sec: -1,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: 0,
            tv_nsec: 1_000_000_000,
        },
    ] {
        let (result, errno) = futex_wait(&raw mut word, 0, &raw const timeout);
        if result != -1 || errno != libc::EAGAIN {
            return Err("value mismatch must return EAGAIN before validating timeout");
        }
    }

    word = 0;
    let invalid_timeout = libc::timespec {
        tv_sec: 0,
        tv_nsec: 1_000_000_000,
    };
    let (result, errno) = futex_wait(&raw mut word, 0, &raw const invalid_timeout);
    if result != -1 || errno != libc::EINVAL {
        return Err("matching value with invalid timeout must return EINVAL");
    }

    Ok(())
}

fn futex_wait(
    word: *mut u32,
    expected: u32,
    timeout: *const libc::timespec,
) -> (libc::c_long, libc::c_int) {
    let errno = unsafe { std::os::libc_compat::__errno_location() };
    unsafe { errno.write(0) };
    let result = unsafe {
        std::os::libc_compat::syscall(
            libc::SYS_futex,
            word.addr() as libc::c_long,
            FUTEX_WAIT,
            expected as libc::c_long,
            timeout.addr() as libc::c_long,
            0,
            0,
        )
    };
    (result, unsafe { ptr::read(errno) })
}
