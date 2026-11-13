use std::{io, ptr};

use crate::TestResult;

const FUTEX_WAIT: libc::c_long = libc::FUTEX_WAIT as libc::c_long;

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
        let (result, errno) = futex_wait(&mut word, 0, &timeout);
        if result != -1 || errno != libc::EAGAIN {
            return Err("value mismatch must return EAGAIN before validating timeout");
        }
    }

    word = 0;
    let invalid_timeout = libc::timespec {
        tv_sec: 0,
        tv_nsec: 1_000_000_000,
    };
    let (result, errno) = futex_wait(&mut word, 0, &invalid_timeout);
    if result != -1 || errno != libc::EINVAL {
        return Err("matching value with invalid timeout must return EINVAL");
    }

    Ok(())
}

fn futex_wait(
    word: &mut u32,
    expected: u32,
    timeout: &libc::timespec,
) -> (libc::c_long, libc::c_int) {
    // SAFETY: word and timeout are initialized, correctly aligned objects
    // borrowed for this synchronous call. Invalid timespec fields deliberately
    // exercise validation; neither address is retained after the failed wait.
    let result = unsafe {
        libc::syscall(
            libc::SYS_futex,
            ptr::from_mut(word).addr() as libc::c_long,
            FUTEX_WAIT,
            expected as libc::c_long,
            ptr::from_ref(timeout).addr() as libc::c_long,
            0,
            0,
        )
    };
    (
        result,
        io::Error::last_os_error()
            .raw_os_error()
            .expect("futex failure must preserve errno"),
    )
}
