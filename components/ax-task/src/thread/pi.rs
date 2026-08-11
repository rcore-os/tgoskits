//! Task-local PI metadata and scheduler-owned per-lock waiter handles.

use core::sync::atomic::{AtomicU64, Ordering};

pub use crate::sync::{
    PI_MUTEX_WAIT_STORAGE_WORDS, PiMutexAcquire, PiMutexClaimOutcome, PiMutexCore, PiMutexId,
    PiMutexLockResult, PiMutexOwnedRelease, PiMutexOwnerSnapshot, PiMutexRaw, PiMutexRef,
    PiMutexStateError, PiTaskId, PiWaitCancelOutcome, PiWaitToken,
};
use crate::{
    PiWaitStateError, PiWaitTree, TaskError, ThreadId,
    lock::{RawTicketGuard, RawTicketLock},
};

impl From<ThreadId> for PiTaskId {
    fn from(thread: ThreadId) -> Self {
        Self::new(thread.as_u64()).expect("scheduler thread identity must fit the PI owner word")
    }
}

impl From<PiTaskId> for ThreadId {
    fn from(thread: PiTaskId) -> Self {
        let raw = thread.get();
        Self::from_parts(raw as u32, (raw >> 32) as u32)
    }
}

impl From<PiMutexStateError> for TaskError {
    fn from(error: PiMutexStateError) -> Self {
        match error {
            PiMutexStateError::WaiterOwnsLock => {
                Self::InvalidPiWaitState(PiWaitStateError::WaiterOwnsLock)
            }
            PiMutexStateError::InvalidState => Self::InvalidPiState,
        }
    }
}

/// One generation-checked edge from a blocked task to a physical PI mutex.
///
/// The edge is protected by the blocked task's scheduler lock, equivalent to
/// Linux `task_struct::pi_lock`. The referenced lock waiter remains protected
/// by the scheduler-owned wait handle installed in the physical mutex.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PiWaitRegistration {
    pub(crate) lock: PiMutexRaw,
    pub(crate) key: crate::PiWaitKey,
    pub(crate) generation: u64,
}

#[derive(Debug)]
pub(crate) struct PiMutexWaiters {
    pub(crate) waiters: PiWaitTree,
}

impl PiMutexWaiters {
    const fn new() -> Self {
        Self {
            waiters: PiWaitTree::new(),
        }
    }
}

/// Scheduler-owned waiter state installed lazily into one physical PI mutex.
pub(crate) struct PiMutexWaitHandle {
    state: RawTicketLock<PiMutexWaiters>,
}

impl PiMutexWaitHandle {
    fn new() -> Self {
        Self {
            state: RawTicketLock::new(PiMutexWaiters::new()),
        }
    }
}

impl Drop for PiMutexWaitHandle {
    fn drop(&mut self) {
        assert!(
            self.state.lock().waiters.is_empty(),
            "a PI mutex cannot be destroyed with live scheduler waiters"
        );
    }
}

pub(crate) fn lock_pi_mutex_waiters(lock: PiMutexRef<'_>) -> RawTicketGuard<'_, PiMutexWaiters> {
    ensure_pi_mutex_wait_handle(lock.core()).state.lock()
}

pub(crate) unsafe fn lock_raw_pi_mutex_waiters(
    lock: PiMutexRaw,
) -> RawTicketGuard<'static, PiMutexWaiters> {
    installed_pi_mutex_wait_handle(unsafe {
        // SAFETY: the caller retains the registration represented by `lock`.
        lock.core()
    })
    .state
    .lock()
}

pub(crate) unsafe fn try_lock_raw_pi_mutex_waiters(
    lock: PiMutexRaw,
) -> Option<RawTicketGuard<'static, PiMutexWaiters>> {
    installed_pi_mutex_wait_handle(unsafe {
        // SAFETY: the caller retains the registration represented by `lock`.
        lock.core()
    })
    .state
    .try_lock()
}

pub(crate) unsafe fn drop_pi_mutex_wait_handle(wait_handle: *mut ()) {
    let wait_handle = wait_handle.cast::<PiMutexWaitHandle>();
    // SAFETY: PiMutexCore transferred the unique initialized inline object
    // after its final safe reference and waiter registration became unreachable.
    unsafe { wait_handle.drop_in_place() };
}

fn ensure_pi_mutex_wait_handle(core: &PiMutexCore) -> &PiMutexWaitHandle {
    const _: () = assert!(
        core::mem::size_of::<PiMutexWaitHandle>()
            <= PI_MUTEX_WAIT_STORAGE_WORDS * core::mem::size_of::<usize>()
    );
    const _: () =
        assert!(core::mem::align_of::<PiMutexWaitHandle>() <= core::mem::align_of::<usize>());

    unsafe {
        // SAFETY: every ArceOS access to this storage uses the same concrete
        // handle type, whose size and alignment are checked above.
        core.wait_storage().get_or_init(PiMutexWaitHandle::new)
    }
}

fn installed_pi_mutex_wait_handle(core: &PiMutexCore) -> &PiMutexWaitHandle {
    unsafe {
        // SAFETY: a raw waiter registration is created only after the slow path
        // initialized this exact concrete handle type.
        core.wait_storage()
            .get::<PiMutexWaitHandle>()
            .expect("registered PI mutex has no scheduler wait handle")
    }
}

/// Task-local handshake generation for one preallocated PI waiter.
#[derive(Debug)]
pub(crate) struct PiWaitState {
    generation: AtomicU64,
    top_generation: AtomicU64,
    granted_generation: AtomicU64,
}

impl PiWaitState {
    pub(crate) const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            top_generation: AtomicU64::new(0),
            granted_generation: AtomicU64::new(0),
        }
    }

    pub(crate) fn begin(&self) -> Result<u64, TaskError> {
        self.top_generation.store(0, Ordering::Relaxed);
        self.granted_generation.store(0, Ordering::Relaxed);
        self.generation
            .try_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation.checked_add(1)
            })
            .map(|generation| generation + 1)
            .map_err(|_| TaskError::InvalidPiState)
    }

    pub(crate) fn mark_top(&self, generation: u64) -> Result<(), TaskError> {
        if self.generation.load(Ordering::Acquire) != generation
            || self.granted_generation.load(Ordering::Acquire) == generation
        {
            return Err(TaskError::InvalidPiState);
        }
        self.top_generation.store(generation, Ordering::Release);
        Ok(())
    }

    pub(crate) fn clear_top(&self, generation: u64) {
        let _ = self.top_generation.compare_exchange(
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
        self.clear_top(generation);
        self.granted_generation.store(generation, Ordering::Release);
        Ok(())
    }

    pub(crate) fn can_grant(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Acquire) == generation
            && self.granted_generation.load(Ordering::Acquire) != generation
    }

    pub(crate) fn is_granted(&self, generation: u64) -> bool {
        self.granted_generation.load(Ordering::Acquire) == generation
    }

    pub(crate) fn is_top(&self, generation: u64) -> bool {
        self.top_generation.load(Ordering::Acquire) == generation
    }
}
