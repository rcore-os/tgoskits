//! I/O multiplexing primitives: `eventfd` unit tests and an `epoll` smoke
//! test. These cover the syscall surface that async runtimes (e.g. tokio/mio)
//! need to drive timers and wake-ups on ArceOS.

mod epoll;
mod eventfd;
mod syscalls;

pub fn run() -> crate::TestResult {
    let baseline = syscalls::eventfd(0, 0).expect("failed to probe the baseline fd slot");
    let baseline_slot = baseline.as_raw();
    drop(baseline);
    eventfd::run()?;
    epoll::run()?;
    let after = syscalls::eventfd(0, 0).expect("failed to probe the final fd slot");
    assert_eq!(
        after.as_raw(),
        baseline_slot,
        "eventfd/epoll tests must release every fd they allocate"
    );
    Ok(())
}
