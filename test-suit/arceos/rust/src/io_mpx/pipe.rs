//! `pipe` + `epoll` unit tests.
//!
//! These cover the write-end readiness contract that an edge-triggered
//! `EPOLLOUT` watcher relies on:
//!
//! - the write end is writable while the ring buffer has free space and stops
//!   being writable when it is full;
//! - the `Full -> Normal` transition must surface as an `EPOLLOUT` edge even
//!   when it happens between two `epoll_wait` calls, so that no wait ever
//!   samples the unwritable state.
//!
//! A write that overflows the pipe blocks until a reader drains it, so the
//! fill loop probes writability with a level-triggered `EPOLLOUT` watch on a
//! throwaway epoll instance instead of assuming a pipe capacity.

use core::ffi::c_int;
use std::println;

use super::syscalls::{self, EpollEvent};

/// `EPOLLOUT` = 0x004.
const EPOLLOUT: u32 = 0x004;
/// `EPOLLET` = `1 << 31`, requesting edge-triggered delivery.
const EPOLLET: u32 = 1 << 31;
/// `EPOLL_CTL_ADD` = 1.
const EPOLL_CTL_ADD: c_int = 1;
/// `EPOLL_CTL_DEL` = 2.
const EPOLL_CTL_DEL: c_int = 2;

/// Upper bound for the fill loop: the probe stops as soon as the pipe is full.
const PIPE_FILL_LIMIT: usize = 4096;

fn test_write_end_is_writable_until_full() {
    // This test only exercises the write end; the read end is never drained.
    let (_read_fd, write_fd) = syscalls::pipe().expect("pipe() failed");

    let epfd = syscalls::epoll_create1(0).expect("epoll_create1(0) failed");
    let mut interest = EpollEvent {
        events: EPOLLOUT,
        data: 0,
    };
    syscalls::epoll_ctl(&epfd, EPOLL_CTL_ADD, &write_fd, Some(&mut interest))
        .expect("epoll_ctl ADD failed");

    // Level-triggered: the write end must stay reportable while space remains.
    let mut ready = [EpollEvent::default(); 4];
    for i in 0..PIPE_FILL_LIMIT {
        let n = syscalls::epoll_wait(&epfd, &mut ready, 0).expect("epoll_wait failed");
        if n == 0 {
            // The pipe is full after exactly the writes issued so far.
            syscalls::epoll_ctl(&epfd, EPOLL_CTL_DEL, &write_fd, None)
                .expect("epoll_ctl DEL failed");
            println!("pipe filled after {i} bytes");
            return;
        }
        assert_eq!(
            syscalls::write(&write_fd, b"x").expect("write to pipe failed"),
            1,
            "each fill write must enqueue one byte"
        );
    }
    syscalls::epoll_ctl(&epfd, EPOLL_CTL_DEL, &write_fd, None).expect("epoll_ctl DEL failed");
    panic!("pipe still writable after {PIPE_FILL_LIMIT} bytes");
}

/// The `Full -> Normal` writability transition must be delivered even when it
/// happens between two waits.
///
/// The initial writable edge is consumed first, then the pipe is filled and one
/// byte is drained before the next `epoll_wait`: both surrounding samples are
/// writable, so a delivery rule that only compares the previously sampled
/// writability drops the edge and leaves a writer waiting for a wake that
/// never comes.
fn test_writable_edge_between_waits_is_reported() {
    let epfd = syscalls::epoll_create1(0).expect("epoll_create1(0) failed");
    let (read_fd, write_fd) = syscalls::pipe().expect("pipe() failed");

    let mut interest = EpollEvent {
        events: EPOLLOUT | EPOLLET,
        data: 0,
    };
    syscalls::epoll_ctl(&epfd, EPOLL_CTL_ADD, &write_fd, Some(&mut interest))
        .expect("epoll_ctl ADD failed");

    // The write end starts writable, so the initial edge is reported once.
    let mut ready = [EpollEvent::default(); 4];
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        1,
        "the initial writable edge must be reported"
    );
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        0,
        "the initial writable edge must be reported once"
    );

    // Fill the pipe, sampling writability only through the throwaway level
    // probe: the watched instance must not observe the unwritable state.
    let probe = syscalls::epoll_create1(0).expect("epoll_create1(0) failed");
    let mut probe_interest = EpollEvent {
        events: EPOLLOUT,
        data: 0,
    };
    syscalls::epoll_ctl(&probe, EPOLL_CTL_ADD, &write_fd, Some(&mut probe_interest))
        .expect("epoll_ctl ADD on probe failed");
    for _ in 0..PIPE_FILL_LIMIT {
        if syscalls::epoll_wait(&probe, &mut ready, 0).unwrap() == 0 {
            break;
        }
        assert_eq!(
            syscalls::write(&write_fd, b"x").expect("write to pipe failed"),
            1,
            "each fill write must enqueue one byte"
        );
    }
    syscalls::epoll_ctl(&probe, EPOLL_CTL_DEL, &write_fd, None).expect("epoll_ctl DEL failed");

    // Full -> Normal with no wait in between: the writable edge must survive.
    let mut buf = [0u8; 1];
    assert_eq!(
        syscalls::read(&read_fd, &mut buf).expect("read from pipe failed"),
        1,
        "the drain must free exactly one byte"
    );
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        1,
        "a writable edge between two waits must not be dropped"
    );
    assert_eq!(
        ready[0].events & EPOLLOUT,
        EPOLLOUT,
        "the event must carry EPOLLOUT"
    );
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        0,
        "the writable edge must be reported once"
    );
}

pub fn run() -> crate::TestResult {
    test_write_end_is_writable_until_full();
    test_writable_edge_between_waits_is_reported();
    println!("io_mpx: pipe unit tests OK");
    Ok(())
}
