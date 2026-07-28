//! Completion ownership for asynchronous remote affinity changes.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{TaskError, ThreadCore, ThreadHandle, ThreadState, WaitQueue};

/// Per-thread completion sequence shared by concurrent affinity setters.
#[derive(Debug)]
pub(crate) struct ThreadAffinityCompletion {
    completed_generation: AtomicU64,
    waiters: WaitQueue,
}

impl ThreadAffinityCompletion {
    pub(crate) const fn new(completed_generation: u64) -> Self {
        Self {
            completed_generation: AtomicU64::new(completed_generation),
            waiters: WaitQueue::new(),
        }
    }

    pub(crate) fn publish(&self, generation: u64) -> bool {
        let mut completed = self.completed_generation.load(Ordering::Acquire);
        loop {
            if completed >= generation {
                return false;
            }
            match self.completed_generation.compare_exchange_weak(
                completed,
                generation,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => completed = observed,
            }
        }
    }

    pub(crate) fn completed_generation(&self) -> u64 {
        self.completed_generation.load(Ordering::Acquire)
    }

    pub(crate) fn notify_waiters(&self) {
        self.waiters.notify_all();
    }

    fn wait_for(&self, request: &ThreadAffinityChange) -> Result<(), TaskError> {
        self.waiters
            .try_wait_until(|| request.try_result().is_some())?;
        request
            .try_result()
            .expect("affinity wait predicate resolved the request")
    }
}

/// Move-only completion for one generation of a remote affinity change.
#[derive(Debug)]
#[must_use = "dropping the change leaves the affinity update asynchronous"]
pub struct ThreadAffinityChange {
    thread: ThreadHandle,
    generation: u64,
}

impl ThreadAffinityChange {
    pub(crate) fn new(core: Arc<ThreadCore>, generation: u64) -> Self {
        Self {
            thread: ThreadHandle::from_core(core),
            generation,
        }
    }

    /// Returns the generation assigned to this affinity change.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Observes completion, target exit, or a still-pending owner transition.
    pub fn try_result(&self) -> Option<Result<(), TaskError>> {
        if self.thread.core.affinity_completion.completed_generation() >= self.generation {
            Some(Ok(()))
        } else if self.thread.state() == ThreadState::Exited {
            Some(Err(TaskError::StaleThreadId))
        } else {
            None
        }
    }

    /// Sleeps until the owner runqueue orders this generation.
    ///
    /// A later concurrent affinity update may supersede this request. In that
    /// case both requests complete after the owner has installed the latest
    /// placement, matching Linux's shared pending-affinity completion.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::StaleThreadId`] if the target exits before the
    /// generation completes, and propagates scheduler blocking failures.
    pub fn wait(self) -> Result<(), TaskError> {
        self.thread.core.affinity_completion.wait_for(&self)
    }
}
