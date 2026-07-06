//! Task wake handles.

use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::{TaskCore, TaskId, TaskRef, TaskState};

/// Coalesced wake bits.
pub type WakeBits = u64;

/// Monotonic wake sequence.
pub type WakeSeq = u64;

/// Result returned by wake operations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WakeResult {
    woke: bool,
    local: bool,
    remote: bool,
}

impl WakeResult {
    /// Creates a result.
    pub const fn new(woke: bool, local: bool, remote: bool) -> Self {
        Self {
            woke,
            local,
            remote,
        }
    }

    /// Returns whether a new wake was produced.
    pub const fn woke(self) -> bool {
        self.woke
    }

    /// Returns whether local reschedule/drain work is needed.
    pub const fn should_resched(self) -> bool {
        self.woke
    }

    /// Returns whether the wake targets the current CPU.
    pub const fn local(self) -> bool {
        self.local
    }

    /// Returns whether the wake targets a remote CPU.
    pub const fn remote(self) -> bool {
        self.remote
    }
}

#[derive(Clone)]
struct WakeHandle {
    task: Weak<TaskCore>,
    task_id: TaskId,
    generation: u64,
}

impl WakeHandle {
    fn new(task: TaskRef) -> Self {
        Self {
            task_id: task.id(),
            generation: task.wake_generation(),
            task: Arc::downgrade(&task),
        }
    }

    fn valid_task(&self) -> Option<TaskRef> {
        let task = self.task.upgrade()?;
        if task.id() != self.task_id || !task.wake_generation_matches(self.generation) {
            return None;
        }
        Some(task)
    }
}

/// Cloneable task-context wake handle.
#[derive(Clone)]
pub struct TaskWaker {
    handle: WakeHandle,
}

impl TaskWaker {
    /// Creates a task-context waker.
    pub fn new(task: TaskRef) -> Self {
        Self {
            handle: WakeHandle::new(task),
        }
    }

    /// Creates a hard-IRQ-safe waker for the same task.
    pub fn to_hard_irq_waker(&self) -> HardIrqWaker {
        HardIrqWaker {
            handle: self.handle.clone(),
        }
    }

    /// Wakes the task from task context.
    pub fn wake(&self, bits: WakeBits) -> WakeResult {
        let Some(task) = self.handle.valid_task() else {
            return WakeResult::default();
        };
        task.publish_wake_bits(bits);
        task.bump_wake_seq();
        let woke = matches!(task.state(), TaskState::Blocked | TaskState::Ready);
        if task.state() == TaskState::Blocked {
            task.set_state(TaskState::Ready);
        }
        WakeResult::new(woke, true, false)
    }

    /// Takes coalesced wake bits.
    pub fn take_bits(&self) -> WakeBits {
        self.handle
            .task
            .upgrade()
            .map_or(0, |task| task.take_wake_bits())
    }

    /// Returns the current wake sequence.
    pub fn seq(&self) -> WakeSeq {
        self.handle.task.upgrade().map_or(0, |task| task.wake_seq())
    }
}

/// Cloneable hard-IRQ-safe wake handle.
#[derive(Clone)]
pub struct HardIrqWaker {
    handle: WakeHandle,
}

impl HardIrqWaker {
    /// Publishes wake state from hard IRQ context.
    ///
    /// This method only touches task-local atomics. Queue insertion and remote
    /// CPU notification are performed by the runtime/OS adapter.
    pub fn wake_from_irq(&self, bits: WakeBits) -> (Option<TaskRef>, WakeResult) {
        let Some(task) = self.handle.valid_task() else {
            return (None, WakeResult::default());
        };
        task.publish_wake_bits(bits);
        task.bump_wake_seq();
        let woke = task.mark_wake_pending();
        (Some(task), WakeResult::new(woke, false, false))
    }

    /// Takes coalesced wake bits.
    pub fn take_bits(&self) -> WakeBits {
        self.handle
            .task
            .upgrade()
            .map_or(0, |task| task.take_wake_bits())
    }

    /// Returns the current wake sequence.
    pub fn seq(&self) -> WakeSeq {
        self.handle.task.upgrade().map_or(0, |task| task.wake_seq())
    }
}

/// Lock-free intrusive MPSC stack used by hard-IRQ wake queues.
pub struct IrqWakeQueueCore {
    head: AtomicPtr<()>,
}

impl IrqWakeQueueCore {
    /// Creates an empty IRQ wake queue.
    pub const fn new() -> Self {
        Self {
            head: AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    /// Pushes `node` and stores the previous head through `set_next`.
    ///
    /// `node` must remain valid until a successful [`pop`](Self::pop) returns
    /// it to the caller.
    pub fn push(&self, node: *mut (), mut set_next: impl FnMut(*mut ())) {
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            set_next(head);
            match self
                .head
                .compare_exchange_weak(head, node, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(next) => head = next,
            }
        }
    }

    /// Pops one node, using `next_of` to read its intrusive next pointer.
    pub fn pop(&self, mut next_of: impl FnMut(*mut ()) -> *mut ()) -> Option<*mut ()> {
        loop {
            let head = self.head.load(Ordering::Acquire);
            if head.is_null() {
                return None;
            }
            let next = next_of(head);
            if self
                .head
                .compare_exchange(head, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(head);
            }
        }
    }

    /// Returns whether the queue is currently empty.
    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire).is_null()
    }
}

impl Default for IrqWakeQueueCore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicPtr, Ordering};

    use super::IrqWakeQueueCore;

    struct Node {
        next: AtomicPtr<()>,
    }

    #[test]
    fn irq_wake_queue_core_pushes_and_pops_intrusive_nodes() {
        let queue = IrqWakeQueueCore::new();
        let first = Node {
            next: AtomicPtr::new(core::ptr::null_mut()),
        };
        let second = Node {
            next: AtomicPtr::new(core::ptr::null_mut()),
        };
        let first_ptr = (&first as *const Node).cast_mut().cast::<()>();
        let second_ptr = (&second as *const Node).cast_mut().cast::<()>();

        queue.push(first_ptr, |next| first.next.store(next, Ordering::Release));
        queue.push(second_ptr, |next| {
            second.next.store(next, Ordering::Release)
        });

        assert_eq!(
            queue.pop(|node| unsafe { &*node.cast::<Node>() }
                .next
                .load(Ordering::Acquire)),
            Some(second_ptr)
        );
        assert_eq!(
            queue.pop(|node| unsafe { &*node.cast::<Node>() }
                .next
                .load(Ordering::Acquire)),
            Some(first_ptr)
        );
        assert!(queue.is_empty());
    }
}
