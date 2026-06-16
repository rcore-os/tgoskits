//! A library for polling I/O events and waking up tasks.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

use alloc::boxed::Box;
use core::{
    mem::MaybeUninit,
    task::{Context, Waker},
};

use ax_kspin::SpinNoIrq;
use bitflags::bitflags;
use linux_raw_sys::general::*;
use spin::LazyLock;

bitflags! {
    /// I/O events.
    #[derive(Debug, Clone, Copy)]
    pub struct IoEvents: u32 {
        /// Available for read
        const IN     = POLLIN;
        /// Urgent data for read
        const PRI    = POLLPRI;
        /// Available for write
        const OUT    = POLLOUT;

        /// Error condition
        const ERR    = POLLERR;
        /// Hang up
        const HUP    = POLLHUP;
        /// Invalid request
        const NVAL   = POLLNVAL;

        /// Equivalent to [`IN`](Self::IN)
        const RDNORM = POLLRDNORM;
        /// Priority band data can be read
        const RDBAND = POLLRDBAND;
        /// Equivalent to [`OUT`](Self::OUT)
        const WRNORM = POLLWRNORM;
        /// Priority data can be written
        const WRBAND = POLLWRBAND;

        /// Message
        const MSG    = POLLMSG;
        /// Remove
        const REMOVE = POLLREMOVE;
        /// Stream socket peer closed connection, or shut down writing half of connection.
        const RDHUP  = POLLRDHUP;

        /// Events that are always polled even without specifying them.
        const ALWAYS_POLL = Self::ERR.bits() | Self::HUP.bits();
    }
}

/// Trait for types that can be polled for I/O events.
pub trait Pollable {
    /// Polls for I/O events.
    fn poll(&self) -> IoEvents;

    /// Registers wakers for I/O events.
    fn register(&self, context: &mut Context<'_>, events: IoEvents);
}

const POLL_SET_CAPACITY: usize = 64;

struct Entry {
    waker: Waker,
    interests: IoEvents,
}

impl Entry {
    fn wake(self) {
        self.waker.wake();
    }
}

struct Inner {
    entries: Box<[MaybeUninit<Entry>]>,
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

    fn register(&mut self, waker: &Waker, interests: IoEvents) {
        let slot = self.cursor % POLL_SET_CAPACITY;
        if self.cursor >= POLL_SET_CAPACITY {
            let old = unsafe { self.entries[slot].assume_init_read() };
            if !old.waker.will_wake(waker) {
                old.wake();
            }
            self.cursor = ((slot + 1) % POLL_SET_CAPACITY) + POLL_SET_CAPACITY;
        } else {
            self.cursor += 1;
        }
        self.entries[slot].write(Entry {
            waker: waker.clone(),
            interests,
        });
    }

    fn wake(&mut self, ready: IoEvents) -> usize {
        if self.is_empty() {
            return 0;
        }

        let mut old = Self::new();
        core::mem::swap(&mut old, self);

        let mut woke = 0;
        for i in 0..old.len() {
            let entry = unsafe { old.entries[i].assume_init_read() };
            if entry.interests.intersects(ready) {
                woke += 1;
                entry.wake();
            } else {
                self.register(&entry.waker, entry.interests);
            }
        }
        old.cursor = 0;
        woke
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        for i in 0..self.len() {
            unsafe { self.entries[i].assume_init_read() }.wake();
        }
    }
}

/// A data structure for waking up tasks that are waiting for I/O events.
pub struct PollSet(LazyLock<SpinNoIrq<Inner>>);

impl Default for PollSet {
    fn default() -> Self {
        Self::new()
    }
}

impl PollSet {
    /// Creates a new empty [`PollSet`].
    pub const fn new() -> Self {
        Self(LazyLock::new(|| SpinNoIrq::new(Inner::new())))
    }

    /// Registers a waker for the requested I/O events.
    ///
    /// # Safety
    ///
    /// This method is task/deferred-context only. Callers must not invoke it
    /// from hard IRQ, NMI, or trap callbacks, and must not hold locks that may
    /// be re-entered by the registered waker or by poll wakeup paths.
    pub unsafe fn register(&self, waker: &Waker, interests: IoEvents) {
        self.0.lock().register(waker, interests);
    }

    /// Wakes up registered wakers whose interests intersect `ready`.
    ///
    /// # Safety
    ///
    /// This method is task/deferred-context only. Callers must not invoke it
    /// from hard IRQ, NMI, or trap callbacks. The readiness state represented
    /// by `ready` must be published before this method is called, and callers
    /// must not hold locks that may be re-entered by waker execution or poll
    /// wakeup paths.
    pub unsafe fn wake(&self, ready: IoEvents) -> usize {
        self.0.lock().wake(ready)
    }
}

impl Drop for PollSet {
    fn drop(&mut self) {
        // Ensure all entries are dropped
        unsafe { self.wake(IoEvents::all()) };
    }
}
