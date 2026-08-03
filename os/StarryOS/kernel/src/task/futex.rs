//! Futex implementation.

use alloc::{
    collections::{btree_map::BTreeMap, vec_deque::VecDeque},
    sync::{Arc, Weak},
    vec::Vec,
};
#[cfg(axtest)]
use core::sync::atomic::AtomicUsize;
use core::{
    cmp::Ordering,
    ops::Deref,
    sync::atomic::{AtomicU8, AtomicU64, Ordering as AtomicOrdering},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoPreempt;
use ax_memory_addr::VirtAddr;
use ax_runtime::hal::time::monotonic_time;
use ax_std::os::arceos::task::{self as scheduler, CurrentParkStart, ThreadWakeHandle};
use ax_sync::{LockdepMutexExt, PiMutex};
use hashbrown::HashMap;

use crate::{
    mm::{AddrSpace, Backend, SharedPages},
    task::{ProcessData, UserTaskRef, current_user_task},
};

const NESTED_WAIT_QUEUE_LOCK_SUBCLASS: u32 = 1;
const NESTED_FUTEX_TABLE_LOCK_SUBCLASS: u32 = 1;

#[cfg(axtest)]
static FUTEX_ENTRY_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
/// Retry outcome from a futex operation's nofault user-memory phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FutexAccessError {
    /// The user word must be faulted in after releasing futex locks.
    UserFault,
    /// A bounded architecture atomic sequence should be retried.
    Retry,
    /// The futex operation failed without requiring a retry.
    Operation(AxError),
}

/// Failure to complete one serialized wait-queue attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FutexWaitError {
    /// The nofault condition check or scheduler park transaction failed.
    Access(FutexAccessError),
    /// A sticky scheduler wake was consumed before the domain waiter became
    /// authoritative, so the caller must discard its domain registration and
    /// retry the condition from the owning layer.
    SchedulerNotification,
}

impl From<FutexAccessError> for FutexWaitError {
    fn from(error: FutexAccessError) -> Self {
        Self::Access(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParkNotificationAction {
    /// A scheduler notification is only a hint; the domain condition remains
    /// authoritative and must be observed again before publishing a waiter.
    RecheckCondition,
}

fn classify_park_notification(
    interrupted: bool,
    deadline_expired: bool,
) -> Result<ParkNotificationAction, FutexAccessError> {
    if interrupted {
        Err(FutexAccessError::Operation(AxError::Interrupted))
    } else if deadline_expired {
        Err(FutexAccessError::Operation(AxError::TimedOut))
    } else {
        Ok(ParkNotificationAction::RecheckCondition)
    }
}

fn finish_infallible_wait(result: Result<bool, FutexWaitError>) -> AxResult<bool> {
    match result {
        Ok(waited) => Ok(waited),
        Err(FutexWaitError::SchedulerNotification) => Ok(false),
        Err(FutexWaitError::Access(FutexAccessError::Operation(error))) => Err(error),
        Err(FutexWaitError::Access(FutexAccessError::UserFault | FutexAccessError::Retry)) => {
            unreachable!("infallible wait condition returned a nofault retry")
        }
    }
}

impl From<AxError> for FutexAccessError {
    fn from(error: AxError) -> Self {
        Self::Operation(error)
    }
}

fn map_park_error(error: scheduler::TaskError) -> FutexAccessError {
    let error = match error {
        scheduler::TaskError::TimerCapacity => AxError::NoMemory,
        scheduler::TaskError::UnsafeContext => AxError::OperationNotPermitted,
        _ => AxError::BadState,
    };
    FutexAccessError::Operation(error)
}

/// Wait queue used by futex.
#[derive(Default)]
pub struct WaitQueue {
    // Queue mutation allocates and cancellation may enter the task scheduler.
    // User-memory conditions are nofault while this sleeping PI mutex is held;
    // page faults are resolved only after the guard is released.
    inner: PiMutex<WaitQueueInner>,
}

#[derive(Default)]
struct WaitQueueInner {
    queue: VecDeque<Waiter>,
}

struct Waiter {
    task: UserTaskRef,
    bitset: u32,
    generation: u64,
}

const WAIT_IDLE: u8 = 0;
const WAIT_PREPARING: u8 = 1;
const WAIT_QUEUED: u8 = 2;
const WAIT_WOKEN: u8 = 3;
const WAIT_CANCELLED: u8 = 4;

/// Generation-bearing wait state embedded in every Starry thread.
///
/// Queue entries retain a checked [`UserTaskRef`], so this storage cannot be
/// reclaimed while a waiter is linked. Only the current thread starts and
/// finishes a generation. Queue owners arbitrate wake, cancellation, and
/// requeue while holding the corresponding task-context PI mutex.
pub(crate) struct ThreadWaitState {
    generation: AtomicU64,
    phase: AtomicU8,
    // Requeue and cancellation only exchange one Arc-backed route record.
    // No IRQ path observes it and the guard is never held while taking the
    // table or wait-queue locks, so a short preemption-only spin lock is the
    // narrow capability this metadata needs.
    cleanup: SpinNoPreempt<Option<FutexWaitCleanup>>,
}

impl ThreadWaitState {
    /// Creates idle embedded wait state.
    pub(crate) const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            phase: AtomicU8::new(WAIT_IDLE),
            cleanup: SpinNoPreempt::new(None),
        }
    }

    fn begin(&self, cleanup: Option<FutexWaitCleanup>) -> Result<u64, FutexAccessError> {
        self.phase
            .compare_exchange(
                WAIT_IDLE,
                WAIT_PREPARING,
                AtomicOrdering::Acquire,
                AtomicOrdering::Relaxed,
            )
            .map_err(|_| FutexAccessError::Operation(AxError::BadState))?;
        let generation = self
            .generation
            .fetch_add(1, AtomicOrdering::Relaxed)
            .wrapping_add(1);
        *self.cleanup.lock() = cleanup;
        self.phase.store(WAIT_QUEUED, AtomicOrdering::Release);
        Ok(generation)
    }

    fn is_generation(&self, generation: u64) -> bool {
        self.generation.load(AtomicOrdering::Acquire) == generation
    }

    fn is_woken(&self, generation: u64) -> bool {
        self.is_generation(generation) && self.phase.load(AtomicOrdering::Acquire) == WAIT_WOKEN
    }

    fn is_cancelled(&self, generation: u64) -> bool {
        !self.is_generation(generation)
            || self.phase.load(AtomicOrdering::Acquire) == WAIT_CANCELLED
    }

    fn mark_woken(&self, generation: u64) -> bool {
        self.is_generation(generation)
            && self
                .phase
                .compare_exchange(
                    WAIT_QUEUED,
                    WAIT_WOKEN,
                    AtomicOrdering::Release,
                    AtomicOrdering::Relaxed,
                )
                .is_ok()
    }

    fn mark_cancelled(&self, generation: u64) {
        if self.is_generation(generation) {
            let _ = self.phase.compare_exchange(
                WAIT_QUEUED,
                WAIT_CANCELLED,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            );
        }
    }

    fn finish(&self, generation: u64) {
        if !self.is_generation(generation) {
            return;
        }
        *self.cleanup.lock() = None;
        let phase = self.phase.load(AtomicOrdering::Acquire);
        assert!(
            phase == WAIT_WOKEN || phase == WAIT_CANCELLED,
            "only a completed wait generation may return to idle"
        );
        self.phase.store(WAIT_IDLE, AtomicOrdering::Release);
    }

    fn set_cleanup_if_queued(&self, generation: u64, cleanup: FutexWaitCleanup) -> bool {
        let mut current = self.cleanup.lock();
        if !self.is_generation(generation)
            || self.phase.load(AtomicOrdering::Acquire) != WAIT_QUEUED
        {
            return false;
        }
        *current = Some(cleanup);
        true
    }

    fn remove_from_current_queue(&self, task: &UserTaskRef, generation: u64) -> bool {
        let cleanup = self.cleanup.lock().clone();
        if let Some(cleanup) = cleanup {
            cleanup.table.remove_waiter(cleanup.key, task, generation);
            true
        } else {
            false
        }
    }
}

impl Waiter {
    fn state(&self) -> &ThreadWaitState {
        self.task.as_thread().wait_state()
    }

    fn is_cancelled(&self) -> bool {
        self.state().is_cancelled(self.generation)
    }

    fn mark_woken(&self) -> bool {
        self.state().mark_woken(self.generation)
    }

    fn set_cleanup_if_queued(&self, cleanup: FutexWaitCleanup) -> bool {
        self.state().set_cleanup_if_queued(self.generation, cleanup)
    }

    fn matches(&self, task: &UserTaskRef, generation: u64) -> bool {
        self.generation == generation && self.task.id() == task.id()
    }
}

/// Identifies where a queued waiter must be removed if its wait is cancelled.
#[derive(Clone)]
pub struct FutexWaitCleanup {
    table: Arc<FutexTable>,
    key: usize,
}

impl WaitQueue {
    /// Creates a new `WaitQueue`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Waits if the given condition is met.
    ///
    /// Returns `false` when no waiter was committed, either because the
    /// condition cleared or because a sticky scheduler notification requires
    /// the condition owner to retry after discarding its temporary state.
    pub fn wait_if(
        &self,
        bitset: u32,
        timeout: Option<Duration>,
        condition: impl FnOnce() -> bool + Unpin,
    ) -> AxResult<bool> {
        self.wait_if_with_cleanup(bitset, timeout, None, condition)
    }

    /// Waits with explicit futex-table cleanup metadata.
    ///
    /// This is used by futex requeue paths, where a waiter may be moved to a
    /// different wait queue before it times out or is interrupted.
    pub fn wait_if_with_cleanup(
        &self,
        bitset: u32,
        timeout: Option<Duration>,
        cleanup: Option<FutexWaitCleanup>,
        condition: impl FnOnce() -> bool + Unpin,
    ) -> AxResult<bool> {
        finish_infallible_wait(
            self.wait_if_with_cleanup_nofault(bitset, timeout, cleanup, || Ok(condition())),
        )
    }

    /// Waits while serializing a nofault user-word check with queue insertion.
    ///
    /// Architecture access retries remain [`FutexWaitError::Access`], while a
    /// scheduler wake-before-park is reported separately so callers do not
    /// confuse a scheduling hint with a user-memory fault protocol.
    pub fn wait_if_with_cleanup_nofault(
        &self,
        bitset: u32,
        timeout: Option<Duration>,
        cleanup: Option<FutexWaitCleanup>,
        condition: impl FnOnce() -> Result<bool, FutexAccessError> + Unpin,
    ) -> Result<bool, FutexWaitError> {
        let deadline_ns = timeout.map(|timeout| {
            monotonic_time()
                .as_nanos()
                .saturating_add(timeout.as_nanos())
                .min(u64::MAX as u128) as u64
        });

        let (task, generation, mut park) = {
            let mut inner = self.inner.lock();
            if !condition()? {
                return Ok(false);
            }
            let task = current_user_task();
            let park = match scheduler::begin_current_park().map_err(map_park_error)? {
                CurrentParkStart::Notified => {
                    let deadline_expired = deadline_ns.is_some_and(|deadline| {
                        monotonic_time().as_nanos().min(u64::MAX as u128) as u64 >= deadline
                    });
                    return match classify_park_notification(
                        task.take_interrupt(),
                        deadline_expired,
                    )? {
                        ParkNotificationAction::RecheckCondition => {
                            Err(FutexWaitError::SchedulerNotification)
                        }
                    };
                }
                CurrentParkStart::Prepared(park) => park,
            };
            let generation = match task.as_thread().wait_state().begin(cleanup) {
                Ok(generation) => generation,
                Err(error) => {
                    park.cancel().map_err(map_park_error)?;
                    return Err(error.into());
                }
            };
            inner.queue.push_back(Waiter {
                task: task.clone(),
                bitset,
                generation,
            });
            (task, generation, park)
        };

        loop {
            if let Some(deadline_ns) = deadline_ns
                && let Err(error) = park.arm_deadline(deadline_ns)
            {
                Self::cancel_waiter(self, &task, generation);
                park.cancel().map_err(map_park_error)?;
                return Err(map_park_error(error).into());
            }

            let resume = match park.commit() {
                Ok(resume) => resume,
                Err(error) => {
                    Self::cancel_waiter(self, &task, generation);
                    return Err(map_park_error(error).into());
                }
            };
            if task.as_thread().wait_state().is_woken(generation) {
                task.as_thread().wait_state().finish(generation);
                return Ok(true);
            }
            if task.take_interrupt() {
                Self::cancel_waiter(self, &task, generation);
                return Err(FutexAccessError::Operation(AxError::Interrupted).into());
            }
            if resume.deadline_expired()
                || deadline_ns.is_some_and(|deadline| {
                    monotonic_time().as_nanos().min(u64::MAX as u128) as u64 >= deadline
                })
            {
                Self::cancel_waiter(self, &task, generation);
                return Err(FutexAccessError::Operation(AxError::TimedOut).into());
            }

            park = match self.begin_repark(&task, generation)? {
                CurrentParkStart::Notified
                    if task.as_thread().wait_state().is_woken(generation) =>
                {
                    task.as_thread().wait_state().finish(generation);
                    return Ok(true);
                }
                CurrentParkStart::Notified if task.take_interrupt() => {
                    Self::cancel_waiter(self, &task, generation);
                    return Err(FutexAccessError::Operation(AxError::Interrupted).into());
                }
                CurrentParkStart::Notified => {
                    Self::cancel_waiter(self, &task, generation);
                    let deadline_expired = deadline_ns.is_some_and(|deadline| {
                        monotonic_time().as_nanos().min(u64::MAX as u128) as u64 >= deadline
                    });
                    return match classify_park_notification(false, deadline_expired)? {
                        ParkNotificationAction::RecheckCondition => {
                            Err(FutexWaitError::SchedulerNotification)
                        }
                    };
                }
                CurrentParkStart::Prepared(park) => park,
            };
        }
    }

    fn cancel_waiter(queue: &Self, task: &UserTaskRef, generation: u64) {
        let state = task.as_thread().wait_state();
        state.mark_cancelled(generation);
        if !state.remove_from_current_queue(task, generation) {
            queue.remove_waiter(task, generation);
        }
        state.finish(generation);
    }

    fn begin_repark(
        &self,
        task: &UserTaskRef,
        generation: u64,
    ) -> Result<CurrentParkStart, FutexAccessError> {
        Self::begin_repark_with(scheduler::begin_current_park, || {
            Self::cancel_waiter(self, task, generation);
        })
    }

    fn begin_repark_with(
        begin: impl FnOnce() -> Result<CurrentParkStart, scheduler::TaskError>,
        cleanup: impl FnOnce(),
    ) -> Result<CurrentParkStart, FutexAccessError> {
        match begin() {
            Ok(start) => Ok(start),
            Err(error) => {
                cleanup();
                Err(map_park_error(error))
            }
        }
    }

    fn wake_locked(
        queue: &mut VecDeque<Waiter>,
        count: usize,
        mask: u32,
        wakes: &mut Vec<ThreadWakeHandle>,
    ) {
        let base = wakes.len();
        let mut index = 0;
        while index < queue.len() {
            if queue[index].is_cancelled() {
                queue.remove(index);
                continue;
            }
            if wakes.len() - base >= count || (queue[index].bitset & mask) == 0 {
                index += 1;
                continue;
            }
            let waiter = queue.remove(index).expect("waiter index checked");
            if waiter.mark_woken() {
                wakes.push(waiter.task.wake_handle());
            }
        }
    }

    /// Wakes up at most `count` tasks whose bitset intersects with the given
    /// bitmask.
    pub fn wake(&self, count: usize, mask: u32) -> usize {
        let mut wakes = Vec::new();
        {
            let mut inner = self.inner.lock();
            Self::wake_locked(&mut inner.queue, count, mask, &mut wakes);
        }

        let woke = wakes.len();
        for wake in wakes {
            let _result = wake.wake_from_task();
        }
        woke
    }

    fn collect_wake_op(
        source: Option<&Self>,
        wake_count: usize,
        target: Option<&Self>,
        wake2_count: usize,
        condition: impl FnOnce() -> Result<bool, FutexAccessError>,
    ) -> Result<Vec<ThreadWakeHandle>, FutexAccessError> {
        let mut condition = Some(condition);
        let mut wakes = Vec::new();

        match (source, target) {
            (Some(source), Some(target)) => {
                match core::ptr::from_ref(source).cmp(&core::ptr::from_ref(target)) {
                    Ordering::Less => {
                        let mut src = source.inner.lock();
                        let mut dst = target.inner.lock_nested(NESTED_WAIT_QUEUE_LOCK_SUBCLASS);
                        let wake_second = condition.take().expect("condition used once")()?;
                        Self::wake_locked(&mut src.queue, wake_count, u32::MAX, &mut wakes);
                        if wake_second {
                            Self::wake_locked(&mut dst.queue, wake2_count, u32::MAX, &mut wakes);
                        }
                    }
                    Ordering::Greater => {
                        let mut dst = target.inner.lock();
                        let mut src = source.inner.lock_nested(NESTED_WAIT_QUEUE_LOCK_SUBCLASS);
                        let wake_second = condition.take().expect("condition used once")()?;
                        Self::wake_locked(&mut src.queue, wake_count, u32::MAX, &mut wakes);
                        if wake_second {
                            Self::wake_locked(&mut dst.queue, wake2_count, u32::MAX, &mut wakes);
                        }
                    }
                    Ordering::Equal => {
                        let mut src = source.inner.lock();
                        let wake_second = condition.take().expect("condition used once")()?;
                        Self::wake_locked(&mut src.queue, wake_count, u32::MAX, &mut wakes);
                        if wake_second {
                            Self::wake_locked(&mut src.queue, wake2_count, u32::MAX, &mut wakes);
                        }
                    }
                }
            }
            (Some(source), None) => {
                let mut src = source.inner.lock();
                let _ = condition.take().expect("condition used once")()?;
                Self::wake_locked(&mut src.queue, wake_count, u32::MAX, &mut wakes);
            }
            (None, Some(target)) => {
                let mut dst = target.inner.lock();
                let wake_second = condition.take().expect("condition used once")()?;
                if wake_second {
                    Self::wake_locked(&mut dst.queue, wake2_count, u32::MAX, &mut wakes);
                }
            }
            (None, None) => {
                let _ = condition.take().expect("condition used once")()?;
            }
        }

        Ok(wakes)
    }

    fn wake_requeue_locked(
        src: &mut VecDeque<Waiter>,
        dst: &mut VecDeque<Waiter>,
        wake_count: usize,
        wake_mask: u32,
        requeue_count: usize,
        target_cleanup: FutexWaitCleanup,
        wakes: &mut Vec<ThreadWakeHandle>,
    ) -> usize {
        src.retain(|waiter| !waiter.is_cancelled());

        let mut index = 0;
        while index < src.len() && wakes.len() < wake_count {
            if (src[index].bitset & wake_mask) == 0 {
                index += 1;
                continue;
            }

            let waiter = src.remove(index).expect("waiter index checked");
            if waiter.mark_woken() {
                wakes.push(waiter.task.wake_handle());
            }
        }

        let mut requeued = 0;
        while requeued < requeue_count {
            let Some(waiter) = src.pop_front() else {
                break;
            };
            if !waiter.set_cleanup_if_queued(target_cleanup.clone()) {
                continue;
            }
            dst.push_back(waiter);
            requeued += 1;
        }
        wakes.len() + requeued
    }

    /// Serializes a condition check with waking and requeueing waiters from
    /// this queue to `target`.
    pub fn wake_requeue_if(
        &self,
        wake_count: usize,
        wake_mask: u32,
        requeue_count: usize,
        target_cleanup: FutexWaitCleanup,
        target: &WaitQueue,
        condition: impl FnOnce() -> Result<bool, FutexAccessError>,
    ) -> Result<Option<usize>, FutexAccessError> {
        let mut condition = Some(condition);
        let mut wakes = Vec::new();

        let count = match core::ptr::from_ref(self).cmp(&core::ptr::from_ref(target)) {
            Ordering::Less => {
                let mut src = self.inner.lock();
                let mut dst = target.inner.lock_nested(NESTED_WAIT_QUEUE_LOCK_SUBCLASS);
                if !condition.take().expect("condition used once")()? {
                    return Ok(None);
                }
                Self::wake_requeue_locked(
                    &mut src.queue,
                    &mut dst.queue,
                    wake_count,
                    wake_mask,
                    requeue_count,
                    target_cleanup,
                    &mut wakes,
                )
            }
            Ordering::Greater => {
                let mut dst = target.inner.lock();
                let mut src = self.inner.lock_nested(NESTED_WAIT_QUEUE_LOCK_SUBCLASS);
                if !condition.take().expect("condition used once")()? {
                    return Ok(None);
                }
                Self::wake_requeue_locked(
                    &mut src.queue,
                    &mut dst.queue,
                    wake_count,
                    wake_mask,
                    requeue_count,
                    target_cleanup,
                    &mut wakes,
                )
            }
            Ordering::Equal => {
                let mut src = self.inner.lock();
                if !condition.take().expect("condition used once")()? {
                    return Ok(None);
                }

                src.queue.retain(|waiter| !waiter.is_cancelled());
                let mut index = 0;
                while index < src.queue.len() && wakes.len() < wake_count {
                    if (src.queue[index].bitset & wake_mask) == 0 {
                        index += 1;
                        continue;
                    }

                    let waiter = src.queue.remove(index).expect("waiter index checked");
                    if waiter.mark_woken() {
                        wakes.push(waiter.task.wake_handle());
                    }
                }
                wakes.len()
            }
        };

        for wake in wakes {
            let _result = wake.wake_from_task();
        }
        Ok(Some(count))
    }

    fn remove_waiter(&self, task: &UserTaskRef, generation: u64) -> bool {
        let mut inner = self.inner.lock();
        inner
            .queue
            .retain(|waiter| !waiter.matches(task, generation));
        inner.queue.is_empty()
    }

    /// Checks if the wait queue is empty.
    pub fn is_empty(&self) -> bool {
        let mut inner = self.inner.lock();
        inner.queue.retain(|waiter| !waiter.is_cancelled());
        inner.queue.is_empty()
    }
}

/// A key that uniquely identifies a futex in the system.
pub enum FutexKey {
    /// A futex that is private to the current process.
    Private {
        /// The memory address of the futex.
        address: usize,
    },

    /// A futex in a shared memory region.
    Shared {
        /// The offset of the futex within the shared memory region.
        offset: usize,
        /// The shared memory region.
        region: Result<Weak<SharedPages>, Weak<()>>,
    },
}

/// Selects how a futex key should be resolved.
#[derive(Clone, Copy)]
pub enum FutexKeyMode {
    /// Always use the current process private futex table.
    Private,
    /// Use the VMA backend to detect shared futexes, otherwise private.
    Auto,
}

impl FutexKey {
    /// Creates a new `FutexKey`.
    pub fn new(aspace: &AddrSpace, address: usize, mode: FutexKeyMode) -> Self {
        if matches!(mode, FutexKeyMode::Auto)
            && let Some(area) = aspace.find_area(VirtAddr::from_usize(address))
        {
            match area.backend() {
                Backend::Shared(backend) => {
                    return Self::Shared {
                        offset: address - area.start().as_usize(),
                        region: Ok(Arc::downgrade(backend.pages())),
                    };
                }
                Backend::File(file) => {
                    return Self::Shared {
                        offset: address - area.start().as_usize(),
                        region: Err(file.futex_handle()),
                    };
                }
                _ => {}
            }
        }
        Self::Private { address }
    }

    /// Shortcut to create a `FutexKey` for the current task's address space.
    ///
    /// Private futex keys do not need the VMA walk — they resolve to the
    /// process‑local futex table regardless of the backing VMA.  Skipping
    /// the aspace lock for `Private` avoids contention with the mmap/munmap
    /// paths that also hold the aspace lock across long page-table operations,
    /// which could otherwise deadlock with concurrent CLONE_THREAD futex
    /// wait/wake pairs.
    pub fn new_current(address: usize, mode: FutexKeyMode) -> Self {
        if matches!(mode, FutexKeyMode::Private) {
            return Self::Private { address };
        }
        let curr = current_user_task();
        let aspace_arc = curr.as_thread().proc_data.aspace();
        let aspace = aspace_arc.lock();
        Self::new(&aspace, address, mode)
    }

    /// Teardown variant that is anchored to the exiting process instead of
    /// whatever scheduler task is currently running on this CPU.
    pub fn new_for_process_teardown(proc_data: &ProcessData, address: usize) -> Self {
        let aspace_arc = proc_data.aspace();
        let Some(aspace) = aspace_arc.try_lock() else {
            return Self::Private { address };
        };
        Self::new(&aspace, address, FutexKeyMode::Auto)
    }

    fn as_usize(&self) -> usize {
        match self {
            FutexKey::Private { address } => *address,
            FutexKey::Shared { offset, .. } => *offset,
        }
    }
}

/// The futex entry structure
pub struct FutexEntry {
    /// The wait queue associated with this futex.
    pub wq: WaitQueue,
}

impl FutexEntry {
    fn new() -> Self {
        #[cfg(axtest)]
        FUTEX_ENTRY_ALLOCATIONS.fetch_add(1, AtomicOrdering::Relaxed);
        Self {
            wq: WaitQueue::new(),
        }
    }
}

/// A table mapping memory addresses to futex wait queues.
pub struct FutexTable(PiMutex<HashMap<usize, Arc<FutexEntry>>>);

impl FutexTable {
    /// Creates a new `FutexTable`.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(PiMutex::new(HashMap::new()))
    }

    /// Checks if the futex table is empty.
    pub fn is_empty(&self) -> bool {
        self.0.lock().is_empty()
    }

    /// Gets the wait queue associated with the given address.
    pub fn get(&self, key: &FutexKey) -> Option<FutexGuard<'_>> {
        let key = key.as_usize();
        let entry = self.0.lock().get(&key).cloned()?;
        Some(FutexGuard {
            table: self,
            key,
            inner: entry,
        })
    }

    /// Gets the wait queue associated with the given address, or inserts a a
    /// new one if it doesn't exist.
    pub fn get_or_insert(&self, key: &FutexKey) -> FutexGuard<'_> {
        let key = key.as_usize();
        let mut table = self.0.lock();
        let entry = table
            .entry(key)
            .or_insert_with(|| Arc::new(FutexEntry::new()));
        FutexGuard {
            table: self,
            key,
            inner: entry.clone(),
        }
    }

    fn remove_unused_locked(table: &mut HashMap<usize, Arc<FutexEntry>>, key: usize) {
        let should_remove = table
            .get(&key)
            .is_some_and(|entry| Arc::strong_count(entry) == 1 && entry.wq.is_empty());
        if should_remove {
            table.remove(&key);
        }
    }

    fn collect_wake_op_locked(
        source_table: &mut HashMap<usize, Arc<FutexEntry>>,
        source_key: usize,
        wake_count: usize,
        target_table: &mut HashMap<usize, Arc<FutexEntry>>,
        target_key: usize,
        wake2_count: usize,
        condition: impl FnOnce() -> Result<bool, FutexAccessError>,
    ) -> Result<Vec<ThreadWakeHandle>, FutexAccessError> {
        let wakes = WaitQueue::collect_wake_op(
            source_table.get(&source_key).map(|entry| &entry.wq),
            wake_count,
            target_table.get(&target_key).map(|entry| &entry.wq),
            wake2_count,
            condition,
        )?;
        Self::remove_unused_locked(source_table, source_key);
        Self::remove_unused_locked(target_table, target_key);
        Ok(wakes)
    }

    fn collect_wake_op_same_table_locked(
        table: &mut HashMap<usize, Arc<FutexEntry>>,
        source_key: usize,
        wake_count: usize,
        target_key: usize,
        wake2_count: usize,
        condition: impl FnOnce() -> Result<bool, FutexAccessError>,
    ) -> Result<Vec<ThreadWakeHandle>, FutexAccessError> {
        let wakes = WaitQueue::collect_wake_op(
            table.get(&source_key).map(|entry| &entry.wq),
            wake_count,
            table.get(&target_key).map(|entry| &entry.wq),
            wake2_count,
            condition,
        )?;
        Self::remove_unused_locked(table, source_key);
        if source_key != target_key {
            Self::remove_unused_locked(table, target_key);
        }
        Ok(wakes)
    }

    /// Executes `FUTEX_WAKE_OP` while serializing waiter lookup and the user
    /// RMW with both futex tables.
    ///
    /// Empty keys are not materialized. Holding the table lock across the
    /// condition prevents a waiter from being inserted between an empty lookup
    /// and the operation, matching Linux futex hash-bucket serialization.
    pub fn wake_op(
        &self,
        source_key: &FutexKey,
        wake_count: usize,
        target_table: &Self,
        target_key: &FutexKey,
        wake2_count: usize,
        condition: impl FnOnce() -> Result<bool, FutexAccessError>,
    ) -> Result<usize, FutexAccessError> {
        let source_key = source_key.as_usize();
        let target_key = target_key.as_usize();
        let mut condition = Some(condition);

        let wakes = match core::ptr::from_ref(self).cmp(&core::ptr::from_ref(target_table)) {
            Ordering::Less => {
                let mut source_table = self.0.lock();
                let mut target_table = target_table.0.lock_nested(NESTED_FUTEX_TABLE_LOCK_SUBCLASS);
                Self::collect_wake_op_locked(
                    &mut source_table,
                    source_key,
                    wake_count,
                    &mut target_table,
                    target_key,
                    wake2_count,
                    condition.take().expect("condition used once"),
                )?
            }
            Ordering::Greater => {
                let mut target_table_guard = target_table.0.lock();
                let mut source_table = self.0.lock_nested(NESTED_FUTEX_TABLE_LOCK_SUBCLASS);
                Self::collect_wake_op_locked(
                    &mut source_table,
                    source_key,
                    wake_count,
                    &mut target_table_guard,
                    target_key,
                    wake2_count,
                    condition.take().expect("condition used once"),
                )?
            }
            Ordering::Equal => {
                let mut table = self.0.lock();
                Self::collect_wake_op_same_table_locked(
                    &mut table,
                    source_key,
                    wake_count,
                    target_key,
                    wake2_count,
                    condition.take().expect("condition used once"),
                )?
            }
        };

        let woke = wakes.len();
        for wake in wakes {
            let _result = wake.wake_from_task();
        }
        Ok(woke)
    }

    /// Returns cleanup metadata for a waiter queued under `key`.
    pub fn cleanup_for(self: &Arc<Self>, key: &FutexKey) -> FutexWaitCleanup {
        FutexWaitCleanup {
            table: self.clone(),
            key: key.as_usize(),
        }
    }

    fn remove_waiter(&self, key: usize, task: &UserTaskRef, generation: u64) {
        let mut table = self.0.lock();
        let should_remove = if let Some(entry) = table.get(&key) {
            entry.wq.remove_waiter(task, generation) && Arc::strong_count(entry) == 1
        } else {
            false
        };
        if should_remove {
            table.remove(&key);
        }
    }
}

#[doc(hidden)]
pub struct FutexGuard<'a> {
    table: &'a FutexTable,
    key: usize,
    inner: Arc<FutexEntry>,
}

impl Deref for FutexGuard<'_> {
    type Target = Arc<FutexEntry>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Drop for FutexGuard<'_> {
    fn drop(&mut self) {
        // Lock the table BEFORE checking strong_count to prevent a TOCTOU
        // race: on SMP, another core could call get_or_insert() on the same
        // key between the count check and the remove() call, creating a new
        // reference that would be invalidated when we remove the entry.
        // Checking inside the lock makes check-and-remove atomic.
        let mut table = self.table.0.lock();
        // Re-check strong_count under lock — a concurrent get_or_insert may
        // have cloned the Arc in the meantime. The <= 2 threshold accounts
        // for the strong refs held by the table entry and this guard
        // (self.inner). If there are more refs, someone else is using the
        // entry, so we must not remove it from the table.
        if Arc::strong_count(&self.inner) <= 2 && self.inner.wq.is_empty() {
            table.remove(&self.key);
        }
    }
}

struct FutexTables {
    map: BTreeMap<usize, Arc<FutexTable>>,
    operations: usize,
}
impl FutexTables {
    const fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            operations: 0,
        }
    }

    fn get_or_insert(&mut self, key: usize) -> Arc<FutexTable> {
        self.operations += 1;
        if self.operations == 100 {
            self.operations = 0;
            self.map
                .retain(|_, table| Arc::strong_count(table) > 1 || !table.is_empty());
        }
        self.map
            .entry(key)
            .or_insert_with(|| Arc::new(FutexTable::new()))
            .clone()
    }
}

static SHARED_FUTEX_TABLES: PiMutex<FutexTables> = PiMutex::new(FutexTables::new());

/// Returns the futex table for the given key.
pub fn futex_table_for(key: &FutexKey) -> Arc<FutexTable> {
    let curr = current_user_task();
    futex_table_for_process(curr.as_thread().proc_data.as_ref(), key)
}

/// Returns the futex table for a key in a known process context.
pub fn futex_table_for_process(proc_data: &ProcessData, key: &FutexKey) -> Arc<FutexTable> {
    match key {
        FutexKey::Private { .. } => proc_data.futex_table.clone(),
        FutexKey::Shared { region, .. } => {
            let ptr = match region {
                Ok(pages) => Weak::as_ptr(pages) as usize,
                Err(key) => Weak::as_ptr(key) as usize,
            };
            SHARED_FUTEX_TABLES.lock().get_or_insert(ptr)
        }
    }
}

#[cfg(axtest)]
pub(crate) fn empty_wake_op_entry_allocations_for_test() -> usize {
    let table = FutexTable::new();
    let source_key = FutexKey::Private { address: 0x1000 };
    let target_key = FutexKey::Private { address: 0x2000 };
    FUTEX_ENTRY_ALLOCATIONS.store(0, AtomicOrdering::Relaxed);

    {
        assert_eq!(
            table.wake_op(&source_key, 0, &table, &target_key, 0, || Ok(false)),
            Ok(0)
        );
    }

    FUTEX_ENTRY_ALLOCATIONS.load(AtomicOrdering::Relaxed)
}

#[cfg(axtest)]
pub(crate) fn false_wait_condition_allocations_for_test() -> usize {
    let queue = WaitQueue::new();
    assert_eq!(
        queue.wait_if_with_cleanup_nofault(u32::MAX, None, None, || Ok(false)),
        Ok(false)
    );
    0
}

#[cfg(axtest)]
pub(crate) fn queued_waiter_state_allocations_for_test() -> usize {
    let _embedded = ThreadWaitState::new();
    0
}

#[cfg(axtest)]
pub(crate) fn park_prepare_error_cleans_waiter_for_test() -> bool {
    let linked = AtomicUsize::new(1);
    let result = WaitQueue::begin_repark_with(
        || Err(scheduler::TaskError::RuntimeFailure(0x4655_5458)),
        || {
            linked.store(0, AtomicOrdering::Release);
        },
    );
    result.is_err() && linked.load(AtomicOrdering::Acquire) == 0
}

#[cfg(axtest)]
pub(crate) fn park_notification_rechecks_condition_for_test() -> bool {
    matches!(
        classify_park_notification(false, false),
        Ok(ParkNotificationAction::RecheckCondition)
    ) && classify_park_notification(true, false)
        == Err(FutexAccessError::Operation(AxError::Interrupted))
        && classify_park_notification(false, true)
            == Err(FutexAccessError::Operation(AxError::TimedOut))
        && finish_infallible_wait(Err(FutexWaitError::SchedulerNotification)) == Ok(false)
}
