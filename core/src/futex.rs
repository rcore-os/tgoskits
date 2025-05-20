//! Futex implementation.

use core::{ops::Deref, sync::atomic::AtomicBool};

use alloc::{collections::btree_map::BTreeMap, sync::Arc};
use axsync::Mutex;
use axtask::{TaskExtRef, WaitQueue, current};

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
pub struct FutexTable(Mutex<BTreeMap<usize, Arc<FutexEntry>>>);
impl FutexTable {
    /// Creates a new `FutexTable`.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    /// Gets the wait queue associated with the given address.
    pub fn get(&self, addr: usize) -> Option<FutexGuard> {
        let entry = self.0.lock().get(&addr).cloned()?;
        Some(FutexGuard {
            key: addr,
            inner: entry,
        })
    }

    /// Gets the wait queue associated with the given address, or inserts a a
    /// new one if it doesn't exist.
    pub fn get_or_insert(&self, addr: usize) -> FutexGuard {
        let mut table = self.0.lock();
        let entry = table
            .entry(addr)
            .or_insert_with(|| Arc::new(FutexEntry::new()));
        FutexGuard {
            key: addr,
            inner: entry.clone(),
        }
    }
}

#[doc(hidden)]
pub struct FutexGuard {
    key: usize,
    inner: Arc<FutexEntry>,
}
impl Deref for FutexGuard {
    type Target = Arc<FutexEntry>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl Drop for FutexGuard {
    fn drop(&mut self) {
        let curr = current();
        let mut table = curr.task_ext().process_data().futex_table.0.lock();
        if Arc::strong_count(&self.inner) == 1 && self.inner.wq.is_empty() {
            table.remove(&self.key);
        }
    }
}
