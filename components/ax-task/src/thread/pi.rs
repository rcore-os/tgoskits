//! Priority-inheritance mutex identities and wait handshake tokens.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{TaskError, ThreadCore, ThreadId};

/// Stable identity of one kernel PI mutex.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PiLockId(usize);

impl PiLockId {
    /// Creates a PI lock identity from its stable address-sized key.
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the underlying identity key.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Token joining ax-sync's waiter grant with ax-task's parking transition.
///
/// The token retains the thread's preallocated wait state. Creating, granting,
/// cancelling, and dropping it never allocates memory.
#[must_use = "a PI wait token must be granted or explicitly cancelled"]
#[derive(Debug)]
pub struct PiWaitToken {
    pub(crate) core: Arc<ThreadCore>,
    pub(crate) generation: u64,
}

impl PiWaitToken {
    /// Returns whether ownership handoff has already selected this waiter.
    pub fn is_granted(&self) -> bool {
        self.core.pi_wait_state().is_granted(self.generation)
    }

    pub(crate) fn waiter(&self) -> ThreadId {
        self.core.id()
    }
}

#[derive(Debug)]
pub(crate) struct PiWaitState {
    generation: AtomicU64,
    granted_generation: AtomicU64,
}

impl PiWaitState {
    pub(crate) const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            granted_generation: AtomicU64::new(0),
        }
    }

    pub(crate) fn begin(&self) -> Result<u64, TaskError> {
        self.granted_generation.store(0, Ordering::Relaxed);
        self.generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation.checked_add(1)
            })
            .map(|generation| generation + 1)
            .map_err(|_| TaskError::InvalidPiState)
    }

    pub(crate) fn grant(&self, generation: u64) -> Result<(), TaskError> {
        if self.generation.load(Ordering::Acquire) != generation {
            return Err(TaskError::InvalidPiState);
        }
        self.granted_generation.store(generation, Ordering::Release);
        Ok(())
    }

    fn is_granted(&self, generation: u64) -> bool {
        self.granted_generation.load(Ordering::Acquire) == generation
    }
}
