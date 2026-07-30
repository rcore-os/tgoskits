//! Priority-inheritance mutex identities and wait handshake tokens.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{TaskError, ThreadCore, ThreadId};

static NEXT_PI_LOCK_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Stable identity of one kernel PI mutex.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PiLockId(u64);

impl PiLockId {
    /// Returns the globally unique generation allocated to this lock instance.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Lazily allocated identity owned by one physical PI lock instance.
///
/// Keeping the allocator state inside the lock makes reconstructing a lock at
/// the same address allocate a fresh generation. The generation is never
/// reused, but the physical lock owner must still quiesce all scheduler wait
/// registrations before destruction.
#[derive(Debug)]
pub struct PiLockIdentity {
    generation: AtomicU64,
}

impl PiLockIdentity {
    /// Creates an identity owner that has not yet entered the PI scheduler.
    pub const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
        }
    }

    /// Returns this lock's stable generation, allocating it on first use.
    pub fn id(&self) -> Result<PiLockId, TaskError> {
        let observed = self.generation.load(Ordering::Acquire);
        if observed != 0 {
            return Ok(PiLockId(observed));
        }

        let allocated = NEXT_PI_LOCK_GENERATION
            .try_update(Ordering::AcqRel, Ordering::Acquire, |next| {
                next.checked_add(1)
            })
            .map(PiLockId)
            .map_err(|_| TaskError::InvalidPiState)?;
        match self
            .generation
            .compare_exchange(0, allocated.0, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(allocated),
            Err(installed) if installed != 0 => Ok(PiLockId(installed)),
            Err(_) => Err(TaskError::InvalidPiState),
        }
    }
}

impl Default for PiLockIdentity {
    fn default() -> Self {
        Self::new()
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
    pub(crate) initial_owner: Option<Arc<ThreadCore>>,
    pub(crate) generation: u64,
}

impl PiWaitToken {
    /// Returns whether ownership handoff has already selected this waiter.
    pub fn is_granted(&self) -> bool {
        self.core.pi_wait_state().is_granted(self.generation)
    }

    /// Returns whether an ownerless PI mutex selected this waiter to claim.
    ///
    /// Selection is only a wake-before-block handshake. The waiter does not
    /// own the mutex until its local owner-word claim and scheduler claim
    /// transaction both complete.
    pub fn is_selected(&self) -> bool {
        self.core.pi_wait_state().is_selected(self.generation)
    }

    /// Returns the owner observed by the wait-registration transaction.
    ///
    /// A waiter which joins an already ownerless claim window has no initial
    /// owner. A later handoff may also redirect an owned waiter, so mutex owner
    /// spinning must stop as soon as its local owner word no longer names this
    /// identity.
    pub fn initial_owner(&self) -> Option<ThreadId> {
        self.initial_owner.as_ref().map(|owner| owner.id())
    }

    /// Returns whether the initially observed owner still occupies a CPU.
    ///
    /// This is a progress hint equivalent to Linux `owner_on_cpu()`, not an
    /// ownership proof. The mutex owner word remains the serialization source
    /// of truth.
    pub fn initial_owner_is_on_cpu(&self) -> bool {
        self.initial_owner
            .as_ref()
            .is_some_and(|owner| owner.sched().scheduler_fence_cpu().is_some())
    }

    pub(crate) fn waiter(&self) -> ThreadId {
        self.core.id()
    }
}

#[derive(Debug)]
pub(crate) struct PiWaitState {
    generation: AtomicU64,
    selected_generation: AtomicU64,
    granted_generation: AtomicU64,
}

impl PiWaitState {
    pub(crate) const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            selected_generation: AtomicU64::new(0),
            granted_generation: AtomicU64::new(0),
        }
    }

    pub(crate) fn begin(&self) -> Result<u64, TaskError> {
        self.selected_generation.store(0, Ordering::Relaxed);
        self.granted_generation.store(0, Ordering::Relaxed);
        self.generation
            .try_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation.checked_add(1)
            })
            .map(|generation| generation + 1)
            .map_err(|_| TaskError::InvalidPiState)
    }

    pub(crate) fn select(&self, generation: u64) -> Result<(), TaskError> {
        if self.generation.load(Ordering::Acquire) != generation
            || self.granted_generation.load(Ordering::Acquire) == generation
        {
            return Err(TaskError::InvalidPiState);
        }
        self.selected_generation
            .store(generation, Ordering::Release);
        Ok(())
    }

    pub(crate) fn clear_selection(&self, generation: u64) {
        let _ = self.selected_generation.compare_exchange(
            generation,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(crate) fn grant(&self, generation: u64) -> Result<(), TaskError> {
        if self.generation.load(Ordering::Acquire) != generation {
            return Err(TaskError::InvalidPiState);
        }
        self.clear_selection(generation);
        self.granted_generation.store(generation, Ordering::Release);
        Ok(())
    }

    pub(crate) fn can_grant(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Acquire) == generation
            && self.granted_generation.load(Ordering::Acquire) != generation
    }

    pub(crate) fn can_select(&self, generation: u64) -> bool {
        self.can_grant(generation) && self.selected_generation.load(Ordering::Acquire) != generation
    }

    fn is_granted(&self, generation: u64) -> bool {
        self.granted_generation.load(Ordering::Acquire) == generation
    }

    fn is_selected(&self, generation: u64) -> bool {
        self.selected_generation.load(Ordering::Acquire) == generation
    }
}
