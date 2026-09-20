//! `eventfd` implementation.
//!
//! An `eventfd` exposes a 64-bit counter as a file descriptor: reads return the
//! current counter and reset it to zero, writes add an 8-byte value to it.
//! Async I/O runtimes use it to wake a blocked `epoll_wait` (e.g. mio's
//! `Waker` creates one with `EFD_CLOEXEC | EFD_NONBLOCK` and writes `1` to
//! wake). Blocking reads/writes yield cooperatively like the rest of ArceOS.

use alloc::sync::Arc;
use core::ffi::{c_int, c_uint};

use ax_io::PollState;

use super::fd_ops::{FileLike, add_file_like};
use crate::{PosixError, PosixResult, ctypes, sync::Mutex};

const EFD_SUPPORTED_FLAGS: u32 = ctypes::EFD_SEMAPHORE | ctypes::EFD_CLOEXEC | ctypes::EFD_NONBLOCK;

pub struct EventFd {
    inner: Mutex<EventFdInner>,
}

struct EventFdInner {
    counter: u64,
    semaphore: bool,
    nonblocking: bool,
    read_readiness_version: u64,
    write_readiness_version: u64,
}

impl EventFd {
    pub fn new(initval: u32, semaphore: bool, nonblocking: bool) -> Self {
        Self {
            inner: Mutex::new(EventFdInner {
                counter: initval as u64,
                semaphore,
                nonblocking,
                read_readiness_version: 0,
                write_readiness_version: 0,
            }),
        }
    }
}

impl FileLike for EventFd {
    fn read(&self, buf: &mut [u8]) -> PosixResult<usize> {
        if buf.len() < 8 {
            return Err(PosixError::EINVAL);
        }
        loop {
            let mut inner = self.inner.lock();
            if inner.counter == 0 {
                if inner.nonblocking {
                    return Err(PosixError::EAGAIN);
                }
                // Busy-wait: there is no wait queue to park on, so a blocking
                // reader just yields to the cooperative scheduler and re-checks.
                // TODO: park on a wait queue once the scheduler grows one, so an
                // isolated waiter does not spin its own core.
                drop(inner);
                crate::sys_sched_yield(); // Wait for a writer.
                continue;
            }
            // The counter was `> 0` on entry (guarded above), so the drain makes
            // it readable→unreadable exactly when it hits zero. Only that
            // transition must bump the read version.
            if inner.counter == u64::MAX - 1 {
                // The pre-read counter sat at the saturation ceiling, so this
                // drain is the one read that flips writability false -> true.
                inner.write_readiness_version = inner.write_readiness_version.wrapping_add(1);
            }
            let value = if inner.semaphore {
                inner.counter -= 1;
                1
            } else {
                let v = inner.counter;
                inner.counter = 0;
                v
            };
            if inner.counter == 0 {
                inner.read_readiness_version = inner.read_readiness_version.wrapping_add(1);
            }
            buf[..8].copy_from_slice(&value.to_ne_bytes());
            return Ok(8);
        }
    }

    fn write(&self, buf: &[u8]) -> PosixResult<usize> {
        // Linux requires the write buffer to be exactly 8 bytes (fs/eventfd.c:
        // `if (count != sizeof(ucnt)) return -EINVAL;`). Unlike read, which
        // accepts any buffer of at least 8 bytes, a longer write fails too.
        if buf.len() != 8 {
            return Err(PosixError::EINVAL);
        }
        let value = u64::from_ne_bytes(buf[..8].try_into().unwrap());
        // A write of UINT64_MAX always fails with EINVAL (fs/eventfd.c).
        if value == u64::MAX {
            return Err(PosixError::EINVAL);
        }
        loop {
            let mut inner = self.inner.lock();
            // The counter saturates at UINT64_MAX - 1; a write whose addition
            // would reach or exceed UINT64_MAX blocks, or fails with EAGAIN
            // if nonblocking. `u64::MAX - value` never underflows because
            // `value` is at most UINT64_MAX - 1.
            if inner.counter >= u64::MAX - value {
                if inner.nonblocking {
                    return Err(PosixError::EAGAIN);
                }
                // Busy-wait, same as read(): TODO park on a wait queue.
                drop(inner);
                crate::sys_sched_yield(); // Wait for a reader to drain the counter.
                continue;
            }
            inner.counter += value;
            // Every accepted write is a read-readiness wakeup event, not only
            // the one that flips the counter from zero to non-zero: Linux
            // `eventfd_write` calls `wake_up_locked_poll(&ctx->wqh, EPOLLIN)`
            // on every write (fs/eventfd.c), unconditionally, so a waiter must
            // observe one edge per write. Bumping only when readability flips
            // drops every wake after the first while the counter stays
            // non-zero -- which is exactly how an async runtime uses its
            // `mio::Waker` eventfd (it never drains it), hanging the second and
            // later `spawn_blocking` calls behind an `epoll_wait` that never
            // returns.
            inner.read_readiness_version = inner.read_readiness_version.wrapping_add(1);
            if inner.counter == u64::MAX - 1 {
                // Reaching the saturation ceiling is the only write that flips
                // writability (true -> false); a plain write must not spoof a
                // writable edge for an `EPOLLOUT` watcher.
                inner.write_readiness_version = inner.write_readiness_version.wrapping_add(1);
            }
            return Ok(8);
        }
    }

    fn stat(&self) -> PosixResult<ctypes::stat> {
        let st_mode = 0o100000 | 0o600u32; // S_IFREG | rw-------
        Ok(ctypes::stat {
            st_ino: 1,
            st_nlink: 1,
            st_mode,
            ..Default::default()
        })
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }

    fn poll(&self) -> PosixResult<PollState> {
        let inner = self.inner.lock();
        // Matches Linux `eventfd_poll`: writable only while
        // `count < ULLONG_MAX - 1`, i.e. while a 1-unit write can still
        // succeed. The `count == ULLONG_MAX` overflow state Linux reports as
        // `EPOLLERR` is unreachable here: only the kernel signal path can land
        // the counter there, and ArceOS writes saturate at `ULLONG_MAX - 1`.
        Ok(PollState {
            readable: inner.counter > 0,
            writable: inner.counter < u64::MAX - 1,
            read_readiness_version: inner.read_readiness_version,
            write_readiness_version: inner.write_readiness_version,
        })
    }

    fn set_nonblocking(&self, nonblocking: bool) -> PosixResult {
        self.inner.lock().nonblocking = nonblocking;
        Ok(())
    }
}

/// Create a file descriptor for event notification.
pub fn sys_eventfd(initval: c_uint, flags: c_int) -> c_int {
    debug!("sys_eventfd <= initval: {initval} flags: {flags:#x}");
    syscall_body!(sys_eventfd, {
        let flags = flags as u32;
        if flags & !EFD_SUPPORTED_FLAGS != 0 {
            return Err(PosixError::EINVAL);
        }
        // `EFD_CLOEXEC` is validated above but deliberately not stored: ArceOS
        // has no `exec`, so there is no child fd table to close it from.
        let eventfd = EventFd::new(
            initval,
            flags & ctypes::EFD_SEMAPHORE != 0,
            flags & ctypes::EFD_NONBLOCK != 0,
        );
        add_file_like(Arc::new(eventfd))
    })
}
