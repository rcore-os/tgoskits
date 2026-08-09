//! I/O multiplexing primitives: `eventfd` unit tests and an `epoll` smoke
//! test. These cover the syscall surface that async runtimes (e.g. tokio/mio)
//! need to drive timers and wake-ups on ArceOS.

mod epoll;
mod eventfd;
mod syscalls;

pub fn run() -> crate::TestResult {
    eventfd::run()?;
    epoll::run()?;
    Ok(())
}
