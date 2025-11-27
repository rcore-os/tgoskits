use alloc::{borrow::Cow, sync::Arc};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    task::Context,
};

use axerrno::AxError;
use axio::{Buf, BufMut, Read, Write};
use axpoll::{IoEvents, PollSet, Pollable};
use axtask::future::{block_on, poll_io};

use crate::file::{FileLike, Kstat, SealedBuf, SealedBufMut};

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
    fn read(&self, dst: &mut SealedBufMut) -> axio::Result<usize> {
        if dst.remaining_mut() < size_of::<u64>() {
            return Err(AxError::InvalidInput);
        }

        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            let result = self
                .count
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
                    dst.write(&count.to_ne_bytes())?;
                    self.poll_tx.wake();
                    Ok(size_of::<u64>())
                }
                Err(_) => Err(AxError::WouldBlock),
            }
        }))
    }

    fn write(&self, src: &mut SealedBuf) -> axio::Result<usize> {
        if src.remaining() < size_of::<u64>() {
            return Err(AxError::InvalidInput);
        }

        let mut value = [0; size_of::<u64>()];
        src.read(&mut value)?;
        let value = u64::from_ne_bytes(value);
        if value == u64::MAX {
            return Err(AxError::InvalidInput);
        }

        block_on(poll_io(self, IoEvents::OUT, self.nonblocking(), || {
            let result = self
                .count
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
                Err(_) => Err(AxError::WouldBlock),
            }
        }))
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
