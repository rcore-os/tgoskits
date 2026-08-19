//! Typed readiness capabilities independent of a queue or scheduler implementation.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{marker::PhantomData, task::Waker};

use bitflags::bitflags;
use linux_raw_sys::general::*;

bitflags! {
    /// I/O readiness events.
    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    pub struct IoEvents: u32 {
        /// Available for read.
        const IN     = POLLIN;
        /// Urgent data for read.
        const PRI    = POLLPRI;
        /// Available for write.
        const OUT    = POLLOUT;
        /// Error condition.
        const ERR    = POLLERR;
        /// Hang up.
        const HUP    = POLLHUP;
        /// Invalid request.
        const NVAL   = POLLNVAL;
        /// Equivalent to [`IN`](Self::IN).
        const RDNORM = POLLRDNORM;
        /// Priority band data can be read.
        const RDBAND = POLLRDBAND;
        /// Equivalent to [`OUT`](Self::OUT).
        const WRNORM = POLLWRNORM;
        /// Priority data can be written.
        const WRBAND = POLLWRBAND;
        /// Message.
        const MSG    = POLLMSG;
        /// Remove.
        const REMOVE = POLLREMOVE;
        /// Stream socket peer closed connection, or shut down writing half.
        const RDHUP  = POLLRDHUP;
        /// Events reported even when callers did not request them.
        const ALWAYS_POLL = Self::ERR.bits() | Self::HUP.bits();
    }
}

/// Marker for a readiness observer that does not consume an event.
pub enum SharedObserver {}

/// Marker for a waiter that competes to consume one readiness event.
pub enum ExclusiveConsumer {}

/// Selection mode attached to one readiness registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistrationMode {
    /// Every matching observer is notified.
    Shared,
    /// One matching consumer is notified by an ordinary wake transaction.
    Exclusive,
}

/// An owned source registration whose drop cancels the exact registered entry.
pub trait PollRegistration: Send {
    /// Returns whether the source selected this registration for notification.
    ///
    /// Once true, the result remains true until the registration is dropped.
    /// Sources publish this state before invoking the registered waker.
    fn was_notified(&self) -> bool;
}

/// A readiness source capable of creating owned registrations.
pub trait PollSource: Send + Sync {
    /// Registers `waker` and returns the exact cancellation lease.
    ///
    /// # Safety
    ///
    /// Registration is task/deferred-context only. The caller must not invoke
    /// it from hard IRQ, NMI, or a trap callback. A producer must publish its
    /// readiness state before waking this registration, and the consumer's
    /// next readiness check must observe that publication through the same
    /// lock or a matching Release/Acquire synchronization pair.
    unsafe fn register(
        &self,
        waker: &Waker,
        interests: IoEvents,
        mode: RegistrationMode,
    ) -> Option<Box<dyn PollRegistration>>;
}

impl<T: PollSource + ?Sized> PollSource for Arc<T> {
    unsafe fn register(
        &self,
        waker: &Waker,
        interests: IoEvents,
        mode: RegistrationMode,
    ) -> Option<Box<dyn PollRegistration>> {
        unsafe { self.as_ref().register(waker, interests, mode) }
    }
}

struct OwnedRegistration {
    lease: Box<dyn PollRegistration>,
    mode: RegistrationMode,
}

/// Owns every readiness registration made by one polling attempt.
///
/// Dropping or resetting a registrar cancels every still-live source lease.
#[must_use = "dropping the registrar immediately cancels its poll registrations"]
pub struct PollRegistrar<M> {
    waker: Waker,
    registrations: Vec<OwnedRegistration>,
    mode: PhantomData<fn() -> M>,
}

impl<M> PollRegistrar<M> {
    /// Creates an empty registrar for `waker`.
    pub fn new(waker: &Waker) -> Self {
        Self {
            waker: waker.clone(),
            registrations: Vec::new(),
            mode: PhantomData,
        }
    }

    /// Cancels the previous polling attempt and starts one with `waker`.
    pub fn reset(&mut self, waker: &Waker) {
        self.registrations.clear();
        self.waker.clone_from(waker);
    }

    /// Cancels every registration owned by this registrar.
    pub fn clear(&mut self) {
        self.registrations.clear();
    }

    /// Returns whether this registrar currently owns no registration.
    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }

    unsafe fn register_mode(
        &mut self,
        source: &dyn PollSource,
        interests: IoEvents,
        mode: RegistrationMode,
    ) {
        if interests.is_empty() {
            return;
        }
        if let Some(lease) = unsafe { source.register(&self.waker, interests, mode) } {
            self.registrations.push(OwnedRegistration { lease, mode });
        }
    }
}

impl PollRegistrar<SharedObserver> {
    /// Registers this observer in `source` for `interests`.
    ///
    /// # Safety
    ///
    /// Registration is task/deferred-context only.
    pub unsafe fn register(&mut self, source: &dyn PollSource, interests: IoEvents) {
        unsafe { self.register_mode(source, interests, RegistrationMode::Shared) };
    }
}

impl PollRegistrar<ExclusiveConsumer> {
    /// Returns whether an exclusive source selected this polling attempt.
    ///
    /// Consumptive sources use this to transfer still-available readiness to
    /// the next exclusive waiter without turning ordinary wakeups into a
    /// broadcast.
    pub fn was_exclusively_notified(&self) -> bool {
        self.registrations.iter().any(|registration| {
            registration.mode == RegistrationMode::Exclusive && registration.lease.was_notified()
        })
    }

    /// Registers this consumer as an exclusive waiter.
    ///
    /// # Safety
    ///
    /// Registration is task/deferred-context only.
    pub unsafe fn register_exclusive(&mut self, source: &dyn PollSource, interests: IoEvents) {
        unsafe { self.register_mode(source, interests, RegistrationMode::Exclusive) };
    }

    /// Registers this consumer as a shared observer at a composite boundary.
    ///
    /// # Safety
    ///
    /// Registration is task/deferred-context only.
    pub unsafe fn register_shared(&mut self, source: &dyn PollSource, interests: IoEvents) {
        unsafe { self.register_mode(source, interests, RegistrationMode::Shared) };
    }
}

/// Capability for adding shared registrations to an owned attempt.
pub trait SharedRegistrationSink {
    /// Returns the waker owned by this registration attempt.
    fn waker(&self) -> &Waker;

    /// Adds a shared registration owned by this sink.
    ///
    /// # Safety
    ///
    /// Registration is task/deferred-context only.
    unsafe fn register_shared(&mut self, source: &dyn PollSource, interests: IoEvents);
}

/// Capability for adding exclusive registrations to an owned attempt.
pub trait ExclusiveRegistrationSink {
    /// Returns the waker owned by this registration attempt.
    fn waker(&self) -> &Waker;

    /// Adds an exclusive registration owned by this sink.
    ///
    /// # Safety
    ///
    /// Registration is task/deferred-context only.
    unsafe fn register_exclusive(&mut self, source: &dyn PollSource, interests: IoEvents);

    /// Borrows this sink's shared-registration capability.
    fn as_shared(&mut self) -> &mut dyn SharedRegistrationSink;
}

impl SharedRegistrationSink for PollRegistrar<SharedObserver> {
    fn waker(&self) -> &Waker {
        &self.waker
    }

    unsafe fn register_shared(&mut self, source: &dyn PollSource, interests: IoEvents) {
        unsafe { self.register(source, interests) };
    }
}

impl SharedRegistrationSink for PollRegistrar<ExclusiveConsumer> {
    fn waker(&self) -> &Waker {
        &self.waker
    }

    unsafe fn register_shared(&mut self, source: &dyn PollSource, interests: IoEvents) {
        unsafe { self.register_shared(source, interests) };
    }
}

impl ExclusiveRegistrationSink for PollRegistrar<ExclusiveConsumer> {
    fn waker(&self) -> &Waker {
        &self.waker
    }

    unsafe fn register_exclusive(&mut self, source: &dyn PollSource, interests: IoEvents) {
        unsafe { self.register_exclusive(source, interests) };
    }

    fn as_shared(&mut self) -> &mut dyn SharedRegistrationSink {
        self
    }
}

/// A value that reports I/O readiness and publishes owned registrations.
pub trait Pollable {
    /// Polls for I/O events.
    fn poll(&self) -> IoEvents;

    /// Registers a shared readiness observer.
    ///
    /// # Safety
    ///
    /// Registration is task/deferred-context only.
    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents);

    /// Registers a consumer that may sleep until readiness changes.
    ///
    /// The default preserves shared-observer semantics. Consumptive sources
    /// override it and use the exclusive capability.
    ///
    /// # Safety
    ///
    /// Registration is task/deferred-context only.
    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        unsafe { self.register_shared(sink.as_shared(), events) };
    }
}
