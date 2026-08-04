//! Futex implementation.

use alloc::{collections::vec_deque::VecDeque, sync::Arc};
#[cfg(axtest)]
use core::sync::atomic::AtomicUsize;
use core::{
    cmp::Ordering,
    sync::atomic::{AtomicU8, AtomicU64, Ordering as AtomicOrdering},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoPreempt;
use ax_memory_addr::VirtAddr;
use ax_runtime::hal::time::monotonic_time;
use ax_std::os::arceos::task::{self as scheduler, CurrentParkStart, ThreadWakeBatch};
use ax_sync::{LockdepMutexExt, PiMutex};

use crate::{
    mm::{AddrSpace, Backend, SharedPages},
    task::{ProcessData, UserTaskRef, current_user_task, process_memory::ProcessMemoryShare},
};

const NESTED_FUTEX_BUCKET_LOCK_SUBCLASS: u32 = 1;
const FUTEX_BUCKET_COUNT: usize = 64;
type WakeBatch = ThreadWakeBatch;

fn wake_batch(wakes: WakeBatch) -> usize {
    wakes.wake_all()
}

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
        let Some(first) = self.cleanup.lock().clone() else {
            return false;
        };
        if first.remove_waiter(task, generation) {
            return true;
        }

        // Requeue publishes the new cleanup route before moving the waiter
        // while holding both buckets. A canceller may have sampled the old
        // route just before that publication; after marking the generation
        // cancelled no later requeue can win, so one route refresh is enough.
        let Some(current) = self.cleanup.lock().clone() else {
            return true;
        };
        if !first.same_route(&current) {
            current.remove_waiter(task, generation);
        }
        true
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
    domain: FutexDomainOwner,
    key: FutexKey,
}

impl FutexWaitCleanup {
    fn remove_waiter(&self, task: &UserTaskRef, generation: u64) -> bool {
        self.domain
            .domain()
            .remove_waiter(&self.key, task, generation)
    }

    fn same_route(&self, other: &Self) -> bool {
        self.domain.same(&other.domain) && self.key.same(&other.key)
    }
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
        let task = current_user_task();
        self.wait_if_with_cleanup_nofault_for(&task, bitset, timeout, cleanup, condition)
    }

    /// Variant for a syscall that already captured its current task identity.
    pub(crate) fn wait_if_with_cleanup_nofault_for(
        &self,
        task: &UserTaskRef,
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
            let task = task.clone();
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

    fn wake_locked(queue: &mut VecDeque<Waiter>, count: usize, mask: u32, wakes: &mut WakeBatch) {
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
                Self::push_wake(wakes, waiter);
            }
        }
    }

    fn push_wake(wakes: &mut WakeBatch, waiter: Waiter) {
        assert!(
            wakes.push(waiter.task.wake_handle()),
            "one futex wait generation cannot enter two live wake batches"
        );
    }

    /// Wakes up at most `count` tasks whose bitset intersects with the given
    /// bitmask.
    pub fn wake(&self, count: usize, mask: u32) -> usize {
        let mut wakes = WakeBatch::new();
        {
            let mut inner = self.inner.lock();
            Self::wake_locked(&mut inner.queue, count, mask, &mut wakes);
        }

        wake_batch(wakes)
    }

    fn remove_waiter(&self, task: &UserTaskRef, generation: u64) -> bool {
        let mut inner = self.inner.lock();
        inner
            .queue
            .retain(|waiter| !waiter.matches(task, generation));
        inner.queue.is_empty()
    }
}

/// Stable backing-object identity retained by a shared futex waiter.
#[derive(Clone)]
pub(crate) enum SharedFutexIdentity {
    Pages(Arc<SharedPages>),
    File(Arc<()>),
}

impl SharedFutexIdentity {
    fn address(&self) -> usize {
        match self {
            Self::Pages(pages) => Arc::as_ptr(pages) as usize,
            Self::File(file) => Arc::as_ptr(file) as usize,
        }
    }

    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Pages(left), Self::Pages(right)) => Arc::ptr_eq(left, right),
            (Self::File(left), Self::File(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

/// A key that uniquely identifies a futex in the system.
#[derive(Clone)]
pub(crate) enum FutexKey {
    /// A private futex follows Linux `current->mm`, not the process/TGID.
    Private { mm_generation: u64, address: usize },

    /// A shared futex follows a stable backing object and mapping offset.
    Shared {
        offset: usize,
        identity: SharedFutexIdentity,
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
    fn new(aspace: &AddrSpace, mm_generation: u64, address: usize, mode: FutexKeyMode) -> Self {
        if matches!(mode, FutexKeyMode::Auto)
            && let Some(area) = aspace.find_area(VirtAddr::from_usize(address))
        {
            match area.backend() {
                Backend::Shared(backend) => {
                    return Self::Shared {
                        offset: address - area.start().as_usize(),
                        identity: SharedFutexIdentity::Pages(backend.pages().clone()),
                    };
                }
                Backend::File(file) => {
                    return Self::Shared {
                        offset: address - area.start().as_usize(),
                        identity: SharedFutexIdentity::File(file.futex_handle()),
                    };
                }
                _ => {}
            }
        }
        Self::Private {
            mm_generation,
            address,
        }
    }

    fn bucket_hash(&self) -> usize {
        match self {
            Self::Private {
                mm_generation,
                address,
            } => mix_futex_hash(*address, *mm_generation as usize),
            Self::Shared { offset, identity } => mix_futex_hash(*offset, identity.address()),
        }
    }

    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Private {
                    mm_generation: left_mm,
                    address: left_address,
                },
                Self::Private {
                    mm_generation: right_mm,
                    address: right_address,
                },
            ) => left_mm == right_mm && left_address == right_address,
            (
                Self::Shared {
                    offset: left_offset,
                    identity: left_identity,
                },
                Self::Shared {
                    offset: right_offset,
                    identity: right_identity,
                },
            ) => left_offset == right_offset && left_identity.same(right_identity),
            _ => false,
        }
    }
}

fn mix_futex_hash(first: usize, second: usize) -> usize {
    let mut value = first ^ second.rotate_left(17);
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9usize);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11ebusize);
    value ^ (value >> 31)
}

struct FutexBucketWaiter {
    key: FutexKey,
    waiter: Waiter,
}

struct FutexBucket {
    waiters: PiMutex<VecDeque<FutexBucketWaiter>>,
}

impl FutexBucket {
    const fn new() -> Self {
        Self {
            waiters: PiMutex::new(VecDeque::new()),
        }
    }
}

static NEXT_PRIVATE_FUTEX_DOMAIN: AtomicU64 = AtomicU64::new(1);
static SHARED_FUTEX_DOMAIN: FutexDomain = FutexDomain::new_shared();

/// Fixed Linux-style futex hash buckets owned by one mm generation.
pub(crate) struct FutexDomain {
    generation: u64,
    buckets: [FutexBucket; FUTEX_BUCKET_COUNT],
}

impl FutexDomain {
    const fn new_shared() -> Self {
        Self {
            generation: 0,
            buckets: [const { FutexBucket::new() }; FUTEX_BUCKET_COUNT],
        }
    }

    pub(crate) fn new_private() -> Self {
        let generation = NEXT_PRIVATE_FUTEX_DOMAIN.fetch_add(1, AtomicOrdering::Relaxed);
        assert_ne!(generation, 0, "private futex mm generation exhausted");
        Self {
            generation,
            buckets: [const { FutexBucket::new() }; FUTEX_BUCKET_COUNT],
        }
    }

    fn generation(&self) -> u64 {
        self.generation
    }

    fn bucket(&self, key: &FutexKey) -> (usize, &FutexBucket) {
        let index = key.bucket_hash() % FUTEX_BUCKET_COUNT;
        (index, &self.buckets[index])
    }

    fn remove_waiter(&self, key: &FutexKey, task: &UserTaskRef, generation: u64) -> bool {
        let (_, bucket) = self.bucket(key);
        let mut waiters = bucket.waiters.lock();
        let Some(index) = waiters
            .iter()
            .position(|entry| entry.key.same(key) && entry.waiter.matches(task, generation))
        else {
            return false;
        };
        waiters.remove(index);
        true
    }
}

#[derive(Clone)]
enum FutexDomainOwner {
    Private(Arc<FutexDomain>),
    Shared,
}

impl FutexDomainOwner {
    fn domain(&self) -> &FutexDomain {
        match self {
            Self::Private(domain) => domain,
            Self::Shared => &SHARED_FUTEX_DOMAIN,
        }
    }

    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Private(left), Self::Private(right)) => Arc::ptr_eq(left, right),
            (Self::Shared, Self::Shared) => true,
            _ => false,
        }
    }
}

/// Per-syscall futex ownership captured from the calling thread once.
///
/// A syscall may retry its nofault user access, but it cannot change process
/// identity while the syscall is active. Shared keys are intentionally
/// re-resolved after a fault because their VMA backing may have changed.
pub(crate) struct FutexContext {
    task: UserTaskRef,
    memory: ProcessMemoryShare,
}

impl FutexContext {
    pub(crate) fn current() -> Self {
        let task = current_user_task();
        let memory = task.as_thread().proc_data.memory_share();
        Self { task, memory }
    }

    pub(crate) fn task(&self) -> &UserTaskRef {
        &self.task
    }

    fn resolve_keys(
        &self,
        first_address: usize,
        second_address: Option<usize>,
        mode: FutexKeyMode,
    ) -> (FutexKey, Option<FutexKey>) {
        if matches!(mode, FutexKeyMode::Private) {
            let mm_generation = self.memory.private_futexes().generation();
            return (
                FutexKey::Private {
                    mm_generation,
                    address: first_address,
                },
                second_address.map(|address| FutexKey::Private {
                    mm_generation,
                    address,
                }),
            );
        }

        let aspace = self.memory.aspace();
        let aspace = aspace.lock();
        let mm_generation = self.memory.private_futexes().generation();
        (
            FutexKey::new(&aspace, mm_generation, first_address, mode),
            second_address.map(|address| FutexKey::new(&aspace, mm_generation, address, mode)),
        )
    }

    fn domain_for(&self, key: &FutexKey) -> FutexDomainOwner {
        match key {
            FutexKey::Private { .. } => FutexDomainOwner::Private(self.memory.private_futexes()),
            FutexKey::Shared { .. } => FutexDomainOwner::Shared,
        }
    }

    pub(crate) fn resolve(&self, address: usize, mode: FutexKeyMode) -> ResolvedFutex {
        let (key, None) = self.resolve_keys(address, None, mode) else {
            unreachable!("single futex resolution returned a second key")
        };
        let domain = self.domain_for(&key);
        ResolvedFutex { key, domain }
    }

    pub(crate) fn resolve_pair(
        &self,
        first_address: usize,
        second_address: usize,
        mode: FutexKeyMode,
    ) -> (ResolvedFutex, ResolvedFutex) {
        let (first_key, Some(second_key)) =
            self.resolve_keys(first_address, Some(second_address), mode)
        else {
            unreachable!("paired futex resolution omitted the second key")
        };
        let first_domain = self.domain_for(&first_key);
        let second_domain = self.domain_for(&second_key);
        (
            ResolvedFutex {
                key: first_key,
                domain: first_domain,
            },
            ResolvedFutex {
                key: second_key,
                domain: second_domain,
            },
        )
    }
}

/// Key and ownership domain resolved together for one futex operation.
pub(crate) struct ResolvedFutex {
    key: FutexKey,
    domain: FutexDomainOwner,
}

impl ResolvedFutex {
    fn cleanup(&self) -> FutexWaitCleanup {
        FutexWaitCleanup {
            domain: self.domain.clone(),
            key: self.key.clone(),
        }
    }

    pub(crate) fn wait_nofault_for(
        &self,
        task: &UserTaskRef,
        bitset: u32,
        timeout: Option<Duration>,
        condition: impl FnOnce() -> Result<bool, FutexAccessError> + Unpin,
    ) -> Result<bool, FutexWaitError> {
        let deadline_ns = timeout.map(|timeout| {
            monotonic_time()
                .as_nanos()
                .saturating_add(timeout.as_nanos())
                .min(u64::MAX as u128) as u64
        });
        let task = task.clone();
        let (_, bucket) = self.domain.domain().bucket(&self.key);
        let (generation, mut park) = {
            let mut waiters = bucket.waiters.lock();
            if !condition()? {
                return Ok(false);
            }
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
            let generation = match task.as_thread().wait_state().begin(Some(self.cleanup())) {
                Ok(generation) => generation,
                Err(error) => {
                    park.cancel().map_err(map_park_error)?;
                    return Err(error.into());
                }
            };
            waiters.push_back(FutexBucketWaiter {
                key: self.key.clone(),
                waiter: Waiter {
                    task: task.clone(),
                    bitset,
                    generation,
                },
            });
            (generation, park)
        };

        loop {
            if let Some(deadline_ns) = deadline_ns
                && let Err(error) = park.arm_deadline(deadline_ns)
            {
                cancel_futex_waiter(&task, generation);
                park.cancel().map_err(map_park_error)?;
                return Err(map_park_error(error).into());
            }

            let resume = match park.commit() {
                Ok(resume) => resume,
                Err(error) => {
                    cancel_futex_waiter(&task, generation);
                    return Err(map_park_error(error).into());
                }
            };
            if task.as_thread().wait_state().is_woken(generation) {
                task.as_thread().wait_state().finish(generation);
                return Ok(true);
            }
            if task.take_interrupt() {
                cancel_futex_waiter(&task, generation);
                return Err(FutexAccessError::Operation(AxError::Interrupted).into());
            }
            if resume.deadline_expired()
                || deadline_ns.is_some_and(|deadline| {
                    monotonic_time().as_nanos().min(u64::MAX as u128) as u64 >= deadline
                })
            {
                cancel_futex_waiter(&task, generation);
                return Err(FutexAccessError::Operation(AxError::TimedOut).into());
            }

            park = match scheduler::begin_current_park() {
                Ok(CurrentParkStart::Notified)
                    if task.as_thread().wait_state().is_woken(generation) =>
                {
                    task.as_thread().wait_state().finish(generation);
                    return Ok(true);
                }
                Ok(CurrentParkStart::Notified) if task.take_interrupt() => {
                    cancel_futex_waiter(&task, generation);
                    return Err(FutexAccessError::Operation(AxError::Interrupted).into());
                }
                Ok(CurrentParkStart::Notified) => {
                    cancel_futex_waiter(&task, generation);
                    let deadline_expired = deadline_ns.is_some_and(|deadline| {
                        monotonic_time().as_nanos().min(u64::MAX as u128) as u64 >= deadline
                    });
                    return match classify_park_notification(false, deadline_expired)? {
                        ParkNotificationAction::RecheckCondition => {
                            Err(FutexWaitError::SchedulerNotification)
                        }
                    };
                }
                Ok(CurrentParkStart::Prepared(park)) => park,
                Err(error) => {
                    cancel_futex_waiter(&task, generation);
                    return Err(map_park_error(error).into());
                }
            };
        }
    }

    pub(crate) fn wake(&self, count: usize, mask: u32) -> usize {
        let (_, bucket) = self.domain.domain().bucket(&self.key);
        let mut wakes = WakeBatch::new();
        {
            let mut waiters = bucket.waiters.lock();
            collect_futex_wakes(&mut waiters, &self.key, count, mask, &mut wakes);
        }
        wake_batch(wakes)
    }

    pub(crate) fn requeue_to(
        &self,
        target: &Self,
        wake_count: usize,
        wake_mask: u32,
        requeue_count: usize,
        condition: impl FnOnce() -> Result<bool, FutexAccessError>,
    ) -> Result<Option<usize>, FutexAccessError> {
        let request = FutexRequeueRequest {
            wake_count,
            wake_mask,
            requeue_count,
        };
        let (_, source_bucket) = self.domain.domain().bucket(&self.key);
        let (_, target_bucket) = target.domain.domain().bucket(&target.key);
        let mut condition = Some(condition);
        let mut wakes = WakeBatch::new();

        let count =
            match core::ptr::from_ref(source_bucket).cmp(&core::ptr::from_ref(target_bucket)) {
                Ordering::Less => {
                    let mut source_waiters = source_bucket.waiters.lock();
                    let mut target_waiters = target_bucket
                        .waiters
                        .lock_nested(NESTED_FUTEX_BUCKET_LOCK_SUBCLASS);
                    if !condition.take().expect("condition used once")()? {
                        return Ok(None);
                    }
                    collect_futex_requeue(
                        &mut source_waiters,
                        &self.key,
                        &mut target_waiters,
                        target,
                        request,
                        &mut wakes,
                    )
                }
                Ordering::Greater => {
                    let mut target_waiters = target_bucket.waiters.lock();
                    let mut source_waiters = source_bucket
                        .waiters
                        .lock_nested(NESTED_FUTEX_BUCKET_LOCK_SUBCLASS);
                    if !condition.take().expect("condition used once")()? {
                        return Ok(None);
                    }
                    collect_futex_requeue(
                        &mut source_waiters,
                        &self.key,
                        &mut target_waiters,
                        target,
                        request,
                        &mut wakes,
                    )
                }
                Ordering::Equal => {
                    let mut waiters = source_bucket.waiters.lock();
                    if !condition.take().expect("condition used once")()? {
                        return Ok(None);
                    }
                    collect_futex_requeue_same_bucket(
                        &mut waiters,
                        &self.key,
                        target,
                        request,
                        &mut wakes,
                    )
                }
            };
        let woken = wake_batch(wakes);
        debug_assert!(count >= woken);
        Ok(Some(count))
    }

    pub(crate) fn wake_op(
        &self,
        wake_count: usize,
        target: &Self,
        wake2_count: usize,
        condition: impl FnOnce() -> Result<bool, FutexAccessError>,
    ) -> Result<usize, FutexAccessError> {
        let (_, source_bucket) = self.domain.domain().bucket(&self.key);
        let (_, target_bucket) = target.domain.domain().bucket(&target.key);
        let mut condition = Some(condition);
        let mut wakes = WakeBatch::new();
        match core::ptr::from_ref(source_bucket).cmp(&core::ptr::from_ref(target_bucket)) {
            Ordering::Less => {
                let mut source_waiters = source_bucket.waiters.lock();
                let mut target_waiters = target_bucket
                    .waiters
                    .lock_nested(NESTED_FUTEX_BUCKET_LOCK_SUBCLASS);
                let wake_second = condition.take().expect("condition used once")()?;
                collect_futex_wakes(
                    &mut source_waiters,
                    &self.key,
                    wake_count,
                    u32::MAX,
                    &mut wakes,
                );
                if wake_second {
                    collect_futex_wakes(
                        &mut target_waiters,
                        &target.key,
                        wake2_count,
                        u32::MAX,
                        &mut wakes,
                    );
                }
            }
            Ordering::Greater => {
                let mut target_waiters = target_bucket.waiters.lock();
                let mut source_waiters = source_bucket
                    .waiters
                    .lock_nested(NESTED_FUTEX_BUCKET_LOCK_SUBCLASS);
                let wake_second = condition.take().expect("condition used once")()?;
                collect_futex_wakes(
                    &mut source_waiters,
                    &self.key,
                    wake_count,
                    u32::MAX,
                    &mut wakes,
                );
                if wake_second {
                    collect_futex_wakes(
                        &mut target_waiters,
                        &target.key,
                        wake2_count,
                        u32::MAX,
                        &mut wakes,
                    );
                }
            }
            Ordering::Equal => {
                let mut waiters = source_bucket.waiters.lock();
                let wake_second = condition.take().expect("condition used once")()?;
                collect_futex_wakes(&mut waiters, &self.key, wake_count, u32::MAX, &mut wakes);
                if wake_second {
                    collect_futex_wakes(
                        &mut waiters,
                        &target.key,
                        wake2_count,
                        u32::MAX,
                        &mut wakes,
                    );
                }
            }
        }
        Ok(wake_batch(wakes))
    }
}

fn cancel_futex_waiter(task: &UserTaskRef, generation: u64) {
    let state = task.as_thread().wait_state();
    state.mark_cancelled(generation);
    let _ = state.remove_from_current_queue(task, generation);
    state.finish(generation);
}

fn collect_futex_wakes(
    waiters: &mut VecDeque<FutexBucketWaiter>,
    key: &FutexKey,
    count: usize,
    mask: u32,
    wakes: &mut WakeBatch,
) {
    let base = wakes.len();
    let mut index = 0;
    while index < waiters.len() {
        if waiters[index].waiter.is_cancelled() {
            waiters.remove(index);
            continue;
        }
        if !waiters[index].key.same(key)
            || wakes.len() - base >= count
            || (waiters[index].waiter.bitset & mask) == 0
        {
            index += 1;
            continue;
        }
        let waiter = waiters
            .remove(index)
            .expect("futex waiter index checked")
            .waiter;
        if waiter.mark_woken() {
            WaitQueue::push_wake(wakes, waiter);
        }
    }
}

fn collect_futex_requeue(
    source: &mut VecDeque<FutexBucketWaiter>,
    source_key: &FutexKey,
    target_waiters: &mut VecDeque<FutexBucketWaiter>,
    target: &ResolvedFutex,
    request: FutexRequeueRequest,
    wakes: &mut WakeBatch,
) -> usize {
    let wake_base = wakes.len();
    collect_futex_wakes(
        source,
        source_key,
        request.wake_count,
        request.wake_mask,
        wakes,
    );
    let woken = wakes.len() - wake_base;
    let mut requeued = 0;
    let mut index = 0;
    while index < source.len() && requeued < request.requeue_count {
        if source[index].waiter.is_cancelled() {
            source.remove(index);
            continue;
        }
        if !source[index].key.same(source_key) {
            index += 1;
            continue;
        }
        let mut entry = source.remove(index).expect("futex waiter index checked");
        if !entry.waiter.set_cleanup_if_queued(target.cleanup()) {
            continue;
        }
        entry.key = target.key.clone();
        target_waiters.push_back(entry);
        requeued += 1;
    }
    woken + requeued
}

fn collect_futex_requeue_same_bucket(
    waiters: &mut VecDeque<FutexBucketWaiter>,
    source_key: &FutexKey,
    target: &ResolvedFutex,
    request: FutexRequeueRequest,
    wakes: &mut WakeBatch,
) -> usize {
    let wake_base = wakes.len();
    collect_futex_wakes(
        waiters,
        source_key,
        request.wake_count,
        request.wake_mask,
        wakes,
    );
    let woken = wakes.len() - wake_base;
    if source_key.same(&target.key) {
        return woken;
    }
    let mut requeued = 0;
    for entry in waiters.iter_mut() {
        if requeued == request.requeue_count {
            break;
        }
        if entry.key.same(source_key)
            && !entry.waiter.is_cancelled()
            && entry.waiter.set_cleanup_if_queued(target.cleanup())
        {
            entry.key = target.key.clone();
            requeued += 1;
        }
    }
    woken + requeued
}

#[derive(Clone, Copy)]
struct FutexRequeueRequest {
    wake_count: usize,
    wake_mask: u32,
    requeue_count: usize,
}

/// Resolves an exit-time futex against the exiting process's captured mm.
pub(crate) fn resolve_futex_for_process_teardown(
    proc_data: &ProcessData,
    address: usize,
) -> ResolvedFutex {
    let memory = proc_data.memory_share();
    let private = memory.private_futexes();
    let aspace = memory.aspace();
    let key = FutexKey::new(
        &aspace.lock(),
        private.generation(),
        address,
        FutexKeyMode::Auto,
    );
    let domain = match key {
        FutexKey::Private { .. } => FutexDomainOwner::Private(private),
        FutexKey::Shared { .. } => FutexDomainOwner::Shared,
    };
    ResolvedFutex { key, domain }
}

#[cfg(axtest)]
pub(crate) fn empty_wake_op_leaves_fixed_buckets_empty_for_test() -> bool {
    let domain = Arc::new(FutexDomain::new_private());
    let source = ResolvedFutex {
        key: FutexKey::Private {
            mm_generation: domain.generation(),
            address: 0x1000,
        },
        domain: FutexDomainOwner::Private(domain.clone()),
    };
    let target = ResolvedFutex {
        key: FutexKey::Private {
            mm_generation: domain.generation(),
            address: 0x2000,
        },
        domain: FutexDomainOwner::Private(domain.clone()),
    };
    assert_eq!(source.wake_op(0, &target, 0, || Ok(false)), Ok(0));
    domain
        .buckets
        .iter()
        .all(|bucket| bucket.waiters.lock().is_empty())
}

#[cfg(axtest)]
pub(crate) fn futex_keys_follow_mm_and_backing_identity_for_test() -> bool {
    let first_mm = FutexDomain::new_private();
    let second_mm = FutexDomain::new_private();
    let first = FutexKey::Private {
        mm_generation: first_mm.generation(),
        address: 0x1000,
    };
    let same_mm = FutexKey::Private {
        mm_generation: first_mm.generation(),
        address: 0x1000,
    };
    let other_mm = FutexKey::Private {
        mm_generation: second_mm.generation(),
        address: 0x1000,
    };
    let file = Arc::new(());
    let same_file = FutexKey::Shared {
        offset: 0x20,
        identity: SharedFutexIdentity::File(file.clone()),
    };
    let alias = FutexKey::Shared {
        offset: 0x20,
        identity: SharedFutexIdentity::File(file),
    };
    let different_file = FutexKey::Shared {
        offset: 0x20,
        identity: SharedFutexIdentity::File(Arc::new(())),
    };

    first.same(&same_mm)
        && !first.same(&other_mm)
        && same_file.same(&alias)
        && !same_file.same(&different_file)
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
