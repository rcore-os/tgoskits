//! Fixed-capacity owner-CPU timer storage.

mod heap;
mod node;

pub use node::{ExpiredTimer, RuntimeTimerOwner, TimerNode, TimerToken};

use self::heap::{TimerEntry, TimerHeap};

/// Failure returned while arming a fixed-capacity timer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TimerError {
    /// Every preallocated heap slot is occupied, including tombstones.
    #[error("per-CPU timer capacity is exhausted")]
    Capacity,
    /// The node's generation space has been exhausted.
    #[error("timer generation space is exhausted")]
    GenerationExhausted,
    /// The runtime identity is zero or the node belongs to a scheduler sleep timer.
    #[error("timer node ownership is incompatible with the requested arm")]
    InvalidOwner,
}

/// Bounded timer-IRQ expiration request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpireRequest {
    now_ns: u64,
    batch_limit: usize,
    timer_resolution_ns: u64,
}

impl ExpireRequest {
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
pub struct ExpireBatch {
    processed: usize,
    expired: usize,
    pending: bool,
    next_deadline_ns: Option<u64>,
}

impl ExpireBatch {
    /// Returns heap nodes inspected or removed, including tombstones.
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
pub struct TimerQueue {
    heap: TimerHeap,
}

impl TimerQueue {
    /// Preallocates exactly `capacity` timer-entry slots.
    pub fn new(capacity: usize) -> Self {
        Self {
            heap: TimerHeap::new(capacity),
        }
    }

    /// Arms an embedded timer node for an absolute monotonic deadline.
    ///
    /// Rearming a node invalidates its previous entry by generation. The stale
    /// entry remains a bounded tombstone until an expiration pass reaches it.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::Capacity`] without changing the node if no heap slot
    /// remains. Returns [`TimerError::GenerationExhausted`] instead of reusing an
    /// old generation.
    ///
    /// # Safety
    ///
    /// `node` must remain pinned and allocated until every entry referring to it
    /// has been removed from this queue. The caller must serialize queue mutation
    /// on its owner CPU.
    pub unsafe fn arm(
        &mut self,
        node: core::pin::Pin<&TimerNode>,
        deadline_ns: u64,
    ) -> Result<TimerToken, TimerError> {
        unsafe {
            // SAFETY: forwarded caller contract keeps `node` pinned until the
            // entry is removed. Class zero preserves caller-drained semantics.
            self.arm_entry(node, deadline_ns, node.owner(), 0)
        }
    }

    /// Arms an embedded node whose expiration belongs to the OS runtime.
    ///
    /// Unlike class-zero timers, this expiration is forwarded through the
    /// value-only TaskRuntime hook at the next scheduler safe point.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::InvalidOwner`] when the runtime identity is zero
    /// or `node` is reserved for a scheduler-thread sleep timer. Capacity and
    /// generation failures have the same meaning as [`Self::arm`].
    ///
    /// # Safety
    ///
    /// In addition to [`Self::arm`]'s pinning and owner-CPU requirements,
    /// `owner` must satisfy [`RuntimeTimerOwner::new`]'s lifetime and type
    /// contract until cancellation and safe-point delivery are complete.
    pub unsafe fn arm_runtime(
        &mut self,
        node: core::pin::Pin<&TimerNode>,
        deadline_ns: u64,
        owner: RuntimeTimerOwner,
    ) -> Result<TimerToken, TimerError> {
        if !owner.is_valid() || node.is_thread_owned() {
            return Err(TimerError::InvalidOwner);
        }
        unsafe {
            // SAFETY: the caller supplies both the pinned node lifetime and
            // runtime-owner lifetime required by the new heap entry.
            self.arm_entry(node, deadline_ns, owner.owner(), owner.owner_class())
        }
    }

    unsafe fn arm_entry(
        &mut self,
        node: core::pin::Pin<&TimerNode>,
        deadline_ns: u64,
        owner: usize,
        owner_class: u64,
    ) -> Result<TimerToken, TimerError> {
        if self.heap.is_full() {
            return Err(TimerError::Capacity);
        }
        let token = node.next_token()?;
        node.activate(token);
        let entry = TimerEntry::new(
            deadline_ns,
            token,
            node.get_ref() as *const TimerNode,
            owner,
            owner_class,
        );
        self.heap.push(entry);
        Ok(token)
    }

    /// Cancels one matching arm operation and immediately releases its heap slot.
    ///
    /// Unlike lazy tombstoning, physical removal lets an owner finish and release
    /// its embedded timer node as soon as this method returns.
    pub fn cancel(&mut self, node: core::pin::Pin<&TimerNode>, token: TimerToken) -> bool {
        let node_ptr = node.get_ref() as *const TimerNode;
        let was_active = node.cancel(token);
        let was_queued = self.heap.remove(node_ptr, token).is_some();
        was_active || was_queued
    }

    /// Returns the earliest representable one-shot deadline without mutating the queue.
    pub fn next_deadline_ns(&self, now_ns: u64, timer_resolution_ns: u64) -> Option<u64> {
        self.next_wakeup(ExpireRequest::new(now_ns, 0, timer_resolution_ns))
            .1
    }

    /// Expires timers into caller-provided storage without allocating or invoking
    /// callbacks.
    pub fn expire(&mut self, request: ExpireRequest, output: &mut [ExpiredTimer]) -> ExpireBatch {
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
                (*entry.node()).try_expire(
                    entry.token(),
                    entry.deadline_ns(),
                    entry.owner(),
                    entry.owner_class(),
                )
            };
            if let Some(event) = event {
                output[expired] = event;
                expired += 1;
            }
        }

        let (pending, next_deadline_ns) = self.next_wakeup(request);
        ExpireBatch {
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

    /// Returns the number of live entries and generation tombstones in storage.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Reports whether no timer entries remain.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Reports whether the heap root still requires immediate owner service.
    pub(crate) fn has_immediately_actionable(&self, now_ns: u64) -> bool {
        let Some(entry) = self.heap.peek() else {
            return false;
        };
        let live = unsafe {
            // Entries retain valid pinned nodes until removal from the heap.
            (*entry.node()).is_active(entry.token())
        };
        !live || entry.deadline_ns() <= now_ns
    }

    fn next_wakeup(&self, request: ExpireRequest) -> (bool, Option<u64>) {
        let Some(entry) = self.heap.peek() else {
            return (false, None);
        };
        let earliest = request
            .now_ns
            .saturating_add(request.timer_resolution_ns.max(1));
        if self.has_immediately_actionable(request.now_ns) {
            (true, Some(earliest))
        } else {
            (false, Some(entry.deadline_ns().max(earliest)))
        }
    }
}

#[cfg(test)]
mod tests;
