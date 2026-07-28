//! Fixed-capacity owner-CPU task-deadline storage.

mod heap;
mod node;

pub use node::{
    ExpiredTaskDeadline, TaskDeadlineKind, TaskDeadlineNode, TaskDeadlineRegistration,
    TaskDeadlineToken,
};

use self::heap::{TimerEntry, TimerHeap};

/// Failure returned while arming a fixed-capacity timer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TaskDeadlineError {
    /// Every preallocated heap slot is occupied by an active task deadline.
    #[error("per-CPU timer capacity is exhausted")]
    Capacity,
    /// `u64::MAX` represents no finite task deadline and cannot be queued.
    #[error("task deadline is not finite")]
    InvalidDeadline,
    /// The node's generation space has been exhausted.
    #[error("timer generation space is exhausted")]
    GenerationExhausted,
}

/// Absolute task deadline that is finite in the monotonic-clock domain.
///
/// Zero remains a valid, immediately due logical deadline. `u64::MAX` is the
/// explicit no-deadline sentinel and is rejected before a heap slot or
/// generation is consumed.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub(super) struct FiniteTaskDeadline(u64);

impl FiniteTaskDeadline {
    const fn from_nanos(deadline_ns: u64) -> Option<Self> {
        if deadline_ns == u64::MAX {
            None
        } else {
            Some(Self(deadline_ns))
        }
    }

    pub(super) const fn as_nanos(self) -> u64 {
        self.0
    }
}

/// Bounded timer-IRQ expiration request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskDeadlineExpireRequest {
    now_ns: u64,
    batch_limit: usize,
    timer_resolution_ns: u64,
}

impl TaskDeadlineExpireRequest {
    /// Creates one bounded timer expiration request.
    pub const fn new(now_ns: u64, batch_limit: usize, timer_resolution_ns: u64) -> Self {
        Self {
            now_ns,
            batch_limit,
            timer_resolution_ns,
        }
    }
}

/// Result of one bounded timer-IRQ pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskDeadlineExpireBatch {
    processed: usize,
    expired: usize,
    pending: bool,
    next_deadline_ns: Option<u64>,
}

impl TaskDeadlineExpireBatch {
    /// Returns heap nodes removed during this pass.
    pub const fn processed(self) -> usize {
        self.processed
    }

    /// Returns valid expirations written into the caller's output storage.
    pub const fn expired(self) -> usize {
        self.expired
    }

    /// Reports that immediately actionable work remains after the batch.
    pub const fn pending(self) -> bool {
        self.pending
    }

    /// Returns the next representable one-shot timer deadline.
    pub const fn next_deadline_ns(self) -> Option<u64> {
        self.next_deadline_ns
    }
}

/// Fixed-capacity value heap created during CPU-local initialization.
///
/// Construction is the only operation that reserves memory. Arming, cancelling,
/// and expiring never grow or shrink the allocation.
#[derive(Debug)]
pub struct TaskDeadlineQueue {
    heap: TimerHeap,
}

impl TaskDeadlineQueue {
    /// Preallocates exactly `capacity` timer-entry slots.
    pub fn new(capacity: usize) -> Self {
        Self {
            heap: TimerHeap::new(capacity),
        }
    }

    /// Arms a typed task deadline for an absolute monotonic deadline.
    ///
    /// Rearming replaces the node's previous entry in place. A node therefore
    /// consumes at most one preallocated heap slot.
    ///
    /// # Errors
    ///
    /// Returns [`TaskDeadlineError::InvalidDeadline`] before consuming a heap
    /// slot or generation when `deadline_ns` is the no-deadline sentinel.
    /// Returns [`TaskDeadlineError::Capacity`] without changing the node if no
    /// heap slot remains. Returns
    /// [`TaskDeadlineError::GenerationExhausted`] instead of reusing an old
    /// generation.
    ///
    /// Queue mutation must remain serialized on its owner CPU. The returned
    /// move-only registration owns the physical entry; the queue stores the
    /// thread, generation, and event kind by value and does not retain `node`.
    pub fn arm(
        &mut self,
        node: &TaskDeadlineNode,
        deadline_ns: u64,
        kind: TaskDeadlineKind,
    ) -> Result<TaskDeadlineRegistration, TaskDeadlineError> {
        let deadline = FiniteTaskDeadline::from_nanos(deadline_ns)
            .ok_or(TaskDeadlineError::InvalidDeadline)?;
        let thread = node.thread();
        let replacing = self.heap.contains_thread(thread);
        if self.heap.is_full() && !replacing {
            return Err(TaskDeadlineError::Capacity);
        }
        let token = node.next_token()?;
        if replacing {
            let removed = self.heap.remove_thread(thread);
            debug_assert!(
                removed.is_some(),
                "contains_thread proved the task deadline entry exists"
            );
        }
        let entry = TimerEntry::new(deadline, thread, token, kind);
        self.heap.push(entry);
        Ok(TaskDeadlineRegistration::new(thread, token, kind))
    }

    /// Cancels one matching arm operation and immediately releases its heap slot.
    ///
    /// Unlike lazy tombstoning, physical removal releases capacity immediately
    /// and makes the registration terminal as soon as this method returns.
    pub fn cancel(&mut self, registration: &TaskDeadlineRegistration) -> bool {
        self.heap
            .remove(
                registration.thread(),
                registration.token(),
                registration.kind(),
            )
            .is_some()
    }

    /// Returns the earliest representable one-shot deadline without mutating the queue.
    pub fn next_deadline_ns(&self, now_ns: u64, timer_resolution_ns: u64) -> Option<u64> {
        self.next_wakeup(TaskDeadlineExpireRequest::new(
            now_ns,
            0,
            timer_resolution_ns,
        ))
        .1
    }

    pub(crate) fn has_immediately_actionable_entry(&self, now_ns: u64) -> bool {
        let Some(entry) = self.heap.peek() else {
            return false;
        };
        entry.deadline_ns() <= now_ns
    }

    /// Expires timers into caller-provided storage without allocating or invoking
    /// callbacks.
    pub fn expire(
        &mut self,
        request: TaskDeadlineExpireRequest,
        output: &mut [ExpiredTaskDeadline],
    ) -> TaskDeadlineExpireBatch {
        let mut processed = 0;
        let mut expired = 0;

        while processed < request.batch_limit {
            let Some(entry) = self.heap.peek() else {
                break;
            };
            if entry.deadline_ns() > request.now_ns {
                break;
            }
            if expired == output.len() {
                break;
            }

            let entry = self
                .heap
                .pop_min()
                .expect("peek proved the fixed timer heap is non-empty");
            processed += 1;
            output[expired] = ExpiredTaskDeadline::new(
                entry.thread(),
                entry.token(),
                entry.deadline_ns(),
                entry.kind(),
            );
            expired += 1;
        }

        let (pending, next_deadline_ns) = self.next_wakeup(request);
        TaskDeadlineExpireBatch {
            processed,
            expired,
            pending,
            next_deadline_ns,
        }
    }

    /// Returns the preallocated entry capacity.
    pub const fn capacity(&self) -> usize {
        self.heap.capacity()
    }

    /// Returns the number of active task deadline entries in storage.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Reports whether no timer entries remain.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    fn next_wakeup(&self, request: TaskDeadlineExpireRequest) -> (bool, Option<u64>) {
        let Some(entry) = self.heap.peek() else {
            return (false, None);
        };
        let immediately_actionable = entry.deadline_ns() <= request.now_ns;
        let earliest = request
            .now_ns
            .saturating_add(request.timer_resolution_ns.max(1));
        if immediately_actionable {
            (true, Some(earliest))
        } else {
            (false, Some(entry.deadline_ns().max(earliest)))
        }
    }
}

#[cfg(test)]
mod tests;
