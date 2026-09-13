//! I/O multiplexing primitives: `eventfd` and `pipe` ABI tests plus an
//! `epoll` smoke test. These cover the syscall surface that async runtimes
//! (e.g. tokio/mio) need to drive timers and wake-ups on ArceOS.

use std::os::fd::AsRawFd;
mod epoll;
mod eventfd;
mod pipe;
mod syscalls;

pub fn run() -> crate::TestResult {
    let baseline = syscalls::eventfd(0, 0).expect("failed to probe the baseline fd slot");
    let baseline_slot = baseline.as_raw_fd();
    drop(baseline);
    pipe::run()?;
    eventfd::run()?;
    epoll::run()?;
    let after = syscalls::eventfd(0, 0).expect("failed to probe the final fd slot");
    assert_eq!(
        after.as_raw_fd(),
        baseline_slot,
        "eventfd/pipe/epoll tests must release every fd they allocate"
    );
    Ok(())
}
