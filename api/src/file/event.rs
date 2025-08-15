use alloc::{borrow::Cow, sync::Arc};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    task::Context,
};

use axerrno::LinuxError;
use axio::{IoEvents, PollSet, Pollable};
use axtask::future::Poller;

use crate::file::{FileLike, Kstat};

pub struct EventFd {
    count: AtomicU64,
    semaphore: bool,
    non_blocking: AtomicBool,

    poll_rx: PollSet,
    poll_tx: PollSet,
}

impl EventFd {
    pub fn new(initval: u64, semaphore: bool) -> Arc<Self> {
        Arc::new(Self {
            count: AtomicU64::new(initval),
            semaphore,
            non_blocking: AtomicBool::new(false),

            poll_rx: PollSet::new(),
            poll_tx: PollSet::new(),
        })
    }
}

impl FileLike for EventFd {
    fn read(&self, buf: &mut [u8]) -> axio::Result<usize> {
        if buf.len() < size_of::<u64>() {
            return Err(LinuxError::EINVAL);
        }

        Poller::new(self, IoEvents::IN)
            .non_blocking(self.nonblocking())
            .poll(|| {
                let result =
                    self.count
                        .fetch_update(Ordering::Release, Ordering::Acquire, |count| {
                            if count > 0 {
                                let dec = if self.semaphore { 1 } else { count };
                                Some(count - dec)
                            } else {
                                None
                            }
                        });
                match result {
                    Ok(count) => {
                        let data = count.to_ne_bytes();
                        buf[..data.len()].copy_from_slice(&data);
                        self.poll_tx.wake();
                        Ok(data.len())
                    }
                    Err(_) => Err(LinuxError::EAGAIN),
                }
            })
    }

    fn write(&self, buf: &[u8]) -> axio::Result<usize> {
        if buf.len() < size_of::<u64>() {
            return Err(LinuxError::EINVAL);
        }

        let value = u64::from_ne_bytes(buf[..size_of::<u64>()].try_into().unwrap());
        if value == u64::MAX {
            return Err(LinuxError::EINVAL);
        }

        Poller::new(self, IoEvents::OUT)
            .non_blocking(self.nonblocking())
            .poll(|| {
                let result =
                    self.count
                        .fetch_update(Ordering::Release, Ordering::Acquire, |count| {
                            if u64::MAX - count > value {
                                Some(count + value)
                            } else {
                                None
                            }
                        });
                match result {
                    Ok(_) => {
                        self.poll_rx.wake();
                        Ok(size_of::<u64>())
                    }
                    Err(_) => Err(LinuxError::EAGAIN),
                }
            })
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

    fn path(&self) -> Cow<str> {
        "anon_inode:[eventfd]".into()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl Pollable for EventFd {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        let count = self.count.load(Ordering::Acquire);
        events.set(IoEvents::IN, count > 0);
        events.set(IoEvents::OUT, u64::MAX - 1 > count);
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_rx.register(context.waker());
        }
        if events.contains(IoEvents::OUT) {
            self.poll_tx.register(context.waker());
        }
    }
}
