//! Futex implementation.

use alloc::{
    collections::vec_deque::VecDeque,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    future::poll_fn,
    ops::Deref,
    sync::atomic::AtomicBool,
    task::{Poll, Waker},
    time::Duration,
};

use axerrno::AxResult;
use axmm::{
    AddrSpace,
    backend::{Backend, SharedPages},
};
use axsync::Mutex;
use axtask::{
    current,
    future::{self, block_on, interruptible},
};
use hashbrown::HashMap;
use kspin::SpinNoIrq;
use memory_addr::VirtAddr;

use crate::task::AsThread;

/// Wait queue used by futex.
#[derive(Default)]
pub struct WaitQueue {
    queue: SpinNoIrq<VecDeque<(Waker, u32)>>,
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
        condition: impl FnOnce() -> bool,
    ) -> AxResult<bool> {
        let mut condition = Some(condition);
        block_on(interruptible(future::timeout(
            timeout,
            poll_fn(|cx| {
                if let Some(cond) = condition.take() {
                    let mut queue = self.queue.lock();
                    if !cond() {
                        Poll::Ready(Ok(false))
                    } else {
                        queue.push_back((cx.waker().clone(), bitset));
                        Poll::Pending
                    }
                } else {
                    Poll::Ready(Ok(true))
                }
            }),
        )))??
    }

    /// Wakes up at most `count` tasks whose bitset intersects with the given
    /// bitmask.
    pub fn wake(&self, count: usize, mask: u32) -> usize {
        let mut woke = 0;
        self.queue.lock().retain(|(waker, bitset)| {
            if woke >= count || (bitset & mask) == 0 {
                true
            } else {
                waker.wake_by_ref();
                woke += 1;
                false
            }
        });
        woke
    }

    /// Checks if the wait queue is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.lock().is_empty()
    }

    /// Requeue at most `count` tasks to the target wait queue.
    pub fn requeue(&self, mut count: usize, target: &WaitQueue) -> usize {
        let tasks: Vec<_> = {
            let mut wq = self.queue.lock();
            count = count.min(wq.len());
            wq.drain(..count).collect()
        };
        if !tasks.is_empty() {
            let mut wq = target.queue.lock();
            wq.extend(tasks);
        }
        count
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
        Self::new(&current().as_thread().proc_data.aspace.lock(), address)
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

    /// Used by robust list, indicates if the owner of this futex is dead.
    pub owner_dead: AtomicBool,
}

impl FutexEntry {
    fn new() -> Self {
        Self {
            wq: WaitQueue::new(),
            owner_dead: AtomicBool::new(false),
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
    pub fn get(&self, key: &FutexKey) -> Option<FutexGuard> {
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
    pub fn get_or_insert(&self, key: &FutexKey) -> FutexGuard {
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
        if Arc::strong_count(&self.inner) <= 2 && self.inner.wq.is_empty() {
            self.table.0.lock().remove(&self.key);
        }
    }
}
