//! `eventfd` unit tests.
//!
//! These exercise the Linux `eventfd(2)` semantics implemented by
//! `ax-posix-api`'s `EventFd`:
//!
//! - an `eventfd` exposes a 64-bit counter as a file descriptor;
//! - a read returns the current counter and resets it to zero (or decrements
//!   by one for `EFD_SEMAPHORE`);
//! - a write adds an 8-byte value to the counter, rejecting `UINT64_MAX` with
//!   `EINVAL` and overflowing writes with `EAGAIN` (nonblocking);
//! - reads/writes with a buffer smaller than 8 bytes fail with `EINVAL`;
//! - unknown creation flags fail with `EINVAL`.
//!
//! All fds are created nonblocking where an empty-counter read is expected, so
//! no test ever blocks in the cooperative scheduler.

use core::ffi::c_int;
use std::println;

use super::syscalls::{self, assert_errno};

/// `EFD_SEMAPHORE` = 1.
const EFD_SEMAPHORE: c_int = 1;
/// `EFD_CLOEXEC` = `O_CLOEXEC` = 02000000 octal = 0x80000.
const EFD_CLOEXEC: c_int = 0o2000000;
/// `EFD_NONBLOCK` = `O_NONBLOCK` = 04000 octal = 0x800.
const EFD_NONBLOCK: c_int = 0o4000;

const EINVAL: c_int = 22;
const EAGAIN: c_int = 11;

fn test_create_and_flag_validation() {
    let fd = syscalls::eventfd(0, 0).expect("eventfd(0, 0) failed");
    assert!(fd >= 0, "eventfd(0, 0) returned an invalid fd");

    let fd = syscalls::eventfd(0, EFD_SEMAPHORE | EFD_CLOEXEC | EFD_NONBLOCK)
        .expect("eventfd with all supported flags failed");
    assert!(
        fd >= 0,
        "eventfd with all supported flags returned an invalid fd"
    );

    assert_errno(
        syscalls::eventfd(0, EFD_SEMAPHORE | 0x4000_0000),
        EINVAL,
        "eventfd with an unknown flag",
    );
}

fn test_read_empty_nonblocking_eagain() {
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("create nonblocking eventfd failed");
    assert_errno(
        syscalls::read_u64(fd),
        EAGAIN,
        "read of an empty nonblocking eventfd",
    );
}

fn test_initval_is_readable_and_drains() {
    let fd = syscalls::eventfd(5, EFD_NONBLOCK).expect("create eventfd(5) failed");
    assert_eq!(
        syscalls::read_u64(fd).unwrap(),
        5,
        "initval must be readable"
    );
    assert_errno(
        syscalls::read_u64(fd),
        EAGAIN,
        "second read after initval was drained",
    );
}

fn test_write_read_accumulate_and_reset() {
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("create eventfd failed");
    assert_eq!(
        syscalls::write_u64(fd, 3).unwrap(),
        8,
        "write must return 8"
    );
    assert_eq!(
        syscalls::write_u64(fd, 4).unwrap(),
        8,
        "write must return 8"
    );
    assert_eq!(
        syscalls::read_u64(fd).unwrap(),
        7,
        "writes must accumulate in the counter"
    );
    assert_errno(
        syscalls::read_u64(fd),
        EAGAIN,
        "read after the counter was reset to zero",
    );
}

fn test_buffer_length_validation() {
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("create eventfd failed");
    let mut small = [0u8; 4];
    assert_errno(
        syscalls::read(fd, &mut small),
        EINVAL,
        "read into a buffer smaller than 8 bytes",
    );
    assert_errno(
        syscalls::write(fd, &small),
        EINVAL,
        "write of a buffer smaller than 8 bytes",
    );
    // Linux read accepts any buffer of at least 8 bytes (it reads exactly 8),
    // so a longer read must reach the counter (EAGAIN here) rather than fail
    // with EINVAL. A longer write, however, must fail: fs/eventfd.c demands
    // `count == sizeof(ucnt)`.
    let mut long = [0u8; 16];
    assert_errno(
        syscalls::read(fd, &mut long),
        EAGAIN,
        "read of a buffer larger than 8 bytes must not be EINVAL",
    );
    assert_errno(
        syscalls::write(fd, &long),
        EINVAL,
        "write of a buffer larger than 8 bytes",
    );
}

fn test_semaphore_decrements_one_at_a_time() {
    let fd = syscalls::eventfd(2, EFD_SEMAPHORE | EFD_NONBLOCK)
        .expect("create semaphore eventfd failed");
    assert_eq!(
        syscalls::read_u64(fd).unwrap(),
        1,
        "semaphore read must return 1"
    );
    assert_eq!(
        syscalls::read_u64(fd).unwrap(),
        1,
        "semaphore read must return 1"
    );
    assert_errno(
        syscalls::read_u64(fd),
        EAGAIN,
        "read of a drained semaphore eventfd",
    );
}

fn test_write_u64_max_einval() {
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("create eventfd failed");
    assert_errno(
        syscalls::write_u64(fd, u64::MAX),
        EINVAL,
        "write of UINT64_MAX",
    );
}

fn test_counter_overflow_eagain() {
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("create eventfd failed");
    // The counter saturates at UINT64_MAX - 1, so a write of UINT64_MAX - 1
    // is the largest accepted value.
    assert_eq!(
        syscalls::write_u64(fd, u64::MAX - 1).unwrap(),
        8,
        "write of UINT64_MAX - 1 must succeed"
    );
    assert_errno(
        syscalls::write_u64(fd, 1),
        EAGAIN,
        "write that would overflow the counter",
    );
    assert_eq!(
        syscalls::read_u64(fd).unwrap(),
        u64::MAX - 1,
        "saturated counter must read back"
    );
}

pub fn run() -> crate::TestResult {
    test_create_and_flag_validation();
    test_read_empty_nonblocking_eagain();
    test_initval_is_readable_and_drains();
    test_write_read_accumulate_and_reset();
    test_buffer_length_validation();
    test_semaphore_decrements_one_at_a_time();
    test_write_u64_max_einval();
    test_counter_overflow_eagain();
    println!("io_mpx: eventfd unit tests OK");
    Ok(())
}
