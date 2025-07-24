use alloc::sync::Arc;
use core::{
    any::Any,
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use axerrno::LinuxError;
use axio::PollState;
use axtask::WaitQueue;
use starry_core::task::AssumeSync;

use crate::file::{FileLike, Kstat};

pub struct EventFd {
    wq: WaitQueue,
    count: AssumeSync<UnsafeCell<u64>>,
    semaphore: bool,
    non_blocking: AtomicBool,
}

impl EventFd {
    pub fn new(initval: u64, semaphore: bool) -> Arc<Self> {
        Arc::new(Self {
            wq: WaitQueue::new(),
            count: AssumeSync(UnsafeCell::new(initval)),
            semaphore,
            non_blocking: AtomicBool::new(false),
        })
    }
}

impl FileLike for EventFd {
    fn read(&self, buf: &mut [u8]) -> axio::Result<usize> {
        if buf.len() < size_of::<u64>() {
            return Err(LinuxError::EINVAL);
        }

        let mut result = 0;
        self.wq.wait_until(|| {
            // SAFETY: condition is evaluated under the lock of the wait queue,
            // so it is safe to access the count.
            let count = unsafe { &mut *self.count.get() };
            if *count > 0 {
                result = if self.semaphore { 1 } else { *count };
                *count -= result;
                true
            } else {
                false
            }
        });
        // TODO: better way?
        self.wq.notify_all(false);

        let data = result.to_ne_bytes();
        buf.copy_from_slice(&data);
        Ok(data.len())
    }

    fn write(&self, buf: &[u8]) -> axio::Result<usize> {
        if buf.len() < size_of::<u64>() {
            return Err(LinuxError::EINVAL);
        }

        let value = u64::from_ne_bytes(buf[..size_of::<u64>()].try_into().unwrap());
        if value == u64::MAX {
            return Err(LinuxError::EINVAL);
        }

        let non_blocking = self.nonblocking();
        let mut failed = false;
        self.wq.wait_until(|| {
            // SAFETY: condition is evaluated under the lock of the wait queue,
            // so it is safe to access the count.
            let count = unsafe { &mut *self.count.get() };
            if u64::MAX - *count > value {
                *count += value;
                true
            } else if non_blocking {
                failed = true;
                true
            } else {
                false
            }
        });
        if failed {
            return Err(LinuxError::EAGAIN);
        }

        Ok(size_of::<u64>())
    }

    fn stat(&self) -> axio::Result<Kstat> {
        Ok(Kstat::default())
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, non_blocking: bool) -> axio::Result {
        self.non_blocking.store(non_blocking, Ordering::Release);
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn poll(&self) -> axio::Result<PollState> {
        // SAFETY: We just read once
        let count = unsafe { *self.count.get() };
        Ok(PollState {
            readable: count > 0,
            writable: u64::MAX - 1 > count,
        })
    }
}
