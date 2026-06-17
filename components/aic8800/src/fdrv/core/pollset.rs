//! Minimal waker set for cross-thread task wakeups.
//!
//! Vendored from the `axpoll` crate's `PollSet`, trimmed to the subset the
//! Wi-Fi driver uses (register / wake) and reworked to use a plain spinlock so
//! the driver core does not depend on any OS-specific synchronization crate.
//!
//! Safety note: this lock is **not** IRQ-masking. It must only ever be locked
//! from task/thread context, never from an interrupt handler. The one wakeup
//! path that is shared with the SDIO ISR uses [`atomic_waker::AtomicWaker`]
//! instead (see `RxState::irq_waker`), which is lock-free and ISR-safe.

use alloc::boxed::Box;
use core::{mem::MaybeUninit, task::Waker};

use spin::Mutex;

const POLL_SET_CAPACITY: usize = 64;

struct Inner {
    entries: Box<[MaybeUninit<Waker>]>,
    cursor: usize,
}

impl Inner {
    fn new() -> Self {
        Self {
            entries: Box::new_uninit_slice(POLL_SET_CAPACITY),
            cursor: 0,
        }
    }

    fn len(&self) -> usize {
        self.cursor.min(POLL_SET_CAPACITY)
    }

    fn is_empty(&self) -> bool {
        self.cursor == 0
    }

    fn register(&mut self, waker: &Waker) {
        let slot = self.cursor % POLL_SET_CAPACITY;
        if self.cursor >= POLL_SET_CAPACITY {
            let old = unsafe { self.entries[slot].assume_init_read() };
            if !old.will_wake(waker) {
                old.wake();
            }
            self.cursor = ((slot + 1) % POLL_SET_CAPACITY) + POLL_SET_CAPACITY;
        } else {
            self.cursor += 1;
        }
        self.entries[slot].write(waker.clone());
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        for i in 0..self.len() {
            unsafe { self.entries[i].assume_init_read() }.wake();
        }
    }
}

/// A set of wakers waiting on a single condition, woken together.
pub struct PollSet(Mutex<Option<Inner>>);

impl Default for PollSet {
    fn default() -> Self {
        Self::new()
    }
}

impl PollSet {
    /// Creates a new empty [`PollSet`].
    pub const fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// Registers a waker to be woken on the next [`wake`](Self::wake).
    pub fn register(&self, waker: &Waker) {
        let mut guard = self.0.lock();
        guard.get_or_insert_with(Inner::new).register(waker);
    }

    /// Wakes all currently registered wakers. Returns how many were woken.
    pub fn wake(&self) -> usize {
        let taken = {
            let mut guard = self.0.lock();
            match guard.as_ref() {
                Some(inner) if !inner.is_empty() => guard.take(),
                _ => None,
            }
        };
        match taken {
            Some(inner) => {
                let n = inner.len();
                drop(inner); // wakes all entries outside the lock
                n
            }
            None => 0,
        }
    }
}

impl Drop for PollSet {
    fn drop(&mut self) {
        self.wake();
    }
}
