//! A library for polling I/O events and waking up tasks.

#![no_std]
#![deny(missing_docs)]

#[cfg(feature = "alloc")]
extern crate alloc;

use core::{
    mem::MaybeUninit,
    task::{Context, Waker},
};

use bitflags::bitflags;
use linux_raw_sys::general::*;
use spin::Mutex;

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

struct Inner {
    entries: [MaybeUninit<Waker>; POLL_SET_CAPACITY],
    cursor: usize,
}

impl Inner {
    const fn new() -> Self {
        Self {
            entries: unsafe { MaybeUninit::uninit().assume_init() },
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

/// A data structure for waking up tasks that are waiting for I/O events.
pub struct PollSet(Mutex<Inner>);

impl Default for PollSet {
    fn default() -> Self {
        Self::new()
    }
}

impl PollSet {
    /// Creates a new empty [`PollSet`].
    pub const fn new() -> Self {
        Self(Mutex::new(Inner::new()))
    }

    /// Registers a waker.
    pub fn register(&self, waker: &Waker) {
        self.0.lock().register(waker);
    }

    /// Wakes up all registered wakers.
    pub fn wake(&self) -> usize {
        let mut guard = self.0.lock();
        if guard.is_empty() {
            return 0;
        }
        let inner = core::mem::replace(&mut *guard, Inner::new());
        drop(guard);
        inner.len()
    }
}

impl Drop for PollSet {
    fn drop(&mut self) {
        // Ensure all entries are dropped
        self.wake();
    }
}

#[cfg(feature = "alloc")]
impl alloc::task::Wake for PollSet {
    fn wake(self: alloc::sync::Arc<Self>) {
        self.as_ref().wake();
    }

    fn wake_by_ref(self: &alloc::sync::Arc<Self>) {
        self.as_ref().wake();
    }
}
