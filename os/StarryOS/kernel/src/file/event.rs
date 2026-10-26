use alloc::{borrow::Cow, sync::Arc};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use axpoll::{IoEvents, Pollable};
use axpoll_set::PollSet;

use crate::{
    StarryError, StarryResult,
    file::{FileLike, IoDst, IoSrc},
    task::{
        current_user_task,
        future::{block_on_user, poll_io},
    },
};

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

    /// Publishes a completion using Linux eventfd_signal semantics.
    ///
    /// Unlike checked additions, a kernel completion may reach `u64::MAX`.
    /// That saturated state reports POLLERR until userspace consumes it; the
    /// completion must not be discarded merely because userspace filled the
    /// counter. Call only from task/deferred context for poll fan-out.
    pub(crate) fn signal_kernel_saturating(&self, value: u64) {
        if value == 0 {
            return;
        }
        let previous = self
            .count
            .try_update(Ordering::Release, Ordering::Relaxed, |count| {
                Some(count.saturating_add(value))
            })
            .expect("saturating eventfd update always has a new value");
        let mut events = IoEvents::IN;
        events.set(IoEvents::ERR, previous.saturating_add(value) == u64::MAX);
        // SAFETY: Completion providers invoke this in task or deferred context.
        // The counter update was published via atomic try_update with Release ordering,
        // and no locks that the registered wakers may re-enter are held by the caller.
        unsafe { self.poll_rx.wake(events) };
    }

    /// Adds to the counter from a kernel producer without user-task signal semantics.
    ///
    /// This path never waits for counter space. It is intended for completion
    /// producers such as Linux AIO workers, which must not impersonate the
    /// submitting user thread or inherit its interruption state.
    pub(crate) fn signal_kernel(&self, value: u64) -> StarryResult<()> {
        if value == u64::MAX {
            return Err(crate::StarryError::InvalidInput);
        }
        if value != 0 {
            self.count
                .try_update(Ordering::Release, Ordering::Acquire, |count| {
                    (u64::MAX - count > value).then_some(count + value)
                })
                .map_err(|_| crate::StarryError::WouldBlock)?;
            // SAFETY: Invoked in task or deferred context. The counter was published
            // with Release ordering before waking observers, and no re-entrant locks are held.
            unsafe { self.poll_rx.wake(IoEvents::IN) };
        }
        Ok(())
    }
}

impl FileLike for EventFd {
    fn validate_write_access(&self) -> StarryResult {
        Ok(())
    }

    fn validate_write_len(&self, len: usize) -> StarryResult {
        if len != size_of::<u64>() {
            return Err(StarryError::InvalidInput);
        }
        Ok(())
    }

    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        if dst.remaining_mut() < size_of::<u64>() {
            return Err(StarryError::InvalidInput);
        }

        let task = current_user_task();
        block_on_user(
            &task,
            poll_io(self, IoEvents::IN, self.nonblocking(), || {
                let result = self
                    .count
                    .try_update(Ordering::Release, Ordering::Acquire, |count| {
                        if count > 0 {
                            let dec = if self.semaphore { 1 } else { count };
                            Some(count - dec)
                        } else {
                            None
                        }
                    });
                match result {
                    Ok(count) => {
                        let value = if self.semaphore { 1 } else { count };
                        dst.write(&value.to_ne_bytes())?;
                        // SAFETY: Invoked in task context after successfully decrementing the counter
                        // with Release ordering. Space is published before waking, and no re-entrant locks are held.
                        unsafe { self.poll_tx.wake(IoEvents::OUT) };
                        Ok(size_of::<u64>())
                    }
                    Err(_) => Err(crate::StarryError::WouldBlock),
                }
            }),
        )
        .into_result()?
    }

    fn write(&self, src: &mut IoSrc) -> StarryResult<usize> {
        if src.remaining() < size_of::<u64>() {
            return Err(StarryError::InvalidInput);
        }

        let mut value = [0; size_of::<u64>()];
        src.read(&mut value)?;
        let value = u64::from_ne_bytes(value);
        if value == u64::MAX {
            return Err(StarryError::InvalidInput);
        }

        let task = current_user_task();
        block_on_user(
            &task,
            poll_io(self, IoEvents::OUT, self.nonblocking(), || {
                self.signal_kernel(value).map(|()| size_of::<u64>())
            }),
        )
        .into_result()?
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, non_blocking: bool) -> StarryResult {
        self.non_blocking.store(non_blocking, Ordering::Release);
        Ok(())
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[eventfd]".into()
    }
}

impl Pollable for EventFd {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        let count = self.count.load(Ordering::Acquire);
        events.set(IoEvents::IN, count > 0);
        events.set(IoEvents::ERR, count == u64::MAX);
        events.set(IoEvents::OUT, u64::MAX - 1 > count);
        events
    }

    /// # Safety
    ///
    /// Must be called in task or deferred context only. The caller must guarantee that
    /// `&self` remains valid for the duration of the registration on `sink`.
    unsafe fn register_shared(
        &self,
        sink: &mut dyn axpoll::SharedRegistrationSink,
        events: IoEvents,
    ) {
        if events.intersects(IoEvents::IN | IoEvents::ERR) {
            // SAFETY: Invoked in task/deferred context per caller contract. `self.poll_rx`
            // is pinned by `self` and implements `PollSource` which remains live for the registration.
            unsafe { sink.register_shared(&self.poll_rx, events & (IoEvents::IN | IoEvents::ERR)) };
        }
        if events.contains(IoEvents::OUT) {
            // SAFETY: Invoked in task/deferred context per caller contract. `self.poll_tx`
            // is pinned by `self` and implements `PollSource` which remains live for the registration.
            unsafe { sink.register_shared(&self.poll_tx, IoEvents::OUT) };
        }
    }

    /// # Safety
    ///
    /// Must be called in task or deferred context only. The caller must guarantee that
    /// `&self` remains valid for the duration of the registration on `sink`.
    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        if events.intersects(IoEvents::IN | IoEvents::ERR) {
            // SAFETY: Invoked in task/deferred context per caller contract. `self.poll_rx`
            // is pinned by `self` and implements `PollSource` which remains live for the registration.
            unsafe {
                sink.register_exclusive(&self.poll_rx, events & (IoEvents::IN | IoEvents::ERR))
            };
        }
        if events.contains(IoEvents::OUT) {
            // SAFETY: Invoked in task/deferred context per caller contract. `self.poll_tx`
            // is pinned by `self` and implements `PollSource` which remains live for the registration.
            unsafe { sink.register_exclusive(&self.poll_tx, IoEvents::OUT) };
        }
    }
}
