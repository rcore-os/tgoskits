//! `epoll` smoke test.
//!
//! Drives the full `epoll` round trip that async runtimes rely on: register an
//! `eventfd` with `epoll_ctl(EPOLL_CTL_ADD)`, write to it, observe `EPOLLIN`
//! from `epoll_wait`, drain it, and remove the watch. Also covers the error
//! paths (`EINVAL` for bad creation flags / non-epoll fds, `EEXIST` for a
//! duplicate add).
//!
//! The `EpollEvent` layout matches the Linux `struct epoll_event` ABI: on
//! x86_64 the `data` field is packed to offset 4 (12-byte struct), elsewhere
//! it is naturally aligned (16-byte struct). `epoll_wait` is always called
//! with `timeout = 0` so the test never blocks in the cooperative scheduler.

use std::{os::fd::AsRawFd, println};

use libc::{EFD_NONBLOCK, EPOLL_CLOEXEC};

use super::syscalls::{self, EpollEvent, assert_errno};
const EPOLLIN: u32 = libc::EPOLLIN as u32;
const EPOLLOUT: u32 = libc::EPOLLOUT as u32;
use libc::{EEXIST, EINVAL, EPOLL_CTL_ADD, EPOLL_CTL_DEL};

/// Opaque token carried through `epoll_event.data` to prove passthrough.
const DATA_TOKEN: u64 = 0xdead_beef;

fn test_create_rejects_unknown_flags() {
    assert_errno(
        syscalls::epoll_create1(1),
        EINVAL,
        "epoll_create1 with an unknown flag",
    );
    let epfd = syscalls::epoll_create1(EPOLL_CLOEXEC).expect("epoll_create1(EPOLL_CLOEXEC) failed");
    assert!(
        epfd.as_raw_fd() >= 0,
        "epoll_create1(EPOLL_CLOEXEC) returned an invalid fd"
    );
}

fn test_eventfd_roundtrip_via_epoll() {
    let epfd = syscalls::epoll_create1(0).expect("epoll_create1(0) failed");
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("eventfd failed");

    let mut interest = EpollEvent {
        events: EPOLLIN,
        u64: DATA_TOKEN,
    };
    syscalls::epoll_ctl(&epfd, EPOLL_CTL_ADD, &fd, Some(&mut interest))
        .expect("epoll_ctl ADD failed");

    let mut ready = [EpollEvent { events: 0, u64: 0 }; 4];
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        0,
        "no event must be reported before any write"
    );

    assert_eq!(
        syscalls::write_u64(&fd, 1).unwrap(),
        8,
        "waking write to eventfd must return 8"
    );
    let n = syscalls::epoll_wait(&epfd, &mut ready, 0).expect("epoll_wait failed");
    assert_eq!(n, 1, "epoll must report the readable eventfd");
    assert_ne!(
        ready[0].events & EPOLLIN,
        0,
        "reported event must carry EPOLLIN"
    );
    assert_eq!(
        { ready[0].u64 },
        DATA_TOKEN,
        "epoll_event.data must round-trip the registered value"
    );

    assert_eq!(
        syscalls::read_u64(&fd).unwrap(),
        1,
        "draining read must return the written value"
    );
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        0,
        "no event must be reported after the counter is drained"
    );

    assert_errno(
        syscalls::epoll_ctl(&epfd, EPOLL_CTL_ADD, &fd, Some(&mut interest)),
        EEXIST,
        "duplicate epoll_ctl ADD",
    );

    syscalls::epoll_ctl(&epfd, EPOLL_CTL_DEL, &fd, None).expect("epoll_ctl DEL failed");
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        0,
        "no event must be reported after DEL"
    );
}

fn test_epoll_ctl_on_non_epoll_fd_is_einval() {
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("eventfd failed");
    let mut interest = EpollEvent {
        events: EPOLLIN,
        u64: 0,
    };
    assert_errno(
        syscalls::epoll_ctl(&fd, EPOLL_CTL_ADD, &fd, Some(&mut interest)),
        EINVAL,
        "epoll_ctl using an eventfd as the epoll instance",
    );
}

/// A saturated counter is the boundary case for writability: Linux reports
/// `EPOLLOUT` only while `count < ULLONG_MAX - 1`, i.e. while a 1-unit write
/// can still succeed. The eventfd is level-triggered, so a wrong writable
/// flag would surface immediately from `epoll_wait`.
fn test_full_counter_writability_via_epoll() {
    let epfd = syscalls::epoll_create1(0).expect("epoll_create1(0) failed");
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("eventfd failed");
    // Fill the counter to its maximum: UINT64_MAX - 1 is the largest single
    // accepted write, and any further write must now block / EAGAIN.
    assert_eq!(
        syscalls::write_u64(&fd, u64::MAX - 1).unwrap(),
        8,
        "fill write of UINT64_MAX - 1 must succeed"
    );

    let mut interest = EpollEvent {
        events: EPOLLOUT,
        u64: 0,
    };
    syscalls::epoll_ctl(&epfd, EPOLL_CTL_ADD, &fd, Some(&mut interest))
        .expect("epoll_ctl ADD EPOLLOUT failed");

    let mut ready = [EpollEvent { events: 0, u64: 0 }; 4];
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        0,
        "a saturated eventfd must not be reported writable"
    );

    // Draining the counter must make it writable again.
    assert_eq!(
        syscalls::read_u64(&fd).unwrap(),
        u64::MAX - 1,
        "saturated counter must read back"
    );
    let n = syscalls::epoll_wait(&epfd, &mut ready, 0).expect("epoll_wait failed");
    assert_eq!(n, 1, "a drained eventfd must be reported writable");
    assert_ne!(
        ready[0].events & EPOLLOUT,
        0,
        "reported event must carry EPOLLOUT"
    );
}

pub fn run() -> crate::TestResult {
    test_create_rejects_unknown_flags();
    test_eventfd_roundtrip_via_epoll();
    test_epoll_ctl_on_non_epoll_fd_is_einval();
    test_full_counter_writability_via_epoll();
    println!("io_mpx: epoll smoke test OK");
    Ok(())
}
