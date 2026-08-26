//! Shared and exclusive readiness queue implementation.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::Waker,
};

use ax_lazyinit::OnceLock;
use ax_sync::SpinLock;
use axpoll::{IoEvents, PollRegistration, PollSource, RegistrationMode};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RegistrationId(u64);

struct Entry {
    id: RegistrationId,
    waker: Waker,
    notified: Arc<AtomicBool>,
    interests: IoEvents,
    mode: RegistrationMode,
}

struct Inner {
    entries: Vec<Entry>,
    next_id: u64,
    closed: bool,
}

impl Inner {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 0,
            closed: false,
        }
    }

    fn register(
        &mut self,
        waker: &Waker,
        interests: IoEvents,
        mode: RegistrationMode,
    ) -> Option<(RegistrationId, Arc<AtomicBool>)> {
        if self.closed || interests.is_empty() {
            return None;
        }
        let id = RegistrationId(self.next_id);
        let notified = Arc::new(AtomicBool::new(false));
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("poll registration ID space exhausted");
        self.entries.push(Entry {
            id,
            waker: waker.clone(),
            notified: notified.clone(),
            interests,
            mode,
        });
        Some((id, notified))
    }

    fn unregister(&mut self, id: RegistrationId) {
        if let Some(index) = self.entries.iter().position(|entry| entry.id == id) {
            self.entries.remove(index);
        }
    }

    fn wake_boundary(&self) -> u64 {
        self.next_id
    }

    fn take_next_matching(
        &mut self,
        ready: IoEvents,
        boundary: u64,
        exclusive_available: bool,
    ) -> Option<Entry> {
        let index = self.entries.iter().position(|entry| {
            entry.id.0 < boundary
                && entry.interests.intersects(ready)
                && (entry.mode == RegistrationMode::Shared || exclusive_available)
        })?;
        self.entries[index].notified.store(true, Ordering::Release);
        Some(self.entries.remove(index))
    }

    fn take_next_before(&mut self, boundary: u64) -> Option<Entry> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id.0 < boundary)?;
        self.entries[index].notified.store(true, Ordering::Release);
        Some(self.entries.remove(index))
    }
}

struct PollState(SpinLock<Inner>);

impl PollState {
    const fn new() -> Self {
        Self(SpinLock::new(Inner::new()))
    }

    fn register(
        &self,
        waker: &Waker,
        interests: IoEvents,
        mode: RegistrationMode,
    ) -> Option<(RegistrationId, Arc<AtomicBool>)> {
        self.0.lock().register(waker, interests, mode)
    }

    fn unregister(&self, id: RegistrationId) {
        self.0.lock().unregister(id);
    }

    fn wake_with(
        &self,
        ready: IoEvents,
        mut exclusive_budget: usize,
        wake: &mut impl FnMut(Waker),
    ) -> usize {
        let boundary = self.0.lock().wake_boundary();
        let mut woke = 0;
        loop {
            let entry = self
                .0
                .lock()
                .take_next_matching(ready, boundary, exclusive_budget != 0);
            let Some(entry) = entry else {
                return woke;
            };
            if entry.mode == RegistrationMode::Exclusive {
                exclusive_budget -= 1;
            }
            wake(entry.waker);
            woke += 1;
        }
    }

    fn close(&self) {
        let boundary = {
            let mut inner = self.0.lock();
            inner.closed = true;
            inner.wake_boundary()
        };
        loop {
            let entry = self.0.lock().take_next_before(boundary);
            let Some(entry) = entry else {
                return;
            };
            entry.waker.wake();
        }
    }
}

struct Registration {
    state: Arc<PollState>,
    id: RegistrationId,
    notified: Arc<AtomicBool>,
}

impl PollRegistration for Registration {
    fn was_notified(&self) -> bool {
        self.notified.load(Ordering::Acquire)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.state.unregister(self.id);
    }
}

/// A readiness source containing shared observers and exclusive consumers.
pub struct PollSet(OnceLock<Arc<PollState>>);

impl Default for PollSet {
    fn default() -> Self {
        Self::new()
    }
}

impl PollSet {
    /// Creates an empty readiness source.
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    fn state(&self) -> Arc<PollState> {
        Arc::clone(self.0.call_once(|| Arc::new(PollState::new())))
    }

    /// Wakes every matching shared observer and one matching exclusive consumer.
    ///
    /// # Safety
    ///
    /// This method is task/deferred-context only. Readiness must be published
    /// first through synchronization observed by the consumer's readiness
    /// check, and the caller must not hold a lock that a waker may re-enter.
    pub unsafe fn wake(&self, ready: IoEvents) -> usize {
        let Some(state) = self.0.get() else {
            return 0;
        };
        state.wake_with(ready, 1, &mut Waker::wake)
    }

    /// Wakes matching registrations through a caller-selected Waker policy.
    ///
    /// Every matching shared observer and one matching exclusive consumer is
    /// removed and passed by value to `wake`. The callback runs after the
    /// readiness-queue lock is released. This permits an OS adapter to attach
    /// scheduler intent such as Linux `WF_SYNC` without coupling this generic
    /// readiness crate to one task implementation.
    ///
    /// # Safety
    ///
    /// This method is task/deferred-context only. Readiness must be published
    /// first through synchronization observed by the consumer's readiness
    /// check, and the caller must not hold a lock that the callback may
    /// re-enter.
    pub unsafe fn wake_with(&self, ready: IoEvents, mut wake: impl FnMut(Waker)) -> usize {
        let Some(state) = self.0.get() else {
            return 0;
        };
        state.wake_with(ready, 1, &mut wake)
    }

    /// Wakes every matching shared observer and exclusive consumer.
    ///
    /// Use this only after publishing a permanent terminal transition.
    ///
    /// # Safety
    ///
    /// This method is task/deferred-context only. The caller must not hold a
    /// lock that a waker may re-enter. The terminal state must be published
    /// through synchronization observed by the consumer's readiness check.
    pub unsafe fn wake_all(&self, ready: IoEvents) -> usize {
        let Some(state) = self.0.get() else {
            return 0;
        };
        state.wake_with(ready, usize::MAX, &mut Waker::wake)
    }
}

impl PollSource for PollSet {
    unsafe fn register(
        &self,
        waker: &Waker,
        interests: IoEvents,
        mode: RegistrationMode,
    ) -> Option<Box<dyn PollRegistration>> {
        let state = self.state();
        state
            .register(waker, interests, mode)
            .map(|(id, notified)| {
                let registration: Box<dyn PollRegistration> = Box::new(Registration {
                    state,
                    id,
                    notified,
                });
                registration
            })
    }
}

impl Drop for PollSet {
    fn drop(&mut self) {
        if let Some(state) = self.0.get() {
            state.close();
        }
    }
}
