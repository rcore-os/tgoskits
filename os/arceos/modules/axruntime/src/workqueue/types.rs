#[cfg(feature = "workqueue")]
use alloc::{boxed::Box, format};
#[cfg(feature = "workqueue")]
use core::sync::atomic::AtomicU8;
use core::{
    cell::UnsafeCell,
    marker::PhantomPinned,
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering},
};

#[cfg(feature = "workqueue")]
use ax_kspin::PreemptGuard;
#[cfg(feature = "workqueue")]
use ax_task::{
    arm_current_runtime_timer, cancel_current_runtime_timer,
    runtime::{RuntimeStatus, RuntimeTimerEventV1},
    timer::{RuntimeTimerOwner, TimerNode, TimerToken},
};
use thiserror::Error;

#[cfg(feature = "workqueue")]
use crate::task::{
    CpuId, CpuSet, FairMode, Nice, SchedulePolicy, TaskError, ThreadHandle, ThreadWakeHandle,
    WaitQueue, current_thread_id, yield_current_cpu,
};

/// Maximum number of intrusive nodes examined by one worker pass.
pub const WORK_BATCH_LIMIT: usize = 64;

pub(super) const QUEUED: u64 = 1 << 0;
pub(super) const RUNNING: u64 = 1 << 1;
pub(super) const RERUN: u64 = 1 << 2;
pub(super) const CANCELLING: u64 = 1 << 3;
pub(super) const FLAGS_MASK: u64 = QUEUED | RUNNING | RERUN | CANCELLING;

pub(super) const ROUTE_SHIFT: u32 = 4;
const ROUTE_BITS: u32 = 16;
pub(super) const ROUTE_VALUE_MASK: u64 = (1 << ROUTE_BITS) - 1;
pub(super) const ROUTE_MASK: u64 = ROUTE_VALUE_MASK << ROUTE_SHIFT;
pub(super) const EPOCH_SHIFT: u32 = ROUTE_SHIFT + ROUTE_BITS;
pub(super) const MAX_EPOCH: u64 = u64::MAX >> EPOCH_SHIFT;

const DOMAIN_ACCEPTING: u8 = 0;
const DOMAIN_DRAINING: u8 = 1;
const DOMAIN_DRAINED: u8 = 2;
const DOMAIN_STATE_BITS: u32 = 2;
const DOMAIN_STATE_MASK: usize = (1 << DOMAIN_STATE_BITS) - 1;
#[cfg(feature = "workqueue")]
const DOMAIN_ACTIVE_ONE: usize = 1 << DOMAIN_STATE_BITS;

#[cfg(feature = "workqueue")]
pub(super) const WORKER_UNINITIALIZED: u8 = 0;
#[cfg(feature = "workqueue")]
pub(super) const WORKER_INSTALLING: u8 = 1;
#[cfg(feature = "workqueue")]
pub(super) const WORKER_READY: u8 = 2;

/// Shared worker class selected for one work activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum WorkPriority {
    /// Normal deferred device and runtime work.
    Normal = 0,
    /// Latency-sensitive work isolated from the normal logical lane.
    High   = 1,
}

/// Result of one allocation-free queue attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum QueueWorkResult {
    /// An idle item was linked into the selected worker lane.
    Queued,
    /// The item already has one queued activation; repeated queueing coalesced.
    AlreadyPending,
    /// The callback was running and exactly one later activation was requested.
    RerunRequested,
    /// Cancellation owns the item until its queued tombstone or callback exits.
    CancelInProgress,
}

/// Workqueue configuration or ownership error.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorkQueueError {
    /// A CPU outside this fixed workqueue topology was selected.
    #[error("workqueue CPU {cpu} is outside the {cpu_count}-CPU topology")]
    InvalidCpu {
        /// Requested logical CPU.
        cpu: usize,
        /// Number of represented logical CPUs.
        cpu_count: usize,
    },
    /// A zero-sized worker pass cannot make progress.
    #[error("workqueue batch limit must be non-zero")]
    EmptyBatch,
    /// A caller attempted to bypass the hard worker-latency bound.
    #[error("workqueue batch limit {requested} exceeds the maximum {maximum}")]
    BatchLimitExceeded {
        /// Requested number of intrusive nodes.
        requested: usize,
        /// Maximum nodes allowed in one worker pass.
        maximum: usize,
    },
    /// Another context already owns this lane's single consumer capability.
    #[error("workqueue lane already has an active consumer")]
    WorkerBusy,
    /// The packed work generation cannot represent another activation.
    #[error("workqueue item activation epoch is exhausted")]
    EpochExhausted,
    /// One intrusive item was submitted through two independent queue systems.
    #[error("workqueue item is permanently bound to another queue system")]
    ForeignSystem,
    /// A logical queue stopped accepting new work before this submission.
    #[error("logical workqueue is draining or already drained")]
    DomainNotAccepting,
    /// An operation used a work item owned by another logical queue.
    #[error("work item is permanently bound to another logical workqueue")]
    ForeignDomain,
    /// Intrusive state did not match the lane that owned the node.
    #[error("workqueue intrusive state invariant was violated")]
    InvalidState,
    /// The selected per-CPU lane has no published shutdown-lifetime worker.
    #[cfg(feature = "workqueue")]
    #[error("workqueue worker is not initialized")]
    WorkerNotInitialized,
    /// A linked work item could not wake its shutdown-lifetime worker. The lane
    /// remains poisoned because the intrusive publication cannot be rolled back.
    #[cfg(feature = "workqueue")]
    #[error("workqueue worker lost its post-publication progress guarantee")]
    WorkerPoisoned,
    /// Another initializer is currently constructing this fixed worker.
    #[cfg(feature = "workqueue")]
    #[error("workqueue worker installation is already in progress")]
    WorkerInstalling,
    /// A CPU attempted to initialize a different CPU's fixed workers.
    #[cfg(feature = "workqueue")]
    #[error("workqueue CPU initialization owner mismatch: expected {expected}, got {actual}")]
    CpuInitializationOwner {
        /// CPU whose fixed workers are being created.
        expected: usize,
        /// CPU executing the initialization call.
        actual: usize,
    },
    /// A blocking facade or worker initializer was invoked from a context
    /// where the scheduler cannot yield or block.
    #[cfg(feature = "workqueue")]
    #[error("workqueue operation requires ordinary schedulable task context")]
    UnsafeContext,
    /// A fixed worker attempted to wait for work that requires a worker to
    /// finish, which would stall at least one shared lane.
    #[cfg(feature = "workqueue")]
    #[error("workqueue synchronous operation would deadlock its worker")]
    WouldDeadlock,
    /// The task runtime rejected worker creation, yielding, or parking.
    #[cfg(feature = "workqueue")]
    #[error(transparent)]
    Task(#[from] TaskError),
    /// A delayed deadline cannot be represented by its fixed atomic command.
    #[cfg(feature = "workqueue")]
    #[error("delayed-work deadline exceeds the representable monotonic range")]
    DelayOverflow,
    /// The ordinary work activation is already queued or running.
    #[cfg(feature = "workqueue")]
    #[error("delayed work is already queued or running")]
    DelayedWorkBusy,
}

/// One fixed-address callback node shared by IRQ producers and worker threads.
///
/// The item may be embedded in a larger shutdown-lifetime object. Submission
/// requires `Pin<&'static WorkItem>` so its intrusive link can never move or
/// expire while a producer, worker, or cancellation ticket still observes it.
#[derive(Debug)]
pub struct WorkItem {
    pub(super) owner_system: AtomicPtr<()>,
    pub(super) owner_domain: AtomicPtr<WorkQueue>,
    pub(super) state: AtomicU64,
    pub(super) completed_epoch: AtomicU64,
    pub(super) next: AtomicPtr<WorkItem>,
    pub(super) callback: fn(usize) -> WorkOutcome,
    pub(super) callback_data: AtomicUsize,
    #[cfg(feature = "workqueue")]
    pub(super) executing_worker: AtomicU64,
    #[cfg(feature = "workqueue")]
    pub(super) completion_wait: WaitQueue,
    _pin: PhantomPinned,
}

impl WorkItem {
    /// Creates an idle work item suitable for static initialization.
    ///
    /// `callback_data` is an opaque value interpreted only by `callback`. The
    /// callback itself runs solely from [`WorkQueueSystem::service_batch`].
    pub const fn new(callback: fn(usize) -> WorkOutcome, callback_data: usize) -> Self {
        Self {
            owner_system: AtomicPtr::new(ptr::null_mut()),
            owner_domain: AtomicPtr::new(ptr::null_mut()),
            state: AtomicU64::new(0),
            completed_epoch: AtomicU64::new(0),
            next: AtomicPtr::new(ptr::null_mut()),
            callback,
            callback_data: AtomicUsize::new(callback_data),
            #[cfg(feature = "workqueue")]
            executing_worker: AtomicU64::new(0),
            #[cfg(feature = "workqueue")]
            completion_wait: WaitQueue::new(),
            _pin: PhantomPinned,
        }
    }

    /// Reports whether no activation, callback, or cancellation owns the item.
    pub fn is_idle(&self) -> bool {
        state_flags(self.state.load(Ordering::Acquire)) == 0
    }

    pub(super) fn flush_token(&'static self) -> FlushToken {
        FlushToken {
            work: self,
            target_epoch: state_epoch(self.state.load(Ordering::Acquire)),
        }
    }

    #[cfg(feature = "workqueue")]
    pub(super) fn bind_private_callback_data(
        &self,
        callback_data: usize,
    ) -> Result<(), WorkQueueError> {
        debug_assert_ne!(callback_data, 0);
        match self.callback_data.compare_exchange(
            0,
            callback_data,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(current) if current == callback_data => Ok(()),
            Err(_) => Err(WorkQueueError::InvalidState),
        }
    }

    #[cfg(feature = "workqueue")]
    pub(super) fn is_unbound(&self) -> bool {
        self.owner_domain.load(Ordering::Acquire).is_null()
            && self.owner_system.load(Ordering::Acquire).is_null()
    }

    pub(super) fn bind_system<const CPU_COUNT: usize>(
        &self,
        system: &'static WorkQueueSystem<CPU_COUNT>,
    ) -> Result<(), WorkQueueError> {
        let system = ptr::from_ref(system).cast::<()>().cast_mut();
        match self.owner_system.compare_exchange(
            ptr::null_mut(),
            system,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(owner) if owner == system => Ok(()),
            Err(_) => Err(WorkQueueError::ForeignSystem),
        }
    }

    pub(super) fn completion_reached(&self, target_epoch: u64) -> bool {
        self.completed_epoch.load(Ordering::Acquire) >= target_epoch
    }

    pub(super) fn mark_one_completed(&self) {
        let previous = self.completed_epoch.fetch_add(1, Ordering::Release);
        debug_assert!(previous < MAX_EPOCH, "work completion epoch overflowed");
    }

    pub(super) fn mark_completed_through(&self, target_epoch: u64) {
        let mut completed = self.completed_epoch.load(Ordering::Acquire);
        while completed < target_epoch {
            match self.completed_epoch.compare_exchange_weak(
                completed,
                target_epoch,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => completed = observed,
            }
        }
    }

    #[cfg(feature = "workqueue")]
    pub(super) fn bind_domain(&self, domain: &'static WorkQueue) -> Result<(), WorkQueueError> {
        let domain = ptr::from_ref(domain).cast_mut();
        match self.owner_domain.compare_exchange(
            ptr::null_mut(),
            domain,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(owner) if owner == domain => Ok(()),
            Err(_) => Err(WorkQueueError::ForeignDomain),
        }
    }

    #[cfg(feature = "workqueue")]
    pub(super) fn belongs_to_domain(&self, domain: &WorkQueue) -> bool {
        self.owner_domain.load(Ordering::Acquire) == ptr::from_ref(domain).cast_mut()
    }

    #[cfg(feature = "workqueue")]
    pub(super) fn belongs_to_system<const CPU_COUNT: usize>(
        &self,
        system: &WorkQueueSystem<CPU_COUNT>,
    ) -> bool {
        self.owner_system.load(Ordering::Acquire) == ptr::from_ref(system).cast::<()>().cast_mut()
    }

    pub(super) fn domain_accepts_callback_requeue(&self) -> bool {
        let domain = self.owner_domain.load(Ordering::Acquire);
        if domain.is_null() {
            return true;
        }
        unsafe {
            // SAFETY: binding requires a pinned shutdown-lifetime WorkQueue,
            // and its state remains atomic for the complete WorkItem lifetime.
            (*domain).state() == WorkQueueState::Accepting
        }
    }

    pub(super) fn release_domain_item(&self) {
        let domain = self.owner_domain.load(Ordering::Acquire);
        if domain.is_null() {
            return;
        }
        unsafe {
            // SAFETY: identical shutdown-lifetime binding to
            // `domain_accepts_callback_requeue`; the worker is the sole final
            // transition from active to idle for this item.
            let domain: &'static WorkQueue = &*domain;
            domain.release_item_reservation();
        }
    }

    #[cfg(feature = "workqueue")]
    pub(super) fn publish_completion(&self) {
        self.completion_wait.notify_all();
    }
}

/// Lifecycle of one logical queue domain sharing the fixed per-CPU workers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum WorkQueueState {
    /// New submissions and callback-owned requeues are accepted.
    Accepting = DOMAIN_ACCEPTING,
    /// New submissions are rejected while already accepted items finish.
    Draining  = DOMAIN_DRAINING,
    /// Every accepted item reached idle after the drain transition.
    Drained   = DOMAIN_DRAINED,
}

/// Logical serialization and admission domain layered over shared worker lanes.
///
/// A domain owns no scheduler thread. It pins all of its items to one CPU and
/// one normal/high worker class, and supplies the accepting/draining boundary
/// needed by subsystem teardown.
#[derive(Debug)]
pub struct WorkQueue {
    cpu: usize,
    priority: WorkPriority,
    lifecycle: AtomicUsize,
    #[cfg(feature = "workqueue")]
    drain_wait: WaitQueue,
    #[cfg(feature = "workqueue")]
    drain_notify_work: WorkItem,
    _pin: PhantomPinned,
}

impl WorkQueue {
    /// Creates an accepting logical queue for one CPU and worker class.
    pub const fn new(cpu: usize, priority: WorkPriority) -> Self {
        Self {
            cpu,
            priority,
            lifecycle: AtomicUsize::new(DOMAIN_ACCEPTING as usize),
            #[cfg(feature = "workqueue")]
            drain_wait: WaitQueue::new(),
            #[cfg(feature = "workqueue")]
            drain_notify_work: WorkItem::new(drain_notify_entry, 0),
            _pin: PhantomPinned,
        }
    }

    /// Returns the CPU affinity shared by all items in this domain.
    pub const fn cpu(&self) -> usize {
        self.cpu
    }

    /// Returns the shared worker priority class selected by this domain.
    pub const fn priority(&self) -> WorkPriority {
        self.priority
    }

    /// Returns the current admission/drain state.
    pub fn state(&self) -> WorkQueueState {
        match domain_state(self.lifecycle.load(Ordering::Acquire)) {
            DOMAIN_ACCEPTING => WorkQueueState::Accepting,
            DOMAIN_DRAINING => WorkQueueState::Draining,
            DOMAIN_DRAINED => WorkQueueState::Drained,
            _ => unreachable!("logical workqueue published an invalid state"),
        }
    }

    /// Stops new submissions and returns a non-blocking drain observation.
    pub fn begin_drain(self: Pin<&'static Self>) -> Result<WorkQueueDrainToken, WorkQueueError> {
        #[cfg(feature = "workqueue")]
        self.drain_notify_work
            .bind_private_callback_data(ptr::from_ref(self.get_ref()).expose_provenance())?;
        loop {
            let observed = self.lifecycle.load(Ordering::Acquire);
            if domain_state(observed) != DOMAIN_ACCEPTING {
                return Err(WorkQueueError::DomainNotAccepting);
            }
            let next_state = if domain_active_items(observed) == 0 {
                DOMAIN_DRAINED
            } else {
                DOMAIN_DRAINING
            };
            let updated = domain_with_state(observed, next_state);
            if self
                .lifecycle
                .compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        let token = WorkQueueDrainToken {
            queue: self.get_ref(),
        };
        Ok(token)
    }

    #[cfg(feature = "workqueue")]
    pub(super) fn reserve_item(&self) -> Result<(), WorkQueueError> {
        loop {
            let observed = self.lifecycle.load(Ordering::Acquire);
            if domain_state(observed) != DOMAIN_ACCEPTING {
                return Err(WorkQueueError::DomainNotAccepting);
            }
            let updated = observed
                .checked_add(DOMAIN_ACTIVE_ONE)
                .ok_or(WorkQueueError::InvalidState)?;
            if self
                .lifecycle
                .compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    pub(super) fn release_item_reservation(&'static self) {
        loop {
            let observed = self.lifecycle.load(Ordering::Acquire);
            let active_items = domain_active_items(observed);
            assert!(active_items > 0, "logical workqueue item count underflowed");
            let active_items = active_items - 1;
            let previous_state = domain_state(observed);
            let next_state = if active_items == 0 && previous_state == DOMAIN_DRAINING {
                DOMAIN_DRAINED
            } else {
                previous_state
            };
            let updated = domain_value(next_state, active_items);
            if self
                .lifecycle
                .compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                #[cfg(feature = "workqueue")]
                if previous_state == DOMAIN_DRAINING && next_state == DOMAIN_DRAINED {
                    self.queue_drain_notification();
                }
                return;
            }
        }
    }

    #[cfg(feature = "workqueue")]
    fn queue_drain_notification(&'static self) {
        let route = WorkerRoute {
            cpu: self.cpu,
            priority: self.priority,
        };
        let work = unsafe {
            // SAFETY: the shutdown-lifetime WorkQueue was pinned before this
            // embedded intrusive node could be submitted.
            Pin::new_unchecked(&self.drain_notify_work)
        };
        let _queue_result = queue_runtime_work(route, work)
            .expect("an accepted workqueue item must retain its fixed worker through drain");
    }
}

#[cfg(feature = "workqueue")]
fn drain_notify_entry(data: usize) -> WorkOutcome {
    let queue = unsafe {
        // SAFETY: `begin_drain` stores only a pinned shutdown-lifetime
        // WorkQueue address before the notification item can be submitted.
        &*ptr::with_exposed_provenance::<WorkQueue>(data)
    };
    queue.drain_wait.notify_all();
    WorkOutcome::Complete
}

/// Non-blocking observation of one logical queue's drain boundary.
#[derive(Clone, Copy, Debug)]
#[must_use]
pub struct WorkQueueDrainToken {
    queue: &'static WorkQueue,
}

impl WorkQueueDrainToken {
    /// Returns whether all work accepted before draining reached idle.
    pub fn is_complete(self) -> bool {
        self.queue.state() == WorkQueueState::Drained
    }
}

fn domain_state(value: usize) -> u8 {
    (value & DOMAIN_STATE_MASK) as u8
}

fn domain_active_items(value: usize) -> usize {
    value >> DOMAIN_STATE_BITS
}

fn domain_with_state(value: usize, state: u8) -> usize {
    (value & !DOMAIN_STATE_MASK) | state as usize
}

fn domain_value(state: u8, active_items: usize) -> usize {
    (active_items << DOMAIN_STATE_BITS) | state as usize
}
