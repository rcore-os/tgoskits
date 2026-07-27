//! Fixed-capacity owner-CPU task-deadline storage.

mod heap;
mod node;

pub use node::{ExpiredTaskDeadline, TaskDeadlineNode, TaskDeadlineToken};

use self::heap::{TimerEntry, TimerHeap};

/// Failure returned while arming a fixed-capacity timer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TaskDeadlineError {
    /// Every preallocated heap slot is occupied by an active task deadline.
    #[error("per-CPU timer capacity is exhausted")]
    Capacity,
    /// The node's generation space has been exhausted.
    #[error("timer generation space is exhausted")]
    GenerationExhausted,
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

/// Fixed-capacity pointer heap created during CPU-local initialization.
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

    /// Arms an embedded timer node for an absolute monotonic deadline.
    ///
    /// Rearming replaces the node's previous entry in place. A node therefore
    /// consumes at most one preallocated heap slot.
    ///
    /// # Errors
    ///
    /// Returns [`TaskDeadlineError::Capacity`] without changing the node if no
    /// heap slot remains. Returns
    /// [`TaskDeadlineError::GenerationExhausted`] instead of reusing an old
    /// generation.
    ///
    /// # Safety
    ///
    /// `node` must remain pinned and allocated until every entry referring to it
    /// has been removed from this queue. The caller must serialize queue mutation
    /// on its owner CPU.
    pub unsafe fn arm(
        &mut self,
        node: core::pin::Pin<&TaskDeadlineNode>,
        deadline_ns: u64,
    ) -> Result<TaskDeadlineToken, TaskDeadlineError> {
        let node_ptr = node.get_ref() as *const TaskDeadlineNode;
        let replacing = self.heap.contains_node(node_ptr);
        if self.heap.is_full() && !replacing {
            return Err(TaskDeadlineError::Capacity);
        }
        let token = node.next_token()?;
        if replacing {
            let removed = self.heap.remove_node(node_ptr);
            debug_assert!(
                removed.is_some(),
                "contains_node proved the task deadline entry exists"
            );
        }
        node.activate(token);
        let entry = TimerEntry::new(deadline_ns, token, node_ptr);
        self.heap.push(entry);
        Ok(token)
    }

    /// Cancels one matching arm operation and immediately releases its heap slot.
    ///
    /// Unlike lazy tombstoning, physical removal lets an owner finish and release
    /// its embedded timer node as soon as this method returns.
    pub fn cancel(
        &mut self,
        node: core::pin::Pin<&TaskDeadlineNode>,
        token: TaskDeadlineToken,
    ) -> bool {
        let node_ptr = node.get_ref() as *const TaskDeadlineNode;
        let was_active = node.cancel(token);
        let was_queued = self.heap.remove(node_ptr, token).is_some();
        was_active || was_queued
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
        let live = unsafe {
            // Entries retain valid pinned nodes until owner-side removal.
            (*entry.node()).is_active(entry.token())
        };
        !live || entry.deadline_ns() <= now_ns
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
            let live = unsafe {
                // Queue construction requires every pointer to remain pinned until
                // its corresponding entry is removed.
                (*entry.node()).is_active(entry.token())
            };
            if live && entry.deadline_ns() > request.now_ns {
                break;
            }
            if live && expired == output.len() {
                break;
            }

            let entry = self
                .heap
                .pop_min()
                .expect("peek proved the fixed timer heap is non-empty");
            processed += 1;
            let event = unsafe {
                // The popped entry still owns its pinned pointer; `try_expire`
                // atomically rejects a concurrent cancellation or rearm.
                (*entry.node()).try_expire(entry.token(), entry.deadline_ns())
            };
            if let Some(event) = event {
                output[expired] = event;
                expired += 1;
            }
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
        let live = unsafe {
            // Entries retain valid pinned nodes until removal from the heap.
            (*entry.node()).is_active(entry.token())
        };
        let immediately_actionable = !live || entry.deadline_ns() <= request.now_ns;
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
