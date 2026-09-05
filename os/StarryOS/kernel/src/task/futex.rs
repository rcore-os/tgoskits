//! Futex implementation.

use alloc::{
    collections::{btree_map::BTreeMap, vec_deque::VecDeque},
    sync::Arc,
    vec::Vec,
};
use core::{
    cmp::Ordering,
    future::Future,
    ops::Deref,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering as AtomicOrdering},
    task::{Poll, Waker},
    time::Duration,
};

use ax_memory_addr::VirtAddr;
use ax_runtime::hal::time::{TimeValue, monotonic_time};
use ax_task::{
    current,
    future::{self, block_on, interruptible},
};
use hashbrown::HashMap;

use crate::{
    StarryError, StarryResult,
    mm::{AddrSpace, SharedFutexIdentity, SharedFutexRegion},
    sync::{LockdepMutexExt, Mutex},
    task::{AsThread, ProcessData},
};

const NESTED_WAIT_QUEUE_LOCK_SUBCLASS: u32 = 1;

/// Result of a user-memory operation performed while futex queues are locked.
pub enum FutexAccessError {
    /// The user mapping must be faulted in after releasing the queue locks.
    Fault,
    /// A bounded architecture atomic sequence must be retried later.
    Retry,
    /// The futex operation failed independently of user-memory residency.
    Operation(StarryError),
}

/// Retries one futex operation whose locked section may only use nofault user
/// access.
///
/// `operation` must release all futex queue locks before returning an error.
/// Page population is therefore performed by `fault_in` only after the locked
/// transaction has aborted without queue side effects.
pub fn retry_futex_nofault<T>(
    operation: impl FnMut() -> Result<T, FutexAccessError>,
    fault_in: impl FnMut() -> StarryResult<()>,
) -> StarryResult<T> {
    retry_futex_nofault_with(operation, fault_in, ax_task::yield_now)
}

fn retry_futex_nofault_with<T>(
    mut operation: impl FnMut() -> Result<T, FutexAccessError>,
    mut fault_in: impl FnMut() -> StarryResult<()>,
    mut retry: impl FnMut(),
) -> StarryResult<T> {
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(FutexAccessError::Fault) => fault_in()?,
            Err(FutexAccessError::Retry) => {}
            Err(FutexAccessError::Operation(error)) => return Err(error),
        }
        retry();
    }
}

/// Wait queue used by futex.
#[derive(Default)]
pub struct WaitQueue {
    // Futex waits must re-check the user value while serializing with wakeups.
    // That re-check may fault and sleep, so this queue cannot use a no-IRQ
    // spinlock.
    inner: Mutex<WaitQueueInner>,
}

#[derive(Default)]
struct WaitQueueInner {
    queue: VecDeque<Waiter>,
}

struct Waiter {
    waker: Waker,
    bitset: u32,
    state: Arc<WaiterState>,
}

struct WaiterState {
    woken: AtomicBool,
    cancelled: AtomicBool,
    cleanup: Mutex<Option<FutexWaitCleanup>>,
}

impl WaiterState {
    fn new(cleanup: Option<FutexWaitCleanup>) -> Self {
        Self {
            woken: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            cleanup: Mutex::new(cleanup),
        }
    }

    fn set_cleanup_if_not_cancelled(&self, cleanup: FutexWaitCleanup) -> bool {
        let mut current = self.cleanup.lock();
        if self.cancelled.load(AtomicOrdering::SeqCst) {
            return false;
        }
        *current = Some(cleanup);
        true
    }

    fn remove_from_current_queue(state: &Arc<Self>) -> bool {
        let cleanup = state.cleanup.lock().clone();
        if let Some(cleanup) = cleanup {
            cleanup.table.remove_waiter(cleanup.key, state);
            true
        } else {
            false
        }
    }
}

struct WaitIfFuture<'a, F> {
    queue: &'a WaitQueue,
    bitset: u32,
    cleanup: Option<FutexWaitCleanup>,
    condition: Option<F>,
    state: Option<Arc<WaiterState>>,
}

impl<F: FnOnce() -> Result<bool, FutexAccessError> + Unpin> Future for WaitIfFuture<'_, F> {
    type Output = Result<bool, FutexAccessError>;

    fn poll(self: Pin<&mut Self>, cx: &mut core::task::Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        if let Some(condition) = this.condition.take() {
            let mut inner = this.queue.inner.lock();
            if !condition()? {
                return Poll::Ready(Ok(false));
            }

            let state = Arc::new(WaiterState::new(this.cleanup.clone()));
            inner.queue.push_back(Waiter {
                waker: cx.waker().clone(),
                bitset: this.bitset,
                state: state.clone(),
            });
            this.state = Some(state);
            return Poll::Pending;
        }

        let Some(state) = &this.state else {
            return Poll::Ready(Ok(true));
        };

        if state.woken.load(AtomicOrdering::SeqCst) {
            this.state = None;
            Poll::Ready(Ok(true))
        } else {
            let mut inner = this.queue.inner.lock();
            if let Some(waiter) = inner
                .queue
                .iter_mut()
                .find(|waiter| Arc::ptr_eq(&waiter.state, state))
            {
                waiter.waker = cx.waker().clone();
            }
            Poll::Pending
        }
    }
}

impl<F> Drop for WaitIfFuture<'_, F> {
    fn drop(&mut self) {
        if let Some(state) = &self.state {
            state.cancelled.store(true, AtomicOrdering::SeqCst);
            if !WaiterState::remove_from_current_queue(state) {
                self.queue.remove_waiter(state);
            }
        }
    }
}

/// Identifies where a queued waiter must be removed if its wait is cancelled.
#[derive(Clone)]
pub struct FutexWaitCleanup {
    table: Arc<FutexTable>,
    key: usize,
}

/// Absolute deadline used by a futex wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FutexWaitDeadline {
    /// Wait without a deadline.
    Infinite,
    /// Wait until an absolute monotonic deadline.
    Monotonic(TimeValue),
    /// Wait until an absolute realtime deadline.
    Realtime(TimeValue),
}

impl WaitQueue {
    /// Creates a new `WaitQueue`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Waits if the given condition is met.
    ///
    /// Returns `false` if the condition is not met and no actual waiting
    /// occurs.
    pub fn wait_if(
        &self,
        bitset: u32,
        timeout: Option<Duration>,
        condition: impl FnOnce() -> bool + Unpin,
    ) -> StarryResult<bool> {
        let deadline = timeout
            .map(|duration| {
                FutexWaitDeadline::Monotonic(monotonic_time().saturating_add(duration))
            })
            .unwrap_or(FutexWaitDeadline::Infinite);
        self.wait_if_with_cleanup(bitset, deadline, None, condition)
    }

    /// Waits with explicit futex-table cleanup metadata.
    ///
    /// This is used by futex requeue paths, where a waiter may be moved to a
    /// different wait queue before it times out or is interrupted.
    pub fn wait_if_with_cleanup(
        &self,
        bitset: u32,
        deadline: FutexWaitDeadline,
        cleanup: Option<FutexWaitCleanup>,
        condition: impl FnOnce() -> bool + Unpin,
    ) -> StarryResult<bool> {
        match self.wait_if_with_cleanup_nofault(bitset, deadline, cleanup, || Ok(condition())) {
            Ok(waited) => Ok(waited),
            Err(FutexAccessError::Operation(error)) => Err(error),
            Err(FutexAccessError::Fault | FutexAccessError::Retry) => {
                unreachable!("infallible wait condition returned a user access error")
            }
        }
    }

    /// Waits after checking a nofault condition while holding the queue lock.
    pub fn wait_if_with_cleanup_nofault(
        &self,
        bitset: u32,
        deadline: FutexWaitDeadline,
        cleanup: Option<FutexWaitCleanup>,
        condition: impl FnOnce() -> Result<bool, FutexAccessError> + Unpin,
    ) -> Result<bool, FutexAccessError> {
        let wait = WaitIfFuture {
            queue: self,
            bitset,
            cleanup,
            condition: Some(condition),
            state: None,
        };
        let timed = match deadline {
            FutexWaitDeadline::Infinite => {
                block_on(interruptible(future::timeout_at(None, wait)))
            }
            FutexWaitDeadline::Monotonic(deadline) => {
                block_on(interruptible(future::timeout_at(Some(deadline), wait)))
            }
            FutexWaitDeadline::Realtime(deadline) => block_on(interruptible(
                future::timeout_at_wall(Some(deadline), wait),
            )),
        }
        .map_err(|error| FutexAccessError::Operation(error.into()))?;
        timed.map_err(|error| FutexAccessError::Operation(error.into()))?
    }

    fn wake_locked(queue: &mut VecDeque<Waiter>, count: usize, mask: u32, wakers: &mut Vec<Waker>) {
        let base = wakers.len();
        queue.retain(|waiter| {
            if waiter.state.cancelled.load(AtomicOrdering::SeqCst) {
                false
            } else if wakers.len() - base >= count || (waiter.bitset & mask) == 0 {
                true
            } else {
                waiter.state.woken.store(true, AtomicOrdering::SeqCst);
                wakers.push(waiter.waker.clone());
                false
            }
        });
    }

    /// Wakes up at most `count` tasks whose bitset intersects with the given
    /// bitmask.
    pub fn wake(&self, count: usize, mask: u32) -> usize {
        let mut wakers = Vec::new();
        {
            let mut inner = self.inner.lock();
            Self::wake_locked(&mut inner.queue, count, mask, &mut wakers);
        }

        let woke = wakers.len();
        for waker in wakers {
            waker.wake();
        }
        woke
    }

    /// Serializes a FUTEX_WAKE_OP user RMW with both futex wait queues.
    pub fn wake_op(
        &self,
        wake_count: usize,
        target: &WaitQueue,
        wake2_count: usize,
        condition: impl FnOnce() -> Result<bool, FutexAccessError>,
    ) -> Result<usize, FutexAccessError> {
        let mut condition = Some(condition);
        let mut wakers = Vec::new();

        match core::ptr::from_ref(self).cmp(&core::ptr::from_ref(target)) {
            Ordering::Less => {
                let mut src = self.inner.lock();
                let mut dst = target.inner.lock_nested(NESTED_WAIT_QUEUE_LOCK_SUBCLASS);
                let wake_second = condition.take().expect("condition used once")()?;
                Self::wake_locked(&mut src.queue, wake_count, u32::MAX, &mut wakers);
                if wake_second {
                    Self::wake_locked(&mut dst.queue, wake2_count, u32::MAX, &mut wakers);
                }
            }
            Ordering::Greater => {
                let mut dst = target.inner.lock();
                let mut src = self.inner.lock_nested(NESTED_WAIT_QUEUE_LOCK_SUBCLASS);
                let wake_second = condition.take().expect("condition used once")()?;
                Self::wake_locked(&mut src.queue, wake_count, u32::MAX, &mut wakers);
                if wake_second {
                    Self::wake_locked(&mut dst.queue, wake2_count, u32::MAX, &mut wakers);
                }
            }
            Ordering::Equal => {
                let mut src = self.inner.lock();
                let wake_second = condition.take().expect("condition used once")()?;
                Self::wake_locked(&mut src.queue, wake_count, u32::MAX, &mut wakers);
                if wake_second {
                    Self::wake_locked(&mut src.queue, wake2_count, u32::MAX, &mut wakers);
                }
            }
        }

        let woke = wakers.len();
        for waker in wakers {
            waker.wake();
        }
        Ok(woke)
    }

    fn wake_requeue_locked(
        src: &mut VecDeque<Waiter>,
        dst: &mut VecDeque<Waiter>,
        wake_count: usize,
        wake_mask: u32,
        requeue_count: usize,
        target_cleanup: FutexWaitCleanup,
        wakers: &mut Vec<Waker>,
    ) -> usize {
        src.retain(|waiter| !waiter.state.cancelled.load(AtomicOrdering::SeqCst));

        let mut index = 0;
        while index < src.len() && wakers.len() < wake_count {
            if (src[index].bitset & wake_mask) == 0 {
                index += 1;
                continue;
            }

            let waiter = src.remove(index).expect("waiter index checked");
            waiter.state.woken.store(true, AtomicOrdering::SeqCst);
            wakers.push(waiter.waker);
        }

        let mut requeued = 0;
        while requeued < requeue_count {
            let Some(waiter) = src.pop_front() else {
                break;
            };
            if !waiter
                .state
                .set_cleanup_if_not_cancelled(target_cleanup.clone())
            {
                continue;
            }
            dst.push_back(waiter);
            requeued += 1;
        }
        wakers.len() + requeued
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
        let mut wakers = Vec::new();

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
                    &mut wakers,
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
                    &mut wakers,
                )
            }
            Ordering::Equal => {
                let mut src = self.inner.lock();
                if !condition.take().expect("condition used once")()? {
                    return Ok(None);
                }

                src.queue
                    .retain(|waiter| !waiter.state.cancelled.load(AtomicOrdering::SeqCst));
                let mut index = 0;
                while index < src.queue.len() && wakers.len() < wake_count {
                    if (src.queue[index].bitset & wake_mask) == 0 {
                        index += 1;
                        continue;
                    }

                    let waiter = src.queue.remove(index).expect("waiter index checked");
                    waiter.state.woken.store(true, AtomicOrdering::SeqCst);
                    wakers.push(waiter.waker);
                }
                wakers.len()
            }
        };

        for waker in wakers {
            waker.wake();
        }
        Ok(Some(count))
    }

    fn remove_waiter(&self, state: &Arc<WaiterState>) -> bool {
        let mut inner = self.inner.lock();
        inner
            .queue
            .retain(|waiter| !Arc::ptr_eq(&waiter.state, state));
        inner.queue.is_empty()
    }

    /// Checks if the wait queue is empty.
    ///
    /// O(1): reads the queue length only. This is called from `FutexGuard::Drop`
    /// while holding the (per-process) futex-table lock on EVERY futex op, so it must
    /// not scan — a prior `queue.retain(cancelled)` here made it O(n) under the table
    /// lock, i.e. an O(N²) collapse of contended futex throughput (schbench's tail).
    /// Cancelled waiters are already pruned by `wake` (its retain) and by each waiter's
    /// own `WaitIfFuture::Drop`, so dropping the scan here only delays a benign
    /// table-entry cleanup (also swept by the periodic `FutexTables` GC), never leaks.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().queue.is_empty()
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
        /// Stable backing-object identity and logical byte offset.
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
    pub fn new(aspace: &AddrSpace, address: usize, mode: FutexKeyMode) -> Self {
        if matches!(mode, FutexKeyMode::Auto)
            && let Some(identity) = aspace.shared_futex_identity(VirtAddr::from_usize(address))
        {
            return Self::Shared { identity };
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
    pub fn new_current(address: usize, mode: FutexKeyMode) -> StarryResult<Self> {
        if matches!(mode, FutexKeyMode::Private) {
            return Ok(Self::Private { address });
        }
        let curr = current();
        let aspace_arc = curr.as_thread().proc_data.pin_aspace()?;
        let aspace = aspace_arc.lock();
        Ok(Self::new(&aspace, address, mode))
    }

    /// Teardown variant that is anchored to the exiting process instead of
    /// whatever scheduler task is currently running on this CPU.
    pub fn new_for_process_teardown(proc_data: &ProcessData, address: usize) -> Option<Self> {
        let aspace_arc = proc_data.pin_aspace().ok()?;
        let aspace = aspace_arc.try_lock()?;
        Some(Self::new(&aspace, address, FutexKeyMode::Auto))
    }

    fn as_usize(&self) -> usize {
        match self {
            FutexKey::Private { address } => *address,
            FutexKey::Shared { identity } => identity.offset(),
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
        Self {
            wq: WaitQueue::new(),
        }
    }
}

/// A table mapping memory addresses to futex wait queues.
/// Number of lock shards in a per-process futex table. Mirrors Linux's
/// `futex_hash_bucket` array: futex ops on distinct addresses fall into distinct
/// buckets, so contended-futex throughput scales toward `ncpu` instead of
/// serializing all threads on one process-wide table lock.
const FUTEX_SHARDS: usize = 64;

pub struct FutexTable {
    buckets: [Mutex<HashMap<usize, Arc<FutexEntry>>>; FUTEX_SHARDS],
}

impl FutexTable {
    /// Creates a new `FutexTable`.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            buckets: core::array::from_fn(|_| Mutex::new(HashMap::new())),
        }
    }

    /// Selects the shard for a futex key via a Fibonacci hash (top bits after a
    /// multiplicative mix) so 4-byte-aligned user addresses spread evenly.
    #[inline]
    fn bucket(&self, key: usize) -> &Mutex<HashMap<usize, Arc<FutexEntry>>> {
        let h = (key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        &self.buckets[(h >> (64 - 6)) as usize % FUTEX_SHARDS]
    }

    /// Checks if the futex table is empty (all shards). Only called by the
    /// periodic table GC, not the hot path.
    pub fn is_empty(&self) -> bool {
        self.buckets.iter().all(|b| b.lock().is_empty())
    }

    /// Gets the wait queue associated with the given address.
    pub fn get(&self, key: &FutexKey) -> Option<FutexGuard<'_>> {
        let key = key.as_usize();
        let entry = self.bucket(key).lock().get(&key).cloned()?;
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
        let mut bucket = self.bucket(key).lock();
        let entry = bucket
            .entry(key)
            .or_insert_with(|| Arc::new(FutexEntry::new()));
        FutexGuard {
            table: self,
            key,
            inner: entry.clone(),
        }
    }

    /// Returns cleanup metadata for a waiter queued under `key`.
    pub fn cleanup_for(self: &Arc<Self>, key: &FutexKey) -> FutexWaitCleanup {
        FutexWaitCleanup {
            table: self.clone(),
            key: key.as_usize(),
        }
    }

    fn remove_waiter(&self, key: usize, state: &Arc<WaiterState>) {
        let mut bucket = self.bucket(key).lock();
        let should_remove = if let Some(entry) = bucket.get(&key) {
            entry.wq.remove_waiter(state) && Arc::strong_count(entry) == 1
        } else {
            false
        };
        if should_remove {
            bucket.remove(&key);
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
        let mut bucket = self.table.bucket(self.key).lock();
        // Re-check strong_count under lock — a concurrent get_or_insert may
        // have cloned the Arc in the meantime. The <= 2 threshold accounts
        // for the strong refs held by the table entry and this guard
        // (self.inner). If there are more refs, someone else is using the
        // entry, so we must not remove it from the table.
        if Arc::strong_count(&self.inner) <= 2 && self.inner.wq.is_empty() {
            bucket.remove(&self.key);
        }
    }
}

struct FutexTables {
    map: BTreeMap<SharedFutexRegion, Arc<FutexTable>>,
    operations: usize,
}
impl FutexTables {
    const fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            operations: 0,
        }
    }

    fn get_or_insert(&mut self, key: SharedFutexRegion) -> Arc<FutexTable> {
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

static SHARED_FUTEX_TABLES: Mutex<FutexTables> = Mutex::new(FutexTables::new());

/// Returns the futex table for the given key.
pub fn futex_table_for(key: &FutexKey) -> Arc<FutexTable> {
    let curr = current();
    futex_table_for_process(curr.as_thread().proc_data.as_ref(), key)
}

/// Returns the futex table for a key in a known process context.
pub fn futex_table_for_process(proc_data: &ProcessData, key: &FutexKey) -> Arc<FutexTable> {
    match key {
        FutexKey::Private { .. } => proc_data.futex_table.clone(),
        FutexKey::Shared { identity } => SHARED_FUTEX_TABLES
            .lock()
            .get_or_insert(identity.region()),
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use alloc::boxed::Box;
    use core::{cell::Cell, task::Context};

    use super::*;

    #[test]
    fn nofault_failure_is_transactional() {
        let wait_queue = WaitQueue::new();
        let mut wait = Box::pin(WaitIfFuture {
            queue: &wait_queue,
            bitset: u32::MAX,
            cleanup: None,
            condition: Some(|| Err(FutexAccessError::Fault)),
            state: None,
        });
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            wait.as_mut().poll(&mut context),
            Poll::Ready(Err(FutexAccessError::Fault))
        ));
        assert!(wait_queue.is_empty());

        let source = WaitQueue::new();
        let target = WaitQueue::new();
        let state = Arc::new(WaiterState::new(None));
        source.inner.lock().queue.push_back(Waiter {
            waker: Waker::noop().clone(),
            bitset: u32::MAX,
            state: state.clone(),
        });

        assert!(matches!(
            source.wake_op(1, &target, 1, || Err(FutexAccessError::Fault)),
            Err(FutexAccessError::Fault)
        ));
        assert_eq!(source.inner.lock().queue.len(), 1);
        assert!(!state.woken.load(AtomicOrdering::SeqCst));

        let target_cleanup = FutexWaitCleanup {
            table: Arc::new(FutexTable::new()),
            key: 0x2000,
        };
        assert!(matches!(
            source.wake_requeue_if(1, u32::MAX, 1, target_cleanup, &target, || {
                Err(FutexAccessError::Retry)
            }),
            Err(FutexAccessError::Retry)
        ));
        assert_eq!(source.inner.lock().queue.len(), 1);
        assert!(target.is_empty());
        assert!(!state.woken.load(AtomicOrdering::SeqCst));

        let attempts = Cell::new(0);
        let fault_in_unlocked = Cell::new(false);
        let result = retry_futex_nofault_with(
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() == 1 {
                    source.wake_op(0, &target, 0, || Err(FutexAccessError::Fault))
                } else {
                    source.wake_op(0, &target, 0, || Ok(false))
                }
            },
            || {
                let source_unlocked = !unsafe { source.inner.raw() }.is_owned_by_current();
                let target_unlocked = !unsafe { target.inner.raw() }.is_owned_by_current();
                fault_in_unlocked.set(source_unlocked && target_unlocked);
                Ok(())
            },
            || {},
        );

        assert!(matches!(result, Ok(0)));
        assert_eq!(attempts.get(), 2);
        assert!(fault_in_unlocked.get());
        assert_eq!(source.inner.lock().queue.len(), 1);
        assert!(!state.woken.load(AtomicOrdering::SeqCst));
    }
}

#[cfg(all(test, axtest))]
mod axtests {
    use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
    use ax_runtime::hal::paging::MappingFlags;

    use super::*;
    use crate::mm::{MappingOperation, SharedMemoryObject};

    fn shared_offset(key: FutexKey) -> usize {
        match key {
            FutexKey::Shared { identity } => identity.offset(),
            FutexKey::Private { .. } => panic!("shared mapping produced a private futex key"),
        }
    }

    #[axtest::axtest]
    fn shared_futex_key_survives_vma_split() {
        let start = VirtAddr::from_usize(0x7100_0000);
        let second_page = start.checked_add(PAGE_SIZE_4K).unwrap();
        let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
        let pages = Arc::new(
            SharedMemoryObject::allocate(PAGE_SIZE_4K * 2, PAGE_SIZE_4K).unwrap(),
        );
        let mut aspace = AddrSpace::new_empty(start, PAGE_SIZE_4K * 2).unwrap();
        aspace
            .map(
                start,
                PAGE_SIZE_4K * 2,
                flags,
                false,
                MappingOperation::new_shared(start, pages),
            )
            .unwrap();

        let before = shared_offset(FutexKey::new(
            &aspace,
            second_page.as_usize(),
            FutexKeyMode::Auto,
        ));
        aspace
            .protect(
                second_page,
                PAGE_SIZE_4K,
                MappingFlags::READ | MappingFlags::USER,
            )
            .unwrap();
        let after = shared_offset(FutexKey::new(
            &aspace,
            second_page.as_usize(),
            FutexKeyMode::Auto,
        ));

        aspace.reset_uninstalled_for_loader().unwrap();
        assert_eq!(before, after, "VMA split changed shared futex identity");
    }
}
