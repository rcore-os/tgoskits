//! Futex implementation.

use alloc::{
    collections::{btree_map::BTreeMap, vec_deque::VecDeque},
    sync::{Arc, Weak},
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

use ax_errno::AxResult;
use ax_memory_addr::VirtAddr;
use ax_sync::Mutex;
use ax_task::{
    current,
    future::{self, block_on, interruptible},
};
use hashbrown::HashMap;

use crate::{
    mm::{AddrSpace, Backend, SharedPages},
    task::AsThread,
};

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
}

impl WaiterState {
    fn new() -> Self {
        Self {
            woken: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        }
    }
}

struct WaitIfFuture<'a, F> {
    queue: &'a WaitQueue,
    bitset: u32,
    condition: Option<F>,
    state: Option<Arc<WaiterState>>,
}

impl<F: FnOnce() -> bool + Unpin> Future for WaitIfFuture<'_, F> {
    type Output = AxResult<bool>;

    fn poll(self: Pin<&mut Self>, cx: &mut core::task::Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        if let Some(condition) = this.condition.take() {
            let mut inner = this.queue.inner.lock();
            if !condition() {
                return Poll::Ready(Ok(false));
            }

            let state = Arc::new(WaiterState::new());
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
        }
        if let Some(state) = &self.state {
            self.queue
                .inner
                .lock()
                .queue
                .retain(|waiter| !Arc::ptr_eq(&waiter.state, state));
        }
    }
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
    ) -> AxResult<bool> {
        block_on(interruptible(future::timeout(
            timeout,
            WaitIfFuture {
                queue: self,
                bitset,
                condition: Some(condition),
                state: None,
            },
        )))??
    }

    /// Wakes up at most `count` tasks whose bitset intersects with the given
    /// bitmask.
    pub fn wake(&self, count: usize, mask: u32) -> usize {
        let wakers = {
            let mut inner = self.inner.lock();
            let mut wakers = Vec::new();

            inner.queue.retain(|waiter| {
                if waiter.state.cancelled.load(AtomicOrdering::SeqCst) {
                    false
                } else if wakers.len() >= count || (waiter.bitset & mask) == 0 {
                    true
                } else {
                    waiter.state.woken.store(true, AtomicOrdering::SeqCst);
                    wakers.push(waiter.waker.clone());
                    false
                }
            });
            wakers
        };

        let woke = wakers.len();
        for waker in wakers {
            waker.wake();
        }
        woke
    }

    /// Checks if the wait queue is empty.
    pub fn is_empty(&self) -> bool {
        let mut inner = self.inner.lock();
        inner
            .queue
            .retain(|waiter| !waiter.state.cancelled.load(AtomicOrdering::SeqCst));
        inner.queue.is_empty()
    }

    /// Requeue at most `count` tasks to the target wait queue.
    pub fn requeue(&self, count: usize, target: &WaitQueue) -> usize {
        fn requeue_locked(
            src: &mut VecDeque<Waiter>,
            dst: &mut VecDeque<Waiter>,
            count: usize,
        ) -> usize {
            src.retain(|waiter| !waiter.state.cancelled.load(AtomicOrdering::SeqCst));
            let count = count.min(src.len());
            dst.extend(src.drain(..count));
            count
        }

        match core::ptr::from_ref(self).cmp(&core::ptr::from_ref(target)) {
            Ordering::Less => {
                let mut src = self.inner.lock();
                let mut dst = target.inner.lock();
                requeue_locked(&mut src.queue, &mut dst.queue, count)
            }
            Ordering::Greater => {
                let mut dst = target.inner.lock();
                let mut src = self.inner.lock();
                requeue_locked(&mut src.queue, &mut dst.queue, count)
            }
            Ordering::Equal => 0,
        }
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

impl FutexKey {
    /// Creates a new `FutexKey`.
    pub fn new(aspace: &AddrSpace, address: usize) -> Self {
        if let Some(area) = aspace.find_area(VirtAddr::from_usize(address)) {
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
    pub fn new_current(address: usize) -> Self {
        let curr = current();
        let aspace_arc = curr.as_thread().proc_data.aspace();
        let aspace = aspace_arc.lock();
        Self::new(&aspace, address)
    }

    /// Best-effort variant for teardown paths that may be reached after a
    /// faultable user-memory access.
    pub fn new_current_teardown(address: usize) -> Self {
        let curr = current();
        let aspace_arc = curr.as_thread().proc_data.aspace();
        let Some(aspace) = aspace_arc.try_lock() else {
            return Self::Private { address };
        };
        Self::new(&aspace, address)
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
        Self {
            wq: WaitQueue::new(),
        }
    }
}

/// A table mapping memory addresses to futex wait queues.
pub struct FutexTable(Mutex<HashMap<usize, Arc<FutexEntry>>>);

impl FutexTable {
    /// Creates a new `FutexTable`.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
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

static SHARED_FUTEX_TABLES: Mutex<FutexTables> = Mutex::new(FutexTables::new());

/// Returns the futex table for the given key.
pub fn futex_table_for(key: &FutexKey) -> Arc<FutexTable> {
    match key {
        FutexKey::Private { .. } => current().as_thread().proc_data.futex_table.clone(),
        FutexKey::Shared { region, .. } => {
            let ptr = match region {
                Ok(pages) => Weak::as_ptr(pages) as usize,
                Err(key) => Weak::as_ptr(key) as usize,
            };
            SHARED_FUTEX_TABLES.lock().get_or_insert(ptr)
        }
    }
}
