//! Embedded task-deadline node and generation state.

use core::{
    marker::PhantomPinned,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
};

use super::TaskDeadlineError;
use crate::ThreadId;

/// Generation token identifying one specific task-deadline arm operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct TaskDeadlineToken(u64);

impl TaskDeadlineToken {
    /// Sentinel that cannot identify a live task-deadline arm.
    pub const NONE: Self = Self(0);

    /// Returns the monotonically assigned arm generation.
    pub const fn generation(self) -> u64 {
        self.0
    }
}

/// Task-deadline node embedded in one generation-checked scheduler thread.
#[derive(Debug)]
pub struct TaskDeadlineNode {
    thread: ThreadId,
    sequence: AtomicU64,
    active_generation: AtomicU64,
    _pin: PhantomPinned,
}

impl TaskDeadlineNode {
    /// Creates a deadline node owned by one generation-checked scheduler thread.
    pub const fn for_thread(thread: ThreadId) -> Self {
        Self {
            thread,
            sequence: AtomicU64::new(0),
            active_generation: AtomicU64::new(0),
            _pin: PhantomPinned,
        }
    }

    /// Cancels the matching arm operation before its queue entry is removed.
    pub(super) fn cancel(self: Pin<&Self>, token: TaskDeadlineToken) -> bool {
        if token == TaskDeadlineToken::NONE {
            return false;
        }
        self.active_generation
            .compare_exchange(token.0, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(super) fn next_token(&self) -> Result<TaskDeadlineToken, TaskDeadlineError> {
        let mut sequence = self.sequence.load(Ordering::Relaxed);
        loop {
            if sequence == u64::MAX {
                return Err(TaskDeadlineError::GenerationExhausted);
            }
            match self.sequence.compare_exchange_weak(
                sequence,
                sequence + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(TaskDeadlineToken(sequence + 1)),
                Err(updated) => sequence = updated,
            }
        }
    }

    pub(super) fn activate(&self, token: TaskDeadlineToken) {
        self.active_generation.store(token.0, Ordering::Release);
    }

    pub(super) fn is_active(&self, token: TaskDeadlineToken) -> bool {
        self.active_generation.load(Ordering::Acquire) == token.0
    }

    pub(super) fn try_expire(
        &self,
        token: TaskDeadlineToken,
        deadline_ns: u64,
    ) -> Option<ExpiredTaskDeadline> {
        self.active_generation
            .compare_exchange(token.0, 0, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ExpiredTaskDeadline {
                thread: self.thread,
                token,
                deadline_ns,
                valid: true,
            })
    }
}

/// Allocation-free task expiration copied into caller-owned IRQ storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpiredTaskDeadline {
    thread: ThreadId,
    token: TaskDeadlineToken,
    deadline_ns: u64,
    valid: bool,
}

impl ExpiredTaskDeadline {
    /// Empty value used to initialize fixed output arrays.
    pub const EMPTY: Self = Self {
        thread: ThreadId::from_parts(0, 0),
        token: TaskDeadlineToken::NONE,
        deadline_ns: 0,
        valid: false,
    };

    /// Returns the generation-checked thread owning this deadline.
    pub const fn thread(self) -> Option<ThreadId> {
        if self.valid { Some(self.thread) } else { None }
    }

    /// Returns the generation that reached expiration.
    pub const fn token(self) -> TaskDeadlineToken {
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
