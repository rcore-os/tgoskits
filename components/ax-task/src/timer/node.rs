//! Embedded timer node and generation state.

use core::{
    marker::PhantomPinned,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
};

use super::TimerError;
use crate::ThreadId;

/// Runtime interpretation attached to one general-purpose timer arm.
///
/// Class zero is reserved for caller-drained timers and scheduler thread sleep
/// timers. A non-zero class tells the scheduler to forward expiration through
/// [`crate::runtime::TaskRuntime::dispatch_expired_timer`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeTimerOwner {
    owner: usize,
    owner_class: u64,
}

impl RuntimeTimerOwner {
    /// Creates one runtime-owned timer identity.
    ///
    /// # Safety
    ///
    /// `owner` must be non-zero and identify a pinned object that remains live
    /// until the timer is cancelled and every already-published expiration has
    /// passed a scheduler safe point. `owner_class` must be non-zero and the
    /// linked runtime must interpret it as that exact object type.
    pub const unsafe fn new(owner: usize, owner_class: u64) -> Self {
        Self { owner, owner_class }
    }

    pub(super) const fn owner(self) -> usize {
        self.owner
    }

    pub(super) const fn owner_class(self) -> u64 {
        self.owner_class
    }

    pub(crate) const fn is_valid(self) -> bool {
        self.owner != 0 && self.owner_class != 0
    }
}

/// Generation token identifying one specific timer arm operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct TimerToken(u64);

impl TimerToken {
    /// Sentinel that cannot identify a live timer arm.
    pub const NONE: Self = Self(0);

    /// Returns the monotonically assigned arm generation.
    pub const fn generation(self) -> u64 {
        self.0
    }

    /// Reconstructs a live generation stored by an owner object.
    pub const fn from_generation(generation: u64) -> Option<Self> {
        if generation == 0 {
            None
        } else {
            Some(Self(generation))
        }
    }
}

/// Timer node embedded in a thread, coroutine, or other shutdown-lived owner.
#[derive(Debug)]
pub struct TimerNode {
    owner: usize,
    owner_thread: u64,
    sequence: AtomicU64,
    active_generation: AtomicU64,
    _pin: PhantomPinned,
}

impl TimerNode {
    /// Creates a detached timer node with caller-defined owner data.
    pub const fn new(owner: usize) -> Self {
        Self {
            owner,
            owner_thread: 0,
            sequence: AtomicU64::new(0),
            active_generation: AtomicU64::new(0),
            _pin: PhantomPinned,
        }
    }

    /// Creates a timer node owned by one generation-checked scheduler thread.
    pub(crate) const fn for_thread(thread: ThreadId) -> Self {
        Self {
            owner: thread.slot() as usize,
            owner_thread: thread.as_u64(),
            sequence: AtomicU64::new(0),
            active_generation: AtomicU64::new(0),
            _pin: PhantomPinned,
        }
    }

    /// Cancels the matching arm operation by publishing a generation tombstone.
    pub fn cancel(self: Pin<&Self>, token: TimerToken) -> bool {
        self.active_generation
            .compare_exchange(token.0, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(super) fn next_token(&self) -> Result<TimerToken, TimerError> {
        let mut sequence = self.sequence.load(Ordering::Relaxed);
        loop {
            if sequence == u64::MAX {
                return Err(TimerError::GenerationExhausted);
            }
            match self.sequence.compare_exchange_weak(
                sequence,
                sequence + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(TimerToken(sequence + 1)),
                Err(updated) => sequence = updated,
            }
        }
    }

    pub(super) fn activate(&self, token: TimerToken) {
        self.active_generation.store(token.0, Ordering::Release);
    }

    pub(super) fn is_active(&self, token: TimerToken) -> bool {
        self.active_generation.load(Ordering::Acquire) == token.0
    }

    pub(super) const fn owner(&self) -> usize {
        self.owner
    }

    pub(super) const fn is_thread_owned(&self) -> bool {
        self.owner_thread != 0
    }

    pub(super) fn try_expire(
        &self,
        token: TimerToken,
        deadline_ns: u64,
        owner: usize,
        owner_class: u64,
    ) -> Option<ExpiredTimer> {
        self.active_generation
            .compare_exchange(token.0, 0, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ExpiredTimer {
                owner,
                node: (self as *const Self).expose_provenance(),
                owner_class,
                owner_thread: self.owner_thread,
                token,
                deadline_ns,
                valid: true,
            })
    }
}

/// Allocation-free timer expiration copied into caller-owned IRQ storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpiredTimer {
    owner: usize,
    node: usize,
    owner_class: u64,
    owner_thread: u64,
    token: TimerToken,
    deadline_ns: u64,
    valid: bool,
}

impl ExpiredTimer {
    /// Empty value used to initialize fixed output arrays.
    pub const EMPTY: Self = Self {
        owner: 0,
        node: 0,
        owner_class: 0,
        owner_thread: 0,
        token: TimerToken::NONE,
        deadline_ns: 0,
        valid: false,
    };

    /// Returns the caller-defined embedded-node owner data.
    pub const fn owner(self) -> usize {
        self.owner
    }

    /// Returns the pinned timer-node address carried by this expiration.
    pub const fn node(self) -> usize {
        self.node
    }

    /// Returns the runtime-defined class, or zero for caller-drained timers.
    pub const fn owner_class(self) -> u64 {
        self.owner_class
    }

    /// Returns the scheduler thread owning an embedded sleep timer.
    pub const fn owner_thread(self) -> Option<ThreadId> {
        if self.owner_thread == 0 {
            None
        } else {
            Some(ThreadId::from_parts(
                self.owner_thread as u32,
                (self.owner_thread >> 32) as u32,
            ))
        }
    }

    /// Returns the generation that reached expiration.
    pub const fn token(self) -> TimerToken {
        self.token
    }

    /// Returns the absolute requested deadline.
    pub const fn deadline_ns(self) -> u64 {
        self.deadline_ns
    }

    /// Reports whether this value was written by an expiration pass.
    pub const fn is_valid(self) -> bool {
        self.valid
    }
}
