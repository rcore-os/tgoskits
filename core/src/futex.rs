//! Futex implementation.

use core::ops::Deref;

use alloc::{collections::btree_map::BTreeMap, sync::Arc};
use axsync::Mutex;
use axtask::{TaskExtRef, WaitQueue, current};

/// A table mapping memory addresses to futex wait queues.
pub struct FutexTable(Mutex<BTreeMap<usize, Arc<WaitQueue>>>);
impl FutexTable {
    /// Creates a new `FutexTable`.
    pub fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    /// Gets the wait queue associated with the given address.
    pub fn get(&self, addr: usize) -> Option<WaitQueueGuard> {
        let wq = self.0.lock().get(&addr).cloned()?;
        Some(WaitQueueGuard {
            key: addr,
            inner: wq,
        })
    }

    /// Gets the wait queue associated with the given address, or inserts a a
    /// new one if it doesn't exist.
    pub fn get_or_insert(&self, addr: usize) -> WaitQueueGuard {
        let mut table = self.0.lock();
        let wq = table
            .entry(addr)
            .or_insert_with(|| Arc::new(WaitQueue::new()));
        WaitQueueGuard {
            key: addr,
            inner: wq.clone(),
        }
    }
}

#[doc(hidden)]
pub struct WaitQueueGuard {
    key: usize,
    inner: Arc<WaitQueue>,
}
impl Deref for WaitQueueGuard {
    type Target = Arc<WaitQueue>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl Drop for WaitQueueGuard {
    fn drop(&mut self) {
        let curr = current();
        let mut table = curr.task_ext().process_data().futex_table.0.lock();
        if Arc::strong_count(&self.inner) == 1 && self.inner.is_empty() {
            table.remove(&self.key);
        }
    }
}
