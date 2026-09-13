//! Cache-only inspection with no device access or OS synchronization.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use spinning_top::Spinlock;

use super::*;

pub(super) struct SharedInodes {
    pub(super) entries: Spinlock<BTreeMap<InodeCacheKey, CachedInode>>,
    paused: AtomicUsize,
    alive: AtomicBool,
}

impl SharedInodes {
    pub(super) fn new(entries: BTreeMap<InodeCacheKey, CachedInode>) -> Self {
        Self {
            entries: Spinlock::new(entries),
            paused: AtomicUsize::new(0),
            alive: AtomicBool::new(true),
        }
    }
}

/// Reads complete cached inodes without acquiring the mounted filesystem.
/// Busy, missing, transaction-private and retired state all return `None`.
/// Callers needing an authoritative answer then use their serialized path.
#[derive(Clone)]
pub struct InodeCacheReader {
    shared: Arc<SharedInodes>,
}

impl InodeCacheReader {
    /// Copies an existing inode without allocation, waiting, loading or I/O.
    /// The caller must separately retain the inode's allocation lifetime.
    pub fn try_get(&self, inode: InodeNumber) -> Option<Ext4Inode> {
        let entries = self.shared.entries.try_lock()?;
        if !self.shared.alive.load(Ordering::Acquire)
            || self.shared.paused.load(Ordering::Acquire) != 0
        {
            return None;
        }
        entries.get(&inode).map(|cached| cached.inode)
    }
}

impl core::fmt::Debug for InodeCacheReader {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InodeCacheReader")
            .field("alive", &self.shared.alive.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

/// Keeps partially updated or rolled-back metadata invisible to readers.
pub(crate) struct CacheReadPause(Arc<SharedInodes>);

impl Drop for CacheReadPause {
    fn drop(&mut self) {
        // All map publications/rollback precede reopening read visibility.
        self.0.paused.fetch_sub(1, Ordering::Release);
    }
}

impl InodeCache {
    /// Creates a read-only view of this owner, not a second metadata cache.
    pub fn reader(&self) -> InodeCacheReader {
        InodeCacheReader {
            shared: self.cache.clone(),
        }
    }

    pub(crate) fn pause_readers(&self) -> CacheReadPause {
        self.cache
            .paused
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .expect("metadata nesting cannot exhaust the address space");
        // A reader that observed zero already owns entries and completes its
        // old snapshot before any mutation can acquire that same map lock.
        CacheReadPause(self.cache.clone())
    }

    pub(crate) fn restore_snapshot(&mut self, snapshot: Self) {
        self.invalidate_all_reads();
        let entries = core::mem::take(&mut *snapshot.cache.entries.lock());
        let previous = core::mem::replace(&mut *self.cache.entries.lock(), entries);
        self.max_entries = snapshot.max_entries;
        self.access_counter = snapshot.access_counter;
        self.inode_size = snapshot.inode_size;
        // Retain our original Arc and pause depth. Readers must follow the
        // restored contents, not the temporary snapshot owner's identity.
        drop(previous);
    }
}

impl Clone for InodeCache {
    fn clone(&self) -> Self {
        Self {
            cache: Arc::new(SharedInodes::new(self.cache.entries.lock().clone())),
            max_entries: self.max_entries,
            access_counter: self.access_counter,
            inode_size: self.inode_size,
            pending_reads: BTreeMap::new(),
        }
    }
}

impl Drop for InodeCache {
    fn drop(&mut self) {
        self.cache.alive.store(false, Ordering::Release);
    }
}
