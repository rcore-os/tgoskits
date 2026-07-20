//! Fixed-capacity IRQ-to-owner event transport.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    ops::{BitOr, BitOrAssign},
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use thiserror::Error;

/// Maximum events consumed by one maintenance owner pass.
pub const MAINTENANCE_BATCH_LIMIT: usize = 64;

/// Number of preallocated event slots in every maintenance mailbox.
pub const MAINTENANCE_MAILBOX_CAPACITY: usize = 64;

/// Coalesced reasons for waking a maintenance owner.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct MaintenanceCauses(u64);

impl MaintenanceCauses {
    /// No pending reason.
    pub const EMPTY: Self = Self(0);
    /// A hardware IRQ snapshot was published.
    pub const IRQ: Self = Self(1 << 0);
    /// A task-context request was submitted.
    pub const SUBMIT: Self = Self(1 << 1);
    /// An absolute watchdog deadline expired.
    pub const WATCHDOG: Self = Self(1 << 2);
    /// Domain shutdown needs owner-thread progress.
    pub const SHUTDOWN: Self = Self(1 << 3);
    /// At least one event could not fit in the fixed mailbox.
    pub const OVERFLOW: Self = Self(1 << 63);

    /// Constructs a domain-specific cause set.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Returns the raw cause bits.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Reports whether every bit in `other` is present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Reports whether no cause is present.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for MaintenanceCauses {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for MaintenanceCauses {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Result of one non-blocking event publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum MaintenancePublishResult {
    /// The snapshot was published into one fixed slot.
    Published,
    /// The snapshot was rejected and the overflow cause was published.
    Overflowed,
}

/// Invalid owner drain request.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MaintenanceDrainError {
    /// A zero-event pass cannot make progress.
    #[error("maintenance drain batch must be non-zero")]
    EmptyBatch,
    /// The caller attempted to bypass the hard batch bound.
    #[error("maintenance drain batch {requested} exceeds maximum {maximum}")]
    BatchLimitExceeded {
        /// Requested number of events.
        requested: usize,
        /// Maximum number of events in one pass.
        maximum: usize,
    },
}

/// Result of one bounded owner-thread drain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MaintenanceDrain {
    causes: MaintenanceCauses,
    drained: usize,
    pending: bool,
}

impl MaintenanceDrain {
    /// Returns reasons coalesced before this pass claimed them.
    pub const fn causes(self) -> MaintenanceCauses {
        self.causes
    }

    /// Returns the number of snapshots delivered to the owner callback.
    pub const fn drained(self) -> usize {
        self.drained
    }

    /// Reports whether another bounded pass is required.
    pub const fn pending(self) -> bool {
        self.pending
    }
}

struct TaskEventSlot<T: Copy> {
    sequence: AtomicUsize,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T: Copy> TaskEventSlot<T> {
    fn new(sequence: usize) -> Self {
        Self {
            sequence: AtomicUsize::new(sequence),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

// SAFETY: a producer writes only after reserving the slot sequence, and the
// sole owner reads only after the producer's Release publication. Copy values
// carry no destructor that could run in hard IRQ context.
unsafe impl<T: Copy + Send> Sync for TaskEventSlot<T> {}

/// A bounded MPSC queue used only by ordinary task-context producers.
struct TaskEventQueue<T: Copy> {
    enqueue: AtomicUsize,
    dequeue: AtomicUsize,
    slots: [TaskEventSlot<T>; MAINTENANCE_MAILBOX_CAPACITY],
}

impl<T: Copy + Send> TaskEventQueue<T> {
    fn new() -> Self {
        Self {
            enqueue: AtomicUsize::new(0),
            dequeue: AtomicUsize::new(0),
            slots: core::array::from_fn(TaskEventSlot::new),
        }
    }

    fn try_push(&self, event: T) -> bool {
        let mut position = self.enqueue.load(Ordering::Relaxed);
        loop {
            let slot = &self.slots[position % MAINTENANCE_MAILBOX_CAPACITY];
            let sequence = slot.sequence.load(Ordering::Acquire);
            let difference = sequence.wrapping_sub(position) as isize;
            if difference == 0 {
                match self.enqueue.compare_exchange_weak(
                    position,
                    position.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // SAFETY: the successful sequence reservation gives
                        // this producer exclusive write ownership of the slot.
                        unsafe { (*slot.value.get()).write(event) };
                        slot.sequence
                            .store(position.wrapping_add(1), Ordering::Release);
                        return true;
                    }
                    Err(observed) => position = observed,
                }
            } else if difference < 0 {
                return false;
            } else {
                position = self.enqueue.load(Ordering::Relaxed);
            }
        }
    }

    fn pop(&self) -> Option<T> {
        let position = self.dequeue.load(Ordering::Relaxed);
        let slot = &self.slots[position % MAINTENANCE_MAILBOX_CAPACITY];
        let expected = position.wrapping_add(1);
        if slot.sequence.load(Ordering::Acquire) != expected {
            return None;
        }
        let event = unsafe {
            // SAFETY: the matching sequence is a producer's Release proof that
            // this Copy value is initialized, and this queue has one owner.
            (*slot.value.get()).assume_init_read()
        };
        slot.sequence.store(
            position.wrapping_add(MAINTENANCE_MAILBOX_CAPACITY),
            Ordering::Release,
        );
        self.dequeue.store(expected, Ordering::Relaxed);
        Some(event)
    }

    fn is_empty(&self) -> bool {
        self.enqueue.load(Ordering::Acquire) == self.dequeue.load(Ordering::Acquire)
    }
}

struct LocalIrqEventSlot<T: Copy> {
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T: Copy> LocalIrqEventSlot<T> {
    const fn new() -> Self {
        Self {
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

// SAFETY: local hard-IRQ producers are serialized on the maintenance CPU.
// The producer publishes `tail` with Release only after writing a slot, while
// the sole owner reads it only after an Acquire observation. The owner may be
// interrupted between its own operations, but producer and consumer never
// write the same index field or the same live slot.
unsafe impl<T: Copy + Send> Sync for LocalIrqEventSlot<T> {}

/// A same-CPU serialized-producer/single-owner queue for hard-IRQ snapshots.
struct LocalIrqEventRing<T: Copy> {
    producer_active: AtomicBool,
    producer: AtomicUsize,
    consumer: AtomicUsize,
    slots: [LocalIrqEventSlot<T>; MAINTENANCE_MAILBOX_CAPACITY],
}

impl<T: Copy + Send> LocalIrqEventRing<T> {
    fn new() -> Self {
        Self {
            producer_active: AtomicBool::new(false),
            producer: AtomicUsize::new(0),
            consumer: AtomicUsize::new(0),
            slots: core::array::from_fn(|_| LocalIrqEventSlot::new()),
        }
    }

    fn try_push_serialized(&self, event: T) -> bool {
        if self
            .producer_active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        let producer = self.producer.load(Ordering::Relaxed);
        let consumer = self.consumer.load(Ordering::Acquire);
        if producer.wrapping_sub(consumer) >= MAINTENANCE_MAILBOX_CAPACITY {
            self.producer_active.store(false, Ordering::Release);
            return false;
        }
        let slot = &self.slots[producer % MAINTENANCE_MAILBOX_CAPACITY];
        // SAFETY: all hard-IRQ publishers for this domain execute serially on
        // one CPU. The capacity check proves that the sole owner has released
        // this slot before the producer overwrites it.
        unsafe { (*slot.value.get()).write(event) };
        self.producer
            .store(producer.wrapping_add(1), Ordering::Release);
        self.producer_active.store(false, Ordering::Release);
        true
    }

    fn pop(&self) -> Option<T> {
        let consumer = self.consumer.load(Ordering::Relaxed);
        if self.producer.load(Ordering::Acquire) == consumer {
            return None;
        }
        let slot = &self.slots[consumer % MAINTENANCE_MAILBOX_CAPACITY];
        let event = unsafe {
            // SAFETY: the producer's Release update proves this slot contains
            // one initialized Copy value not yet consumed by the sole owner.
            (*slot.value.get()).assume_init_read()
        };
        self.consumer
            .store(consumer.wrapping_add(1), Ordering::Release);
        Some(event)
    }

    fn is_empty(&self) -> bool {
        self.producer.load(Ordering::Acquire) == self.consumer.load(Ordering::Acquire)
    }
}

/// Fixed, independent IRQ and task ingress queues with one task-context owner.
///
/// The local IRQ queue has one CPU-serialized producer side and never executes
/// a compare/exchange retry loop. Ordinary remote tasks use a separate MPSC
/// queue, so request contention cannot consume the IRQ reserve. Waking the
/// owner is deliberately a separate runtime capability.
pub struct MaintenanceMailbox<T: Copy> {
    irq_events: LocalIrqEventRing<T>,
    task_events: TaskEventQueue<T>,
    causes: AtomicU64,
}

impl<T: Copy + Send> MaintenanceMailbox<T> {
    /// Creates one empty preallocated mailbox.
    pub fn new() -> Self {
        Self {
            irq_events: LocalIrqEventRing::new(),
            task_events: TaskEventQueue::new(),
            causes: AtomicU64::new(0),
        }
    }

    /// Publishes one remote task-context request and its coalesced causes.
    pub fn publish_task_event(
        &self,
        causes: MaintenanceCauses,
        event: T,
    ) -> MaintenancePublishResult {
        self.causes.fetch_or(causes.bits(), Ordering::Release);
        if self.task_events.try_push(event) {
            MaintenancePublishResult::Published
        } else {
            self.causes
                .fetch_or(MaintenanceCauses::OVERFLOW.bits(), Ordering::Release);
            MaintenancePublishResult::Overflowed
        }
    }

    /// Publishes one local hard-IRQ snapshot without a retry loop.
    ///
    /// A one-shot producer gate turns unexpected same-CPU IRQ nesting into an
    /// explicit overflow result instead of spinning or aliasing a slot.
    /// Production callers use [`crate::maintenance::LocalIrqWake`] to validate
    /// the fixed owner CPU before reaching this primitive.
    pub fn publish_irq_event_serialized(
        &self,
        causes: MaintenanceCauses,
        event: T,
    ) -> MaintenancePublishResult {
        self.causes.fetch_or(causes.bits(), Ordering::Release);
        if self.irq_events.try_push_serialized(event) {
            MaintenancePublishResult::Published
        } else {
            self.causes
                .fetch_or(MaintenanceCauses::OVERFLOW.bits(), Ordering::Release);
            MaintenancePublishResult::Overflowed
        }
    }

    /// Coalesces a task-context cause that has no event payload.
    pub fn publish_task_cause(&self, cause: MaintenanceCauses) {
        self.causes.fetch_or(cause.bits(), Ordering::Release);
    }

    /// Delivers at most `limit` snapshots to the sole owner.
    pub fn drain_owner(
        &self,
        limit: usize,
        mut consume: impl FnMut(T),
    ) -> Result<MaintenanceDrain, MaintenanceDrainError> {
        validate_batch_limit(limit)?;
        let causes = MaintenanceCauses::from_bits(self.causes.swap(0, Ordering::AcqRel));
        let mut drained = 0;
        while drained < limit {
            let Some(event) = self.irq_events.pop() else {
                break;
            };
            consume(event);
            drained += 1;
        }
        while drained < limit {
            let Some(event) = self.task_events.pop() else {
                break;
            };
            consume(event);
            drained += 1;
        }
        Ok(MaintenanceDrain {
            causes,
            drained,
            pending: self.has_pending(),
        })
    }

    /// Reports whether event or cause evidence still needs owner service.
    pub fn has_pending(&self) -> bool {
        self.causes.load(Ordering::Acquire) != 0
            || !self.irq_events.is_empty()
            || !self.task_events.is_empty()
    }

    /// Reports whether hard-IRQ evidence is published or still being queued.
    ///
    /// The cause bit is published before the ring slot, so this observation
    /// also covers the small producer window before the stable event becomes
    /// visible at the tail index.
    pub fn has_irq_pending(&self) -> bool {
        let causes = self.causes.load(Ordering::Acquire);
        causes & (MaintenanceCauses::IRQ.bits() | MaintenanceCauses::OVERFLOW.bits()) != 0
            || !self.irq_events.is_empty()
    }
}

impl<T: Copy + Send> Default for MaintenanceMailbox<T> {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_batch_limit(limit: usize) -> Result<(), MaintenanceDrainError> {
    if limit == 0 {
        return Err(MaintenanceDrainError::EmptyBatch);
    }
    if limit > MAINTENANCE_BATCH_LIMIT {
        return Err(MaintenanceDrainError::BatchLimitExceeded {
            requested: limit,
            maximum: MAINTENANCE_BATCH_LIMIT,
        });
    }
    Ok(())
}
