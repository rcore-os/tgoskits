//! Fixed-capacity owner-CPU task-deadline storage.

mod heap;
mod kernel;
mod node;

pub use kernel::{
    KernelTimerAction, KernelTimerCallback, KernelTimerCancelOutcome, KernelTimerHandle,
    RestartableKernelTimerCallback,
};
pub(crate) use kernel::{KernelTimerEntry, KernelTimerExecution, KernelTimerQueue};
pub use node::{
    ExpiredTaskDeadline, TaskDeadlineKind, TaskDeadlineNode, TaskDeadlineRegistration,
    TaskDeadlineToken,
};

use self::{
    heap::{TimerEntry, TimerHeap},
    node::{TASK_DEADLINE_CLASS_COUNT, TaskDeadlineClass, TaskDeadlineNodeId},
};
use crate::runtime::{MonotonicDeadline, MonotonicInstant};

/// Failure returned while arming a fixed-capacity timer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TaskDeadlineError {
    /// Every preallocated heap slot is occupied by an active task deadline.
    #[error("per-CPU timer capacity is exhausted")]
    Capacity,
    /// The node identity or arm generation space has been exhausted.
    #[error("timer identity or generation space is exhausted")]
    GenerationExhausted,
    /// The typed event does not belong to the supplied embedded timer node.
    #[error("task deadline kind does not match its timer node")]
    KindMismatch,
}

/// Bounded timer-IRQ expiration request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskDeadlineExpireRequest {
    now: MonotonicInstant,
    batch_limit: usize,
}

impl TaskDeadlineExpireRequest {
    /// Creates one bounded timer expiration request.
    pub const fn new(now: MonotonicInstant, batch_limit: usize) -> Self {
        Self { now, batch_limit }
    }
}

/// Result of one bounded timer-IRQ pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskDeadlineExpireBatch {
    processed: usize,
    expired: usize,
    pending: bool,
    next_deadline: Option<MonotonicDeadline>,
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

    /// Returns the next logical task deadline.
    pub const fn next_deadline(self) -> Option<MonotonicDeadline> {
        self.next_deadline
    }
}

/// Fixed-capacity value heap created during CPU-local initialization.
///
/// Construction is the only operation that reserves memory. Arming, cancelling,
/// and expiring never grow or shrink the allocation.
#[derive(Debug)]
pub struct TaskDeadlineQueue {
    heaps: [TimerHeap; TASK_DEADLINE_CLASS_COUNT],
    capacity_per_class: usize,
}

/// Reversible removal of one task-deadline queue entry.
///
/// The owner CPU keeps this transaction while it derives and publishes the
/// replacement clockevent state. A pre-publication failure can therefore
/// restore the exact generation-bearing entry without allocating a new slot or
/// consuming another timer generation.
#[must_use = "a task-deadline cancellation must be committed or rolled back"]
pub(crate) struct TaskDeadlineCancelTxn {
    entry: TimerEntry,
}

/// Fully validated arm operation whose queue commit cannot fail.
///
/// Owner code may prepare every timer affected by one scheduler transition
/// before changing any queue entry. This is the task-deadline equivalent of
/// Linux hrtimer's prepare/enqueue split: capacity and generation failures stay
/// on the recoverable side of the scheduler commit boundary.
#[must_use = "a prepared task deadline must be committed or discarded"]
pub(crate) struct TaskDeadlineArmPlan {
    entry: TimerEntry,
    replacing: bool,
}

impl TaskDeadlineCancelTxn {
    pub(crate) const fn commit(self) {}

    pub(crate) fn rollback(self, queue: &mut TaskDeadlineQueue) {
        queue.restore_cancelled(self.entry);
    }
}

impl TaskDeadlineQueue {
    /// Preallocates `capacity` independent slots for each typed timer class.
    pub fn new(capacity: usize) -> Self {
        Self {
            heaps: core::array::from_fn(|_| TimerHeap::new(capacity)),
            capacity_per_class: capacity,
        }
    }

    fn heap(&self, class: TaskDeadlineClass) -> &TimerHeap {
        &self.heaps[class.index()]
    }

    fn heap_mut(&mut self, class: TaskDeadlineClass) -> &mut TimerHeap {
        &mut self.heaps[class.index()]
    }

    /// Arms a typed task deadline for an absolute monotonic deadline.
    ///
    /// Rearming replaces this physical node's previous entry in place. Distinct
    /// nodes for one thread remain independent, and each node consumes at most
    /// one preallocated heap slot.
    ///
    /// # Errors
    ///
    /// Returns [`TaskDeadlineError::Capacity`] without changing the queue or
    /// consuming an arm generation if no heap slot remains. A node may retain
    /// the lazily assigned identity used for this capacity check. Returns
    /// [`TaskDeadlineError::GenerationExhausted`] instead of reusing an old
    /// generation.
    ///
    /// Queue mutation must remain serialized on its owner CPU. The returned
    /// move-only registration owns the physical entry; the queue stores the
    /// thread, generation, and event kind by value and does not retain `node`.
    pub fn arm(
        &mut self,
        node: &TaskDeadlineNode,
        deadline: MonotonicDeadline,
        kind: TaskDeadlineKind,
    ) -> Result<TaskDeadlineRegistration, TaskDeadlineError> {
        let plan = self.prepare_arm(node, deadline, kind)?;
        Ok(self.commit_arm(plan))
    }

    pub(crate) fn prepare_arm(
        &self,
        node: &TaskDeadlineNode,
        deadline: MonotonicDeadline,
        kind: TaskDeadlineKind,
    ) -> Result<TaskDeadlineArmPlan, TaskDeadlineError> {
        let thread = node.thread();
        let class = kind.class();
        if node.class() != class {
            return Err(TaskDeadlineError::KindMismatch);
        }
        let identity = node.identity()?;
        let heap = self.heap(class);
        let replacing = heap.contains_node(identity);
        if heap.is_full() && !replacing {
            return Err(TaskDeadlineError::Capacity);
        }
        let token = node.next_token(identity)?;
        Ok(TaskDeadlineArmPlan {
            entry: TimerEntry::new(deadline, thread, token, kind),
            replacing,
        })
    }

    pub(crate) fn commit_arm(&mut self, plan: TaskDeadlineArmPlan) -> TaskDeadlineRegistration {
        let TaskDeadlineArmPlan { entry, replacing } = plan;
        let identity = entry.token().node();
        let heap = self.heap_mut(entry.kind().class());
        if replacing {
            let removed = heap.remove_node(identity);
            assert!(
                removed.is_some(),
                "prepared replacement must retain its physical task deadline entry"
            );
        }
        heap.push(entry);
        TaskDeadlineRegistration::new(
            entry.thread(),
            entry.token(),
            entry.deadline(),
            entry.kind(),
        )
    }

    /// Cancels one matching arm operation and immediately releases its heap slot.
    ///
    /// Unlike lazy tombstoning, physical removal releases capacity immediately
    /// and makes the registration terminal as soon as this method returns.
    pub fn cancel(&mut self, registration: &TaskDeadlineRegistration) -> bool {
        let Some(cancellation) = self.begin_cancel(registration) else {
            return false;
        };
        cancellation.commit();
        true
    }

    pub(crate) fn begin_cancel(
        &mut self,
        registration: &TaskDeadlineRegistration,
    ) -> Option<TaskDeadlineCancelTxn> {
        self.heap_mut(registration.kind().class())
            .remove(
                registration.thread(),
                registration.token(),
                registration.kind(),
            )
            .map(|entry| TaskDeadlineCancelTxn { entry })
    }

    fn restore_cancelled(&mut self, entry: TimerEntry) {
        let heap = self.heap_mut(entry.kind().class());
        assert!(
            !heap.contains_node(entry.token().node()),
            "cancelled task deadline node was reused before transaction completion"
        );
        heap.push(entry);
    }

    /// Returns the earliest logical task deadline without mutating the queue.
    pub fn next_deadline(&self) -> Option<MonotonicDeadline> {
        self.next_entry_in(&[
            TaskDeadlineClass::Park,
            TaskDeadlineClass::DeadlineCbs,
            TaskDeadlineClass::DeadlineZeroLag,
        ])
        .map(TimerEntry::deadline)
    }

    pub(crate) fn has_immediately_actionable_soft_entry(&self, now: MonotonicInstant) -> bool {
        self.heap(TaskDeadlineClass::Park)
            .peek()
            .is_some_and(|entry| now.reached(entry.deadline()))
    }

    pub(crate) fn has_immediately_actionable_scheduler_entry(&self, now: MonotonicInstant) -> bool {
        self.next_entry_in(&[
            TaskDeadlineClass::DeadlineCbs,
            TaskDeadlineClass::DeadlineZeroLag,
        ])
        .is_some_and(|entry| now.reached(entry.deadline()))
    }

    /// Expires timers into caller-provided storage without allocating or invoking
    /// callbacks.
    pub fn expire(
        &mut self,
        request: TaskDeadlineExpireRequest,
        output: &mut [ExpiredTaskDeadline],
    ) -> TaskDeadlineExpireBatch {
        self.expire_classes(
            request,
            output,
            &[
                TaskDeadlineClass::Park,
                TaskDeadlineClass::DeadlineCbs,
                TaskDeadlineClass::DeadlineZeroLag,
            ],
        )
    }

    pub(crate) fn expire_soft(
        &mut self,
        request: TaskDeadlineExpireRequest,
        output: &mut [ExpiredTaskDeadline],
    ) -> TaskDeadlineExpireBatch {
        self.expire_classes(request, output, &[TaskDeadlineClass::Park])
    }

    pub(crate) fn expire_scheduler(
        &mut self,
        request: TaskDeadlineExpireRequest,
        output: &mut [ExpiredTaskDeadline],
    ) -> TaskDeadlineExpireBatch {
        self.expire_classes(
            request,
            output,
            &[
                TaskDeadlineClass::DeadlineCbs,
                TaskDeadlineClass::DeadlineZeroLag,
            ],
        )
    }

    fn expire_classes(
        &mut self,
        request: TaskDeadlineExpireRequest,
        output: &mut [ExpiredTaskDeadline],
        classes: &[TaskDeadlineClass],
    ) -> TaskDeadlineExpireBatch {
        let mut processed = 0;
        let mut expired = 0;

        while processed < request.batch_limit {
            let Some(entry) = self.next_entry_in(classes) else {
                break;
            };
            if !request.now.reached(entry.deadline()) {
                break;
            }
            if expired == output.len() {
                break;
            }

            let entry = self
                .heap_mut(entry.kind().class())
                .pop_min()
                .expect("peek proved the fixed timer heap is non-empty");
            processed += 1;
            output[expired] = ExpiredTaskDeadline::new(
                entry.thread(),
                entry.token(),
                entry.deadline(),
                entry.kind(),
            );
            expired += 1;
        }

        let next_deadline = self.next_entry_in(classes).map(TimerEntry::deadline);
        let pending = next_deadline.is_some_and(|deadline| request.now.reached(deadline));
        TaskDeadlineExpireBatch {
            processed,
            expired,
            pending,
            next_deadline,
        }
    }

    /// Returns the preallocated entry capacity.
    pub const fn capacity(&self) -> usize {
        self.capacity_per_class
    }

    /// Returns the number of active task deadline entries in storage.
    pub fn len(&self) -> usize {
        self.heaps.iter().map(TimerHeap::len).sum()
    }

    /// Reports whether no timer entries remain.
    pub fn is_empty(&self) -> bool {
        self.heaps.iter().all(TimerHeap::is_empty)
    }

    fn next_entry_in(&self, classes: &[TaskDeadlineClass]) -> Option<TimerEntry> {
        classes
            .iter()
            .filter_map(|class| self.heap(*class).peek())
            .reduce(|earliest, candidate| {
                if candidate.precedes(earliest) {
                    candidate
                } else {
                    earliest
                }
            })
    }
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests;
