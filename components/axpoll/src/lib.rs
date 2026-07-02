//! A library for polling I/O events and waking up tasks.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;
use core::{
    mem::MaybeUninit,
    task::{Context, Waker},
};

use ax_kspin::SpinNoIrq;
use bitflags::bitflags;
use linux_raw_sys::general::*;
use spin::Once;

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

const IRQ_WAKE_BATCH: usize = 64;

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
    entries: Vec<Entry>,
}

impl Inner {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn register(&mut self, waker: &Waker, interests: IoEvents) {
        for entry in &mut self.entries {
            if entry.waker.will_wake(waker) {
                entry.waker = waker.clone();
                entry.interests |= interests;
                return;
            }
        }

        self.entries.push(Entry {
            waker: waker.clone(),
            interests,
        });
    }

    fn drain_ready(&mut self, ready: IoEvents, ready_entries: &mut Vec<Entry>) {
        if self.is_empty() {
            return;
        }

        let mut index = 0;
        while index < self.entries.len() {
            if self.entries[index].interests.intersects(ready) {
                ready_entries.push(self.entries.swap_remove(index));
            } else {
                index += 1;
            }
        }
    }

    fn drain_ready_batch(
        &mut self,
        ready: IoEvents,
        ready_entries: &mut [MaybeUninit<Entry>],
    ) -> usize {
        let mut ready_len = 0;
        let mut index = 0;
        while index < self.entries.len() && ready_len < ready_entries.len() {
            if self.entries[index].interests.intersects(ready) {
                ready_entries[ready_len].write(self.entries.swap_remove(index));
                ready_len += 1;
            } else {
                index += 1;
            }
        }
        ready_len
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        for entry in self.entries.drain(..) {
            entry.wake();
        }
    }
}

/// A data structure for waking up tasks that are waiting for I/O events.
pub struct PollSet(Once<SpinNoIrq<Inner>>);

impl Default for PollSet {
    fn default() -> Self {
        Self::new()
    }
}

impl PollSet {
    /// Creates a new empty [`PollSet`].
    pub const fn new() -> Self {
        Self(Once::new())
    }

    /// Registers a waker for the requested I/O events.
    ///
    /// # Safety
    ///
    /// This method is task/deferred-context only. Callers must not invoke it
    /// from hard IRQ, NMI, or trap callbacks, and must not hold locks that may
    /// be re-entered by the registered waker or by poll wakeup paths.
    pub unsafe fn register(&self, waker: &Waker, interests: IoEvents) {
        self.0
            .call_once(|| SpinNoIrq::new(Inner::new()))
            .lock()
            .register(waker, interests);
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
        let Some(inner) = self.0.get() else {
            return 0;
        };
        let mut ready_entries = Vec::new();
        {
            inner.lock().drain_ready(ready, &mut ready_entries);
        }
        let woke = ready_entries.len();
        for entry in ready_entries {
            entry.wake();
        }
        woke
    }

    /// Wakes up registered wakers whose interests intersect `ready` from IRQ context.
    ///
    /// This method is kept for legacy users only. New hard IRQ code should
    /// publish state into the device/backend and wake a deferred worker via an
    /// IRQ-safe task waker instead. Unlike [`wake`](Self::wake), this method
    /// avoids allocation by draining matching entries in fixed-size batches.
    pub fn wake_from_irq(&self, ready: IoEvents) -> usize {
        let Some(inner) = self.0.get() else {
            return 0;
        };

        let mut woke = 0;
        loop {
            let mut ready_entries = [const { MaybeUninit::<Entry>::uninit() }; IRQ_WAKE_BATCH];
            let ready_len = {
                let mut inner = inner.lock();
                if inner.is_empty() {
                    return woke;
                }
                inner.drain_ready_batch(ready, &mut ready_entries)
            };
            if ready_len == 0 {
                return woke;
            }

            woke += ready_len;
            for entry in ready_entries.iter_mut().take(ready_len) {
                unsafe { entry.assume_init_read() }.wake();
            }
        }
    }
}

impl Drop for PollSet {
    fn drop(&mut self) {
        // Ensure all entries are dropped
        unsafe { self.wake(IoEvents::all()) };
    }
}
