//! Shared page-cache state, reclaim policy, and inode cache identity.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
#[cfg(feature = "ext4")]
use alloc::{collections::BTreeMap, sync::Weak};
#[cfg(feature = "vfs")]
use core::mem;
#[cfg(feature = "vfs")]
use core::sync::atomic::AtomicBool;
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicU64, Ordering},
};

use axfs_ng_vfs::{FileNode, VfsError, VfsResult};
#[cfg(feature = "ext4")]
use axfs_ng_vfs::{FilesystemOps, Location};
use intrusive_collections::{LinkedList, LinkedListAtomicLink, intrusive_adapter};
use lru::LruCache;

#[cfg(feature = "ext4")]
use crate::os::sync::SpinMutex;
use crate::{
    file::page::PageCache,
    os::{
        memory::PAGE_SIZE,
        sync::{PiMutex, PiMutexGuard},
    },
};

const DISK_PAGE_CACHE_CAP: usize = 512;
const CACHE_PRESSURE_WRITEBACK_BATCH: usize = 256;
#[cfg(feature = "vfs")]
const REGISTRY_RECLAIM_BATCH: usize = 64;

#[cfg(feature = "ext4")]
pub(super) type CachedFileKey = (usize, u64);
#[cfg(feature = "ext4")]
type InodeCacheIndex = BTreeMap<CachedFileKey, Weak<CachedFileShared>>;

#[cfg(feature = "ext4")]
static CACHED_FILE_BY_INODE: spin::LazyLock<SpinMutex<InodeCacheIndex>> =
    spin::LazyLock::new(|| SpinMutex::new(BTreeMap::new()));

/// Eviction listener callback. Returns `true` if the listener successfully
/// invalidated all mappings for the evicted page.
type EvictListenerFn = Arc<dyn Fn(u32, &PageCache) -> bool + Send + Sync>;
type WritebackProtectListenerFn = Arc<dyn Fn(u32) -> bool + Send + Sync>;

struct DirtyPageSnapshot {
    pn: u32,
    generation: u64,
    data: Box<[u8]>,
    len: usize,
}

struct EvictListener {
    listener: EvictListenerFn,
    writeback_protect: WritebackProtectListenerFn,
    link: LinkedListAtomicLink,
}

intrusive_adapter!(EvictListenerAdapter = Box<EvictListener>: EvictListener { link: LinkedListAtomicLink });

pub(super) struct CachedFileShared {
    pub(super) page_cache: PiMutex<LruCache<u32, PageCache>>,
    pub(super) io_lock: PiMutex<()>,
    evict_listeners: PiMutex<LinkedList<EvictListenerAdapter>>,
    backing: Option<FileNode>,
    len: AtomicU64,
}

impl CachedFileShared {
    pub(super) fn new(len: u64, backing: FileNode) -> Self {
        Self {
            page_cache: PiMutex::new(LruCache::new(
                NonZeroUsize::new(DISK_PAGE_CACHE_CAP).unwrap(),
            )),
            io_lock: PiMutex::new(()),
            evict_listeners: PiMutex::new(LinkedList::default()),
            backing: Some(backing),
            len: AtomicU64::new(len),
        }
    }

    pub(super) fn new_unbounded(len: u64) -> Self {
        Self {
            page_cache: PiMutex::new(LruCache::unbounded()),
            io_lock: PiMutex::new(()),
            evict_listeners: PiMutex::new(LinkedList::default()),
            backing: None,
            len: AtomicU64::new(len),
        }
    }

    pub(super) fn len(&self) -> u64 {
        self.len.load(Ordering::Acquire)
    }

    pub(super) fn update_len_max(&self, len: u64) {
        let mut current = self.len();
        while len > current {
            match self
                .len
                .compare_exchange_weak(current, len, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    pub(super) fn set_len(&self, len: u64) {
        self.len.store(len, Ordering::Release);
    }

    pub(super) fn add_page_listener<E, W>(&self, evict: E, writeback_protect: W) -> usize
    where
        E: Fn(u32, &PageCache) -> bool + Send + Sync + 'static,
        W: Fn(u32) -> bool + Send + Sync + 'static,
    {
        let listener = Box::new(EvictListener {
            listener: Arc::new(evict),
            writeback_protect: Arc::new(writeback_protect),
            link: LinkedListAtomicLink::new(),
        });
        let handle = listener.as_ref() as *const EvictListener as usize;
        self.evict_listeners.lock().push_back(listener);
        handle
    }

    /// Removes a listener previously registered on this shared cache.
    ///
    /// # Safety
    ///
    /// `handle` must come from [`Self::add_page_listener`] on this object and
    /// must not already have been removed.
    pub(super) unsafe fn remove_page_listener(&self, handle: usize) {
        let mut listeners = self.evict_listeners.lock();
        let mut cursor = unsafe { listeners.cursor_mut_from_ptr(handle as *const EvictListener) };
        cursor.remove();
    }

    pub(super) fn notify_page_eviction(&self, pn: u32, page: &PageCache) -> bool {
        let listeners = self
            .evict_listeners
            .lock()
            .iter()
            .map(|listener| listener.listener.clone())
            .collect::<Vec<_>>();
        listeners.iter().all(|listener| listener(pn, page))
    }

    /// Makes one cache slot available while preserving bounded, run-based writeback.
    ///
    /// The returned guard keeps cached-file I/O serialized until the caller inserts its page.
    /// Dirty LRU pages are protected without holding `io_lock`, then written as one contiguous
    /// batch without forcing backing-store durability. This mirrors Linux page-cache pressure
    /// writeback: reclaim drives writeback, while `fsync` remains an explicit operation.
    pub(super) fn prepare_page_insert(&self, pn: u32) -> VfsResult<PiMutexGuard<'_, ()>> {
        let io = self.io_lock.lock();
        self.prepare_page_insert_locked(pn, io)
    }

    pub(super) fn prepare_page_insert_locked<'a>(
        &'a self,
        pn: u32,
        io: PiMutexGuard<'a, ()>,
    ) -> VfsResult<PiMutexGuard<'a, ()>> {
        let (file_len, dirty_keys) = {
            let mut cache = self.page_cache.lock();
            if cache.contains(&pn) || cache.len() < cache.cap().get() {
                return Ok(io);
            }
            if !cache.peek_lru().is_some_and(|(_, page)| page.dirty) {
                return Ok(io);
            }

            let file_len = self.len();
            let mut dirty_keys = Vec::with_capacity(CACHE_PRESSURE_WRITEBACK_BATCH);
            for (&candidate_pn, page) in cache.iter_mut().rev() {
                if !page.dirty || page.writeback_protecting {
                    break;
                }
                page.writeback_protecting = true;
                page.dirty_during_writeback = false;
                dirty_keys.push(candidate_pn);
                if dirty_keys.len() == CACHE_PRESSURE_WRITEBACK_BATCH {
                    break;
                }
            }
            (file_len, dirty_keys)
        };
        drop(io);

        if dirty_keys.is_empty() {
            return Err(VfsError::ResourceBusy);
        }

        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;

        let io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        Ok(io)
    }

    fn backing(&self) -> VfsResult<&FileNode> {
        self.backing.as_ref().ok_or(VfsError::InvalidInput)
    }

    pub(super) fn write_backing_all_at(&self, mut data: &[u8], mut offset: u64) -> VfsResult<()> {
        let backing = self.backing()?;
        while !data.is_empty() {
            let written = backing.write_at(data, offset)?;
            if written == 0 {
                return Err(VfsError::WriteZero);
            }
            if written > data.len() {
                return Err(VfsError::BadState);
            }
            data = &data[written..];
            offset = offset
                .checked_add(written as u64)
                .ok_or(VfsError::InvalidInput)?;
        }
        Ok(())
    }

    pub(super) fn writeback(&self) -> VfsResult<alloc::vec::Vec<u32>> {
        let (file_len, dirty_keys) = self.begin_writeback_all_dirty();
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(false)?;
        Ok(dirty_keys)
    }

    pub(super) fn writeback_pages(&self, pns: &[u32]) -> VfsResult<()> {
        let (file_len, dirty_keys) = self.begin_writeback_pages(pns);
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(false)?;
        Ok(())
    }

    pub(super) fn sync(&self, data_only: bool) -> VfsResult<()> {
        let (file_len, dirty_keys) = self.begin_writeback_all_dirty();
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(data_only)?;
        Ok(())
    }

    #[cfg(feature = "vfs")]
    fn writeback_dirty_for_global_sync(&self) -> VfsResult<()> {
        let (file_len, dirty_keys) = self.begin_writeback_all_dirty();
        if dirty_keys.is_empty() {
            return Ok(());
        }
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result
    }

    #[cfg(feature = "vfs")]
    fn has_dirty_pages(&self) -> bool {
        self.page_cache.lock().iter().any(|(_, page)| page.dirty)
    }

    fn begin_writeback_all_dirty(&self) -> (u64, Vec<u32>) {
        self.begin_writeback(None)
    }

    fn begin_writeback_pages(&self, pns: &[u32]) -> (u64, Vec<u32>) {
        self.begin_writeback(Some(pns))
    }

    fn begin_writeback(&self, requested: Option<&[u32]>) -> (u64, Vec<u32>) {
        let _io = self.io_lock.lock();
        let file_len = self.len();
        let mut requested_pns = requested.map(|pns| pns.to_vec());
        if let Some(pns) = requested_pns.as_mut() {
            pns.sort_unstable();
            pns.dedup();
        }
        let mut guard = self.page_cache.lock();
        let dirty_keys = guard
            .iter_mut()
            .filter_map(|(&pn, page)| {
                if !page.dirty {
                    return None;
                }
                if let Some(requested) = requested_pns.as_ref()
                    && requested.binary_search(&pn).is_err()
                {
                    return None;
                }
                let page_start = pn as u64 * PAGE_SIZE as u64;
                let len = file_len.saturating_sub(page_start).min(PAGE_SIZE as u64);
                if len == 0 {
                    return None;
                }
                page.writeback_protecting = true;
                page.dirty_during_writeback = false;
                Some(pn)
            })
            .collect();
        (file_len, dirty_keys)
    }

    fn writeback_page_runs(&self, file_len: u64, pns: &[u32]) -> VfsResult<()> {
        let mut snapshots = self.snapshot_dirty_pages(file_len, pns)?;
        snapshots.sort_by_key(|page| page.pn);

        let mut run_start = 0;
        while run_start < snapshots.len() {
            let mut run_end = run_start + 1;
            while run_end < snapshots.len()
                && snapshots[run_end].pn == snapshots[run_end - 1].pn + 1
                && snapshots[run_end - 1].len == PAGE_SIZE
            {
                run_end += 1;
            }

            let offset = snapshots[run_start].pn as u64 * PAGE_SIZE as u64;
            let run_len = snapshots[run_start..run_end]
                .iter()
                .map(|page| page.len)
                .sum();
            let mut data = alloc::vec::Vec::with_capacity(run_len);
            for page in &snapshots[run_start..run_end] {
                data.extend_from_slice(&page.data[..page.len]);
            }
            self.write_backing_all_at(&data, offset)?;

            {
                let mut guard = self.page_cache.lock();
                for page in &snapshots[run_start..run_end] {
                    if let Some(current) = guard.peek_mut(&page.pn)
                        && current.dirty
                        && current.dirty_generation == page.generation
                        && !current.dirty_during_writeback
                    {
                        current.dirty = false;
                    }
                }
            }

            run_start = run_end;
        }
        Ok(())
    }

    fn snapshot_dirty_pages(
        &self,
        file_len: u64,
        pns: &[u32],
    ) -> VfsResult<alloc::vec::Vec<DirtyPageSnapshot>> {
        let mut snapshots = alloc::vec::Vec::new();
        let mut guard = self.page_cache.lock();
        for pn in pns {
            let Some(page) = guard.peek_mut(pn) else {
                continue;
            };
            if !page.dirty {
                continue;
            }
            let page_start = *pn as u64 * PAGE_SIZE as u64;
            let len = file_len.saturating_sub(page_start).min(PAGE_SIZE as u64) as usize;
            if len == 0 {
                continue;
            }
            snapshots.push(DirtyPageSnapshot {
                pn: *pn,
                generation: page.dirty_generation,
                data: page.data()[..len].to_vec().into_boxed_slice(),
                len,
            });
        }
        Ok(snapshots)
    }

    fn protect_dirty_pages_before_writeback(&self, pns: &[u32]) -> VfsResult<()> {
        let listeners = self.writeback_protect_listeners();
        for pn in pns {
            for listener in &listeners {
                if !(listener)(*pn) {
                    return Err(VfsError::ResourceBusy);
                }
            }
        }
        Ok(())
    }

    fn writeback_protect_listeners(&self) -> Vec<WritebackProtectListenerFn> {
        self.evict_listeners
            .lock()
            .iter()
            .map(|listener| listener.writeback_protect.clone())
            .collect()
    }

    fn cancel_writeback_tracking(&self, pns: &[u32]) {
        let _io = self.io_lock.lock();
        self.finish_writeback_tracking(pns);
    }

    fn finish_writeback_tracking(&self, pns: &[u32]) {
        let mut guard = self.page_cache.lock();
        for pn in pns {
            if let Some(page) = guard.peek_mut(pn) {
                page.writeback_protecting = false;
                page.dirty_during_writeback = false;
            }
        }
    }

    #[cfg(test)]
    fn invoke_writeback_protect_for_test(&self, pns: &[u32]) -> VfsResult<()> {
        self.protect_dirty_pages_before_writeback(pns)
    }

    #[cfg(test)]
    fn io_lock_is_free_for_test(&self) -> bool {
        self.io_lock.try_lock().is_some()
    }

    #[cfg(test)]
    fn listener_lock_is_free_for_test(&self) -> bool {
        self.evict_listeners.try_lock().is_some()
    }

    /// Scan the LRU and evict up to `max` clean pages.
    ///
    /// Two-phase eviction:
    /// 1. Under `page_cache` lock: identify clean pages, pop them from the
    ///    cache, and move them into a local buffer.
    /// 2. Outside `page_cache` lock: invoke evict listeners.  If all
    ///    listeners confirm the PTE unmap, the page is dropped (freeing its
    ///    physical frame).  If any listener cannot unmap (e.g., AddrSpace
    ///    lock contention), the page is re-inserted into the cache to
    ///    prevent use-after-free.
    ///
    /// Returns the number of pages successfully evicted.
    ///
    /// # Lock ordering
    ///
    /// Reclaim takes `io_lock` with `try_lock` before touching `page_cache`.
    /// It retains that I/O reservation until each popped page is either
    /// accepted or reinserted, preventing a concurrent cache miss from
    /// installing a second frame for the same page number. Listener callbacks
    /// run without `page_cache` or `evict_listeners` held.
    #[cfg(feature = "vfs")]
    fn try_evict_clean_pages(&self, max: usize) -> usize {
        let limit = max.min(256);
        let Some(_io) = self.io_lock.try_lock() else {
            return 0;
        };

        // Phase 1: Pop clean pages from LRU under page_cache lock.
        // Two-pass: first collect page numbers (borrows cache immutably),
        // then pop by number (borrows cache mutably).
        let mut pending: Vec<(u32, PageCache)> = Vec::new();
        {
            let Some(mut cache) = self.page_cache.try_lock() else {
                return 0;
            };
            let mut to_pop = [0u32; 256];
            let mut cnt = 0;
            for (&pn, page) in cache.iter().rev() {
                if !page.dirty && cnt < limit {
                    to_pop[cnt] = pn;
                    cnt += 1;
                }
            }
            for &pn in to_pop[..cnt].iter() {
                if let Some(page) = cache.pop(&pn) {
                    pending.push((pn, page));
                }
            }
        } // page_cache lock released

        // Phase 2: Invoke listeners outside page_cache lock.
        let mut evicted = 0;
        for (pn, page) in pending.into_iter() {
            if self.notify_page_eviction(pn, &page) {
                // All listeners confirmed unmap — drop page (frees physical frame).
                drop(page);
                evicted += 1;
            } else {
                // Listener could not unmap (e.g., AddrSpace lock contention).
                // Re-insert page into cache to avoid freeing a physical frame
                // that still has live PTEs pointing to it.
                let mut cache = self.page_cache.lock();
                cache.put(pn, page);
            }
        }
        evicted
    }
}

#[cfg(feature = "vfs")]
struct ReclaimGuard;

#[cfg(feature = "vfs")]
impl Drop for ReclaimGuard {
    fn drop(&mut self) {
        RECLAIM_IN_PROGRESS.store(false, Ordering::Release);
    }
}

#[cfg(feature = "vfs")]
static GLOBAL_CACHED_FILES: ax_kspin::SpinRwLock<alloc::vec::Vec<Arc<CachedFileShared>>> =
    ax_kspin::SpinRwLock::new(alloc::vec::Vec::new());

#[cfg(feature = "vfs")]
static GLOBAL_CACHE_MAINTENANCE: PiMutex<()> = PiMutex::new(());

#[cfg(feature = "vfs")]
static RECLAIM_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "vfs")]
pub fn page_cache_reclaim(num_pages: usize) -> usize {
    if RECLAIM_IN_PROGRESS.swap(true, Ordering::AcqRel) {
        return 0;
    }
    let _guard = ReclaimGuard;

    let mut reclaimed = 0;
    let target = num_pages.max(16).saturating_mul(2);
    let mut file_count = 0;

    // Pin one bounded Arc batch while holding the registry spin lock, then
    // release it before taking cache-local PI locks or invoking listeners.
    // The allocator reclaim path must neither allocate a full registry
    // snapshot nor run arbitrary cache work with preemption disabled.
    let snapshot_len = match GLOBAL_CACHED_FILES.try_read() {
        Some(guard) => guard.len(),
        None => return 0,
    };
    let mut offset = 0;
    while offset < snapshot_len && reclaimed < target {
        let mut batch: [Option<Arc<CachedFileShared>>; REGISTRY_RECLAIM_BATCH] =
            core::array::from_fn(|_| None);
        let copied = match GLOBAL_CACHED_FILES.try_read() {
            Some(guard) => {
                let end = guard
                    .len()
                    .min(snapshot_len)
                    .min(offset.saturating_add(REGISTRY_RECLAIM_BATCH));
                for (slot, file) in batch.iter_mut().zip(guard[offset..end].iter()) {
                    *slot = Some(file.clone());
                }
                end.saturating_sub(offset)
            }
            None => break,
        };
        if copied == 0 {
            break;
        }
        offset += copied;

        for file in batch[..copied].iter().flatten() {
            let freed = file.try_evict_clean_pages(target - reclaimed);
            reclaimed += freed;
            file_count += 1;
            if reclaimed >= target {
                break;
            }
        }
    }

    if reclaimed > 0 {
        debug!(
            "page_cache_reclaim: evicted {} clean pages across {} files",
            reclaimed, file_count
        );
    }

    reclaimed
}

#[cfg(feature = "vfs")]
pub(super) fn register_cached_file(file: &Arc<CachedFileShared>) {
    let mut guard = GLOBAL_CACHED_FILES.write();
    if !guard.iter().any(|cached| Arc::ptr_eq(cached, file)) {
        guard.push(file.clone());
    }
}

#[cfg(feature = "vfs")]
pub fn sync_all_cached_files(_data_only: bool) -> VfsResult<()> {
    // Membership maintenance is task-context work. Serializing it with a PI
    // mutex lets the registry spin lock remain a narrow Arc-list guard; no
    // page-cache lock or filesystem callback may run while it is held.
    let _maintenance = GLOBAL_CACHE_MAINTENANCE.lock();
    let files = GLOBAL_CACHED_FILES.read().clone();
    let mut first_error = None;
    for file in &files {
        if let Err(err) = file.writeback_dirty_for_global_sync()
            && first_error.is_none()
        {
            first_error = Some(err);
        }
    }

    drop(files);
    prune_global_cache_registry();

    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

#[cfg(feature = "vfs")]
fn prune_global_cache_registry() {
    // Move the membership vector out in one short spin-locked transition.
    // Registrations may proceed into the now-empty vector while cache state is
    // inspected. Merging below deduplicates those concurrent registrations.
    let mut detached = {
        let mut registry = GLOBAL_CACHED_FILES.write();
        mem::take(&mut *registry)
    };
    detached.retain(|cached| Arc::strong_count(cached) > 1 || cached.has_dirty_pages());

    let mut registry = GLOBAL_CACHED_FILES.write();
    for cached in detached {
        if !registry
            .iter()
            .any(|registered| Arc::ptr_eq(registered, &cached))
        {
            registry.push(cached);
        }
    }
}
#[cfg(feature = "ext4")]
pub(super) fn should_share_cached_file_by_inode(location: &Location) -> bool {
    location.filesystem().name() == "ext4"
}

#[cfg(feature = "ext4")]
fn filesystem_key(filesystem: &dyn FilesystemOps) -> usize {
    filesystem as *const dyn FilesystemOps as *const () as usize
}

#[cfg(feature = "ext4")]
pub(super) fn cached_file_key(location: &Location) -> CachedFileKey {
    (filesystem_key(location.filesystem()), location.inode())
}

#[cfg(feature = "ext4")]
pub(super) fn lookup_inode_cached_file(key: CachedFileKey) -> Option<Arc<CachedFileShared>> {
    let mut cache = CACHED_FILE_BY_INODE.lock();
    match cache.get(&key).and_then(Weak::upgrade) {
        Some(shared) => Some(shared),
        None => {
            cache.remove(&key);
            None
        }
    }
}

#[cfg(feature = "ext4")]
pub(super) fn insert_inode_cached_file(key: CachedFileKey, shared: &Arc<CachedFileShared>) {
    CACHED_FILE_BY_INODE
        .lock()
        .insert(key, Arc::downgrade(shared));
}

#[cfg(feature = "ext4")]
pub(crate) fn forget_cached_file_key(filesystem: &dyn FilesystemOps, inode: u64) {
    if filesystem.name() == "ext4" {
        CACHED_FILE_BY_INODE
            .lock()
            .remove(&(filesystem_key(filesystem), inode));
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
