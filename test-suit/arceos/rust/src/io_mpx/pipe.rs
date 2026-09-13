//! `pipe` + `epoll` ABI tests.
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

use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    println,
};

use super::syscalls::{self, EpollEvent};

const EPOLLOUT: u32 = libc::EPOLLOUT as u32;
const EPOLLET: u32 = libc::EPOLLET as u32;
use libc::{EPOLL_CTL_ADD, EPOLL_CTL_DEL};

/// Upper bound for the fill loop: the probe stops as soon as the pipe is full.
const PIPE_FILL_LIMIT: usize = 4096;

fn test_std_pipe_descriptor_flags_and_io() {
    let (mut reader, mut writer) = io::pipe().expect("std pipe creation failed");
    for fd in [reader.as_raw_fd(), writer.as_raw_fd()] {
        // SAFETY: each descriptor is owned by a live pipe endpoint; F_GETFD
        // only reads descriptor metadata and takes no pointer argument.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_eq!(
            flags,
            libc::FD_CLOEXEC,
            "std pipe must set CLOEXEC on both ends"
        );
    }
    writer.write_all(b"std pipe through libc").unwrap();
    drop(writer);
    let mut message = String::new();
    reader.read_to_string(&mut message).unwrap();
    assert_eq!(message, "std pipe through libc");
}

fn test_pipe2_rejects_flags_without_creating_descriptors() {
    let mut fds = [-1; 2];
    // SAFETY: fds has room for the two output descriptors. The deliberately
    // invalid flag must fail before publishing either descriptor.
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), -1) }, -1);
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::EINVAL)
    );
    assert_eq!(fds, [-1; 2]);
}

fn test_descriptor_flags_are_not_shared_by_duplicates() {
    let (reader, _writer) = io::pipe().unwrap();
    let reader = File::from(OwnedFd::from(reader));
    let duplicate = reader.try_clone().unwrap();
    // SAFETY: both descriptors remain owned by their File objects. These
    // fcntl commands only inspect or change per-descriptor scalar flags.
    unsafe {
        assert_eq!(
            libc::fcntl(duplicate.as_raw_fd(), libc::F_GETFD),
            libc::FD_CLOEXEC
        );
        assert_eq!(libc::fcntl(reader.as_raw_fd(), libc::F_SETFD, 0), 0);
        assert_eq!(libc::fcntl(reader.as_raw_fd(), libc::F_GETFD), 0);
        assert_eq!(
            libc::fcntl(duplicate.as_raw_fd(), libc::F_GETFD),
            libc::FD_CLOEXEC
        );
    }
    // SAFETY: F_DUPFD creates a new descriptor at or above the requested
    // minimum. A successful return transfers its sole ownership to File.
    let fd = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_DUPFD, 64) };
    assert!(fd >= 64);
    // SAFETY: fd is the freshly created descriptor checked above.
    let high = unsafe { File::from_raw_fd(fd) };
    // SAFETY: high owns a live descriptor and F_GETFD has no pointer arguments.
    assert_eq!(unsafe { libc::fcntl(high.as_raw_fd(), libc::F_GETFD) }, 0);
}

fn test_pipe2_rolls_back_when_only_one_fd_is_free() {
    let mut occupied = Vec::new();
    loop {
        match syscalls::eventfd(0, 0) {
            Ok(fd) => occupied.push(fd),
            Err(errno) => {
                assert_eq!(errno, libc::EMFILE);
                break;
            }
        }
        assert!(
            occupied.len() < 4096,
            "fd exhaustion probe exceeded its bound"
        );
    }
    let freed = occupied
        .pop()
        .expect("the test must have allocated a descriptor");
    let slot = freed.as_raw_fd();
    drop(freed);
    let mut fds = [-1; 2];
    // SAFETY: fds provides two writable output slots; a single available
    // table slot must cause failure without publishing or leaking either fd.
    assert_eq!(
        unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::EMFILE)
    );
    assert_eq!(fds, [-1; 2]);
    let recovered = syscalls::eventfd(0, 0).expect("failed pipe must return its reserved slot");
    assert_eq!(recovered.as_raw_fd(), slot);
}

fn test_write_end_is_writable_until_full() {
    // This test only exercises the write end; the read end is never drained.
    let (_read_fd, write_fd) = syscalls::pipe().expect("pipe() failed");

    let epfd = syscalls::epoll_create1(0).expect("epoll_create1(0) failed");
    let mut interest = EpollEvent {
        events: EPOLLOUT,
        u64: 0,
    };
    syscalls::epoll_ctl(&epfd, EPOLL_CTL_ADD, &write_fd, Some(&mut interest))
        .expect("epoll_ctl ADD failed");

    // Level-triggered: the write end must stay reportable while space remains.
    let mut ready = [EpollEvent { events: 0, u64: 0 }; 4];
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
        u64: 0,
    };
    syscalls::epoll_ctl(&epfd, EPOLL_CTL_ADD, &write_fd, Some(&mut interest))
        .expect("epoll_ctl ADD failed");

    // The write end starts writable, so the initial edge is reported once.
    let mut ready = [EpollEvent { events: 0, u64: 0 }; 4];
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
        u64: 0,
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
    test_std_pipe_descriptor_flags_and_io();
    test_pipe2_rejects_flags_without_creating_descriptors();
    test_descriptor_flags_are_not_shared_by_duplicates();
    test_pipe2_rolls_back_when_only_one_fd_is_free();
    test_write_end_is_writable_until_full();
    test_writable_edge_between_waits_is_reported();
    println!("io_mpx: pipe ABI tests OK");
    Ok(())
}
