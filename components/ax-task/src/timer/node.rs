//! Task-deadline identity and generation state.

use core::sync::atomic::{AtomicU64, Ordering};

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
}

impl TaskDeadlineNode {
    /// Creates a deadline node owned by one generation-checked scheduler thread.
    pub const fn for_thread(thread: ThreadId) -> Self {
        Self {
            thread,
            sequence: AtomicU64::new(0),
        }
    }

    pub(super) const fn thread(&self) -> ThreadId {
        self.thread
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
}

/// Scheduler-owned meaning of one task deadline.
///
/// The queue deliberately has no arbitrary callback variant. Every entry is a
/// value-owned scheduler identity that can be validated again at a safe point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskDeadlineKind {
    /// Timeout for one generation of the thread park handshake.
    ParkTimeout { park_generation: u64 },
}

impl TaskDeadlineKind {
    /// Creates a timeout for one generation of a move-only park ticket.
    pub const fn park_timeout(park_generation: u64) -> Self {
        Self::ParkTimeout { park_generation }
    }

    /// Returns the park generation carried by this deadline.
    pub const fn park_generation(self) -> u64 {
        match self {
            Self::ParkTimeout { park_generation } => park_generation,
        }
    }
}

/// Move-only ownership of one physical task-deadline queue registration.
///
/// This type intentionally does not implement [`Copy`] or [`Clone`]. A failed
/// owner-CPU check may borrow it and retry, while successful cancellation or
/// expiration consumes the one queue entry identified by its generation.
#[must_use = "a task-deadline registration must remain owned until cancellation or expiration"]
#[derive(Debug, Eq, PartialEq)]
pub struct TaskDeadlineRegistration {
    thread: ThreadId,
    token: TaskDeadlineToken,
    kind: TaskDeadlineKind,
}

impl TaskDeadlineRegistration {
    pub(super) const fn new(
        thread: ThreadId,
        token: TaskDeadlineToken,
        kind: TaskDeadlineKind,
    ) -> Self {
        Self {
            thread,
            token,
            kind,
        }
    }

    /// Returns the generation-bearing thread identity.
    pub const fn thread(&self) -> ThreadId {
        self.thread
    }

    /// Returns the arm generation owned by this registration.
    pub const fn token(&self) -> TaskDeadlineToken {
        self.token
    }

    /// Returns the typed scheduler event.
    pub const fn kind(&self) -> TaskDeadlineKind {
        self.kind
    }
}

/// Allocation-free task expiration copied into caller-owned IRQ storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpiredTaskDeadline {
    thread: ThreadId,
    token: TaskDeadlineToken,
    deadline_ns: u64,
    valid: bool,
    kind: TaskDeadlineKind,
}

impl ExpiredTaskDeadline {
    /// Empty value used to initialize fixed output arrays.
    pub const EMPTY: Self = Self {
        thread: ThreadId::from_parts(0, 0),
        token: TaskDeadlineToken::NONE,
        deadline_ns: 0,
        valid: false,
        kind: TaskDeadlineKind::ParkTimeout { park_generation: 0 },
    };

    pub(super) const fn new(
        thread: ThreadId,
        token: TaskDeadlineToken,
        deadline_ns: u64,
        kind: TaskDeadlineKind,
    ) -> Self {
        Self {
            thread,
            token,
            deadline_ns,
            valid: true,
            kind,
        }
    }

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

    /// Returns the typed scheduler event, or `None` for an empty buffer slot.
    pub const fn kind(self) -> Option<TaskDeadlineKind> {
        if self.valid { Some(self.kind) } else { None }
    }

    /// Reports whether this value was written by an expiration pass.
    pub const fn is_valid(self) -> bool {
        self.valid
    }
}
