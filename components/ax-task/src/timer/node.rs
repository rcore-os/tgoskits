//! Task-deadline identity and generation state.

use core::sync::atomic::{AtomicU64, Ordering};

use super::TaskDeadlineError;
use crate::{ThreadId, runtime::MonotonicDeadline};

pub(super) const TASK_DEADLINE_CLASS_COUNT: usize = 3;
static NEXT_TASK_DEADLINE_NODE_ID: AtomicU64 = AtomicU64::new(1);

/// Process-lifetime identity assigned lazily to one physical timer node.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub(super) struct TaskDeadlineNodeId(u64);

impl TaskDeadlineNodeId {
    const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub(super) const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Independent physical timer slot owned by one scheduler thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(super) enum TaskDeadlineClass {
    Park            = 0,
    DeadlineCbs     = 1,
    DeadlineZeroLag = 2,
}

impl TaskDeadlineClass {
    pub(super) const fn index(self) -> usize {
        self as usize
    }
}

/// Node identity and generation identifying one task-deadline arm operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskDeadlineToken {
    node: TaskDeadlineNodeId,
    generation: u64,
}

impl TaskDeadlineToken {
    /// Sentinel that cannot identify a live task-deadline arm.
    pub const NONE: Self = Self {
        node: TaskDeadlineNodeId::from_raw(0),
        generation: 0,
    };

    /// Returns the monotonically assigned arm generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    const fn new(node: TaskDeadlineNodeId, generation: u64) -> Self {
        Self { node, generation }
    }

    pub(super) const fn node(self) -> TaskDeadlineNodeId {
        self.node
    }
}

/// Task-deadline node embedded in one generation-checked scheduler thread.
#[derive(Debug)]
pub struct TaskDeadlineNode {
    thread: ThreadId,
    class: TaskDeadlineClass,
    identity: AtomicU64,
    sequence: AtomicU64,
}

impl TaskDeadlineNode {
    /// Creates a deadline node owned by one generation-checked scheduler thread.
    pub const fn for_thread(thread: ThreadId) -> Self {
        Self::new(thread, TaskDeadlineClass::Park)
    }

    pub(crate) const fn deadline_cbs_for_thread(thread: ThreadId) -> Self {
        Self::new(thread, TaskDeadlineClass::DeadlineCbs)
    }

    pub(crate) const fn deadline_zero_lag_for_thread(thread: ThreadId) -> Self {
        Self::new(thread, TaskDeadlineClass::DeadlineZeroLag)
    }

    const fn new(thread: ThreadId, class: TaskDeadlineClass) -> Self {
        Self {
            thread,
            class,
            identity: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
        }
    }

    pub(super) const fn thread(&self) -> ThreadId {
        self.thread
    }

    pub(super) const fn class(&self) -> TaskDeadlineClass {
        self.class
    }

    pub(super) fn identity(&self) -> Result<TaskDeadlineNodeId, TaskDeadlineError> {
        let identity = self.identity.load(Ordering::Acquire);
        if identity != 0 {
            return Ok(TaskDeadlineNodeId::from_raw(identity));
        }

        let mut candidate = NEXT_TASK_DEADLINE_NODE_ID.load(Ordering::Relaxed);
        loop {
            if candidate == u64::MAX {
                return Err(TaskDeadlineError::GenerationExhausted);
            }
            match NEXT_TASK_DEADLINE_NODE_ID.compare_exchange_weak(
                candidate,
                candidate + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return match self.identity.compare_exchange(
                        0,
                        candidate,
                        Ordering::Release,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => Ok(TaskDeadlineNodeId::from_raw(candidate)),
                        Err(published) => Ok(TaskDeadlineNodeId::from_raw(published)),
                    };
                }
                Err(updated) => candidate = updated,
            }
        }
    }

    pub(super) fn next_token(
        &self,
        identity: TaskDeadlineNodeId,
    ) -> Result<TaskDeadlineToken, TaskDeadlineError> {
        debug_assert_eq!(self.identity.load(Ordering::Relaxed), identity.as_u64());
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
                Ok(_) => return Ok(TaskDeadlineToken::new(identity, sequence + 1)),
                Err(updated) => sequence = updated,
            }
        }
    }
}

/// Scheduler-owned meaning of one task deadline.
///
/// The queue deliberately has no arbitrary callback variant. Every entry is a
/// value-owned scheduler identity that can be validated again at a safe point.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskDeadlineKind {
    /// Timeout for one generation of the thread park handshake.
    ParkTimeout { park_generation: u64 },
    /// CBS deadline miss or replenishment boundary.
    DeadlineCbs,
    /// GRUB inactive-bandwidth transition at zero lag.
    DeadlineZeroLag,
}

impl TaskDeadlineKind {
    /// Creates a timeout for one generation of a move-only park ticket.
    pub const fn park_timeout(park_generation: u64) -> Self {
        Self::ParkTimeout { park_generation }
    }

    /// Returns the park generation carried by this deadline, when applicable.
    pub const fn park_generation(self) -> Option<u64> {
        match self {
            Self::ParkTimeout { park_generation } => Some(park_generation),
            Self::DeadlineCbs | Self::DeadlineZeroLag => None,
        }
    }

    pub(super) const fn class(self) -> TaskDeadlineClass {
        match self {
            Self::ParkTimeout { .. } => TaskDeadlineClass::Park,
            Self::DeadlineCbs => TaskDeadlineClass::DeadlineCbs,
            Self::DeadlineZeroLag => TaskDeadlineClass::DeadlineZeroLag,
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
    deadline: MonotonicDeadline,
    kind: TaskDeadlineKind,
}

impl TaskDeadlineRegistration {
    pub(super) const fn new(
        thread: ThreadId,
        token: TaskDeadlineToken,
        deadline: MonotonicDeadline,
        kind: TaskDeadlineKind,
    ) -> Self {
        Self {
            thread,
            token,
            deadline,
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

    /// Returns the absolute monotonic deadline owned by this registration.
    pub const fn deadline(&self) -> MonotonicDeadline {
        self.deadline
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
    deadline: MonotonicDeadline,
    valid: bool,
    kind: TaskDeadlineKind,
}

impl ExpiredTaskDeadline {
    /// Empty value used to initialize fixed output arrays.
    pub const EMPTY: Self = Self {
        thread: ThreadId::from_parts(0, 0),
        token: TaskDeadlineToken::NONE,
        deadline: MonotonicDeadline::ORIGIN,
        valid: false,
        kind: TaskDeadlineKind::ParkTimeout { park_generation: 0 },
    };

    pub(super) const fn new(
        thread: ThreadId,
        token: TaskDeadlineToken,
        deadline: MonotonicDeadline,
        kind: TaskDeadlineKind,
    ) -> Self {
        Self {
            thread,
            token,
            deadline,
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
    pub const fn deadline(self) -> Option<MonotonicDeadline> {
        if self.valid {
            Some(self.deadline)
        } else {
            None
        }
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
