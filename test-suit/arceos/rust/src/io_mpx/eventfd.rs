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
//! - every accepted write is a readiness edge for an edge-triggered `epoll`
//!   watch, not only the write that first makes the counter non-zero (see
//!   `test_every_write_is_a_readiness_edge`);
//! - reads/writes with a buffer smaller than 8 bytes fail with `EINVAL`;
//! - unknown creation flags fail with `EINVAL`.
//!
//! All fds are created nonblocking where an empty-counter read is expected, so
//! no test ever blocks in the cooperative scheduler.

use core::ffi::c_int;
use std::println;

use super::syscalls::{self, EpollEvent, assert_errno};

/// `EFD_SEMAPHORE` = 1.
const EFD_SEMAPHORE: c_int = 1;
/// `EFD_CLOEXEC` = `O_CLOEXEC` = 02000000 octal = 0x80000.
const EFD_CLOEXEC: c_int = 0o2000000;
/// `EFD_NONBLOCK` = `O_NONBLOCK` = 04000 octal = 0x800.
const EFD_NONBLOCK: c_int = 0o4000;

const EINVAL: c_int = 22;
const EAGAIN: c_int = 11;

/// `EPOLLIN` = 0x001.
const EPOLLIN: u32 = 0x001;
/// `EPOLLOUT` = 0x004.
const EPOLLOUT: u32 = 0x004;
/// `EPOLLET` = `1 << 31`, requesting edge-triggered delivery.
const EPOLLET: u32 = 1 << 31;
/// `EPOLL_CTL_ADD` = 1.
const EPOLL_CTL_ADD: c_int = 1;

fn test_create_and_flag_validation() {
    let fd = syscalls::eventfd(0, 0).expect("eventfd(0, 0) failed");
    assert!(fd.as_raw() >= 0, "eventfd(0, 0) returned an invalid fd");

    let fd = syscalls::eventfd(0, EFD_SEMAPHORE | EFD_CLOEXEC | EFD_NONBLOCK)
        .expect("eventfd with all supported flags failed");
    assert!(
        fd.as_raw() >= 0,
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
        syscalls::read_u64(&fd),
        EAGAIN,
        "read of an empty nonblocking eventfd",
    );
}

fn test_initval_is_readable_and_drains() {
    let fd = syscalls::eventfd(5, EFD_NONBLOCK).expect("create eventfd(5) failed");
    assert_eq!(
        syscalls::read_u64(&fd).unwrap(),
        5,
        "initval must be readable"
    );
    assert_errno(
        syscalls::read_u64(&fd),
        EAGAIN,
        "second read after initval was drained",
    );
}

fn test_write_read_accumulate_and_reset() {
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("create eventfd failed");
    assert_eq!(
        syscalls::write_u64(&fd, 3).unwrap(),
        8,
        "write must return 8"
    );
    assert_eq!(
        syscalls::write_u64(&fd, 4).unwrap(),
        8,
        "write must return 8"
    );
    assert_eq!(
        syscalls::read_u64(&fd).unwrap(),
        7,
        "writes must accumulate in the counter"
    );
    assert_errno(
        syscalls::read_u64(&fd),
        EAGAIN,
        "read after the counter was reset to zero",
    );
}

fn test_buffer_length_validation() {
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("create eventfd failed");
    let mut small = [0u8; 4];
    assert_errno(
        syscalls::read(&fd, &mut small),
        EINVAL,
        "read into a buffer smaller than 8 bytes",
    );
    assert_errno(
        syscalls::write(&fd, &small),
        EINVAL,
        "write of a buffer smaller than 8 bytes",
    );
    // Linux read accepts any buffer of at least 8 bytes (it reads exactly 8),
    // so a longer read must reach the counter (EAGAIN here) rather than fail
    // with EINVAL. A longer write, however, must fail: fs/eventfd.c demands
    // `count == sizeof(ucnt)`.
    let mut long = [0u8; 16];
    assert_errno(
        syscalls::read(&fd, &mut long),
        EAGAIN,
        "read of a buffer larger than 8 bytes must not be EINVAL",
    );
    assert_errno(
        syscalls::write(&fd, &long),
        EINVAL,
        "write of a buffer larger than 8 bytes",
    );
}

fn test_semaphore_decrements_one_at_a_time() {
    let fd = syscalls::eventfd(2, EFD_SEMAPHORE | EFD_NONBLOCK)
        .expect("create semaphore eventfd failed");
    assert_eq!(
        syscalls::read_u64(&fd).unwrap(),
        1,
        "semaphore read must return 1"
    );
    assert_eq!(
        syscalls::read_u64(&fd).unwrap(),
        1,
        "semaphore read must return 1"
    );
    assert_errno(
        syscalls::read_u64(&fd),
        EAGAIN,
        "read of a drained semaphore eventfd",
    );
}

fn test_write_u64_max_einval() {
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("create eventfd failed");
    assert_errno(
        syscalls::write_u64(&fd, u64::MAX),
        EINVAL,
        "write of UINT64_MAX",
    );
}

fn test_counter_overflow_eagain() {
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("create eventfd failed");
    // The counter saturates at UINT64_MAX - 1, so a write of UINT64_MAX - 1
    // is the largest accepted value.
    assert_eq!(
        syscalls::write_u64(&fd, u64::MAX - 1).unwrap(),
        8,
        "write of UINT64_MAX - 1 must succeed"
    );
    assert_errno(
        syscalls::write_u64(&fd, 1),
        EAGAIN,
        "write that would overflow the counter",
    );
    assert_eq!(
        syscalls::read_u64(&fd).unwrap(),
        u64::MAX - 1,
        "saturated counter must read back"
    );
}

/// Every accepted write is a readiness edge, not only the one that makes the
/// counter non-zero.
///
/// Linux `eventfd_write` calls `wake_up_locked_poll(&ctx->wqh, EPOLLIN)` on
/// every write (`fs/eventfd.c`), so a poller must observe one readiness edge
/// per write. This is how an async runtime uses its `mio::Waker` eventfd: the
/// runtime never drains the counter, so an implementation that bumps readiness
/// only when readability flips delivers the first wake and drops every later
/// one, hanging the second and all subsequent `spawn_blocking` calls behind an
/// `epoll_wait` that never returns.
///
/// `epoll_wait` is called with `timeout = 0`, so a lost edge shows up as a
/// deterministic "zero events" result instead of blocking the cooperative
/// scheduler.
fn test_every_write_is_a_readiness_edge() {
    let epfd = syscalls::epoll_create1(0).expect("epoll_create1(0) failed");
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("create nonblocking eventfd failed");

    let mut interest = EpollEvent {
        events: EPOLLIN | EPOLLET,
        data: 0,
    };
    syscalls::epoll_ctl(&epfd, EPOLL_CTL_ADD, &fd, Some(&mut interest))
        .expect("epoll_ctl ADD failed");

    let mut ready = [EpollEvent::default(); 4];

    // First write: the counter goes 0 -> 1, so the fd becomes readable. This
    // edge is reported whether or not readiness tracks the readability flip.
    assert_eq!(
        syscalls::write_u64(&fd, 1).unwrap(),
        8,
        "write must return 8"
    );
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        1,
        "the first write must surface EPOLLIN"
    );
    assert_eq!(
        ready[0].events & EPOLLIN,
        EPOLLIN,
        "the first event must carry EPOLLIN"
    );

    // Second write with the counter left undrained: it merely grows 1 -> 2, so
    // readability never flips. Linux still wakes the poller, so the event has
    // to be reported again.
    assert_eq!(
        syscalls::write_u64(&fd, 1).unwrap(),
        8,
        "write must return 8"
    );
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        1,
        "every write must be a readiness edge, not only the one that flips readability"
    );
    assert_eq!(
        ready[0].events & EPOLLIN,
        EPOLLIN,
        "the second event must carry EPOLLIN"
    );
}

/// Draining a saturated counter is a writability edge for an edge-triggered
/// `EPOLLOUT` watch.
///
/// `poll()` reports `writable = counter < u64::MAX - 1`, so the only read that
/// changes writability is one that drains a counter saturated at `u64::MAX - 1`
/// (the ceiling Linux leaves below the `count == ULLONG_MAX` `EPOLLERR` state).
/// Reaching saturation requires an accepted write, which always bumps the
/// readiness version, so the drain's `EPOLLOUT` edge is reported either through
/// the version change or, when a wait already consumed it, through the
/// `ready & !last_ready` fallback of the edge-triggered delivery path.
fn test_saturated_read_is_a_writable_edge() {
    let epfd = syscalls::epoll_create1(0).expect("epoll_create1(0) failed");
    let fd = syscalls::eventfd(0, EFD_SEMAPHORE | EFD_NONBLOCK)
        .expect("create semaphore eventfd failed");

    let mut interest = EpollEvent {
        events: EPOLLOUT | EPOLLET,
        data: 0,
    };
    syscalls::epoll_ctl(&epfd, EPOLL_CTL_ADD, &fd, Some(&mut interest))
        .expect("epoll_ctl ADD failed");

    // Saturate the counter at u64::MAX - 1: writability flips false, and the
    // accepted write itself is a readiness edge consumed by the first wait.
    assert_eq!(
        syscalls::write_u64(&fd, u64::MAX - 1).unwrap(),
        8,
        "write must return 8"
    );
    let mut ready = [EpollEvent::default(); 4];
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        0,
        "a saturated counter is not writable"
    );

    // A semaphore read drains one unit: u64::MAX - 1 -> u64::MAX - 2, which
    // flips writability back to true. The EPOLLOUT edge must be reported even
    // though no write bumps the version in between.
    assert_eq!(
        syscalls::read_u64(&fd).unwrap(),
        1,
        "semaphore read must return 1"
    );
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        1,
        "draining a saturated counter must surface EPOLLOUT"
    );
    assert_eq!(
        ready[0].events & EPOLLOUT,
        EPOLLOUT,
        "the event must carry EPOLLOUT"
    );
}

/// A write must not spoof a writable edge for an `EPOLLOUT` edge-triggered watch.
///
/// `EventFd::write` bumps the shared readiness version on every accepted write
/// so an edge-triggered `EPOLLIN` watcher observes one edge per write (the
/// async waker path needs this). But the version is a read-readiness signal: a
/// write does not make the counter newly writable, so an `EPOLLOUT` watcher must
/// not see a spurious edge. Linux only reports a writable edge when writability
/// flips false -> true (a drain from the saturation ceiling), never on a plain
/// write. This guards the regression where a single version gated both classes
/// and re-fired EPOLLOUT on every write.
fn test_write_does_not_spoof_writable_edge() {
    let epfd = syscalls::epoll_create1(0).expect("epoll_create1(0) failed");
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("create nonblocking eventfd failed");

    let mut interest = EpollEvent {
        events: EPOLLOUT | EPOLLET,
        data: 0,
    };
    syscalls::epoll_ctl(&epfd, EPOLL_CTL_ADD, &fd, Some(&mut interest))
        .expect("epoll_ctl ADD failed");

    // The freshly created counter is writable (0 < u64::MAX - 1), so the
    // initial writable edge is reported and consumed.
    let mut ready = [EpollEvent::default(); 4];
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        1,
        "the initial writable edge must be reported"
    );
    assert_eq!(
        ready[0].events & EPOLLOUT,
        EPOLLOUT,
        "the initial edge must carry EPOLLOUT"
    );

    // A write grows the counter 0 -> 1. Writability is unchanged (still true),
    // so no new EPOLLOUT edge. A version-only design would wrongly re-deliver
    // EPOLLOUT here because the write bumps the shared readiness version.
    assert_eq!(
        syscalls::write_u64(&fd, 1).unwrap(),
        8,
        "write must return 8"
    );
    assert_eq!(
        syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
        0,
        "a write must not spoof a writable edge for an EPOLLOUT watch"
    );
}

/// A writability edge that appears between two waits must still be delivered.
///
/// `test_saturated_read_is_a_writable_edge` drains the counter while the watch
/// still remembers an unwritable sample, so a plain `writable && !last_writable`
/// rule reports it. Here the initial writable edge is consumed first, then the
/// counter is saturated and drained again without any wait in between: both
/// samples are `true`, so only a writable readiness generation that advances on
/// the Full -> Normal transition can report the edge. The same shape is what a
/// pipe write end sees when a reader frees space between two `epoll_wait`
/// calls.
fn test_writable_edge_between_waits_is_reported() {
    let epfd = syscalls::epoll_create1(0).expect("epoll_create1(0) failed");
    let fd = syscalls::eventfd(0, EFD_SEMAPHORE | EFD_NONBLOCK)
        .expect("create semaphore eventfd failed");

    let mut interest = EpollEvent {
        events: EPOLLOUT | EPOLLET,
        data: 0,
    };
    syscalls::epoll_ctl(&epfd, EPOLL_CTL_ADD, &fd, Some(&mut interest))
        .expect("epoll_ctl ADD failed");

    // The counter starts writable, so the initial edge is reported and consumed.
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

    // Saturate the counter, then drain one unit: writability goes
    // true -> false -> true with no wait observing the false sample.
    assert_eq!(
        syscalls::write_u64(&fd, u64::MAX - 1).unwrap(),
        8,
        "write must return 8"
    );
    assert_eq!(
        syscalls::read_u64(&fd).unwrap(),
        1,
        "semaphore read must return 1"
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

/// A stream of wakes: every write in a long sequence must surface an edge.
///
/// `mio::Waker` usage is not two writes but the whole lifetime of a runtime:
/// every `wake()` is a write and every one must be observed by `epoll_wait`.
/// A single-shot test can only catch "the first wake is lost"; this loop also
/// catches any delivery that drops, batches, or skips alternating wakes.
fn test_write_stream_delivers_every_wake() {
    const WAKES: usize = 256;
    let epfd = syscalls::epoll_create1(0).expect("epoll_create1(0) failed");
    let fd = syscalls::eventfd(0, EFD_NONBLOCK).expect("create nonblocking eventfd failed");

    let mut interest = EpollEvent {
        events: EPOLLIN | EPOLLET,
        data: 0,
    };
    syscalls::epoll_ctl(&epfd, EPOLL_CTL_ADD, &fd, Some(&mut interest))
        .expect("epoll_ctl ADD failed");

    let mut ready = [EpollEvent::default(); 4];
    for i in 0..WAKES {
        assert_eq!(
            syscalls::write_u64(&fd, 1).unwrap(),
            8,
            "write {i} must return 8"
        );
        assert_eq!(
            syscalls::epoll_wait(&epfd, &mut ready, 0).unwrap(),
            1,
            "write {i} must surface an edge"
        );
        assert_ne!(ready[0].events & EPOLLIN, 0, "write {i} must carry EPOLLIN");
    }
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
    test_every_write_is_a_readiness_edge();
    test_saturated_read_is_a_writable_edge();
    test_writable_edge_between_waits_is_reported();
    test_write_does_not_spoof_writable_edge();
    test_write_stream_delivers_every_wake();
    println!("io_mpx: eventfd unit tests OK");
    Ok(())
}
