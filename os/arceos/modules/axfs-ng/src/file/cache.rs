use alloc::{boxed::Box, sync::Arc, vec::Vec};
#[cfg(feature = "ext4")]
use alloc::{collections::BTreeMap, sync::Weak};
#[cfg(feature = "vfs")]
use core::sync::atomic::AtomicBool;
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicU64, Ordering},
};

use ax_io::prelude::*;
#[cfg(feature = "ext4")]
use axfs_ng_vfs::FilesystemOps;
use axfs_ng_vfs::{FileNode, Location, VfsError, VfsResult};
use intrusive_collections::{LinkedList, LinkedListAtomicLink, intrusive_adapter};
use lru::LruCache;

use super::page::PageCache;
use crate::os::{memory::PAGE_SIZE, sync::SleepMutex as Mutex};

const DISK_PAGE_CACHE_CAP: usize = 512;

#[cfg(feature = "ext4")]
type CachedFileKey = (usize, u64);
#[cfg(feature = "ext4")]
type InodeCacheIndex = BTreeMap<CachedFileKey, Weak<CachedFileShared>>;

#[cfg(feature = "ext4")]
static CACHED_FILE_BY_INODE: spin::LazyLock<Mutex<InodeCacheIndex>> =
    spin::LazyLock::new(|| Mutex::new(BTreeMap::new()));

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

struct CachedFileShared {
    page_cache: Mutex<LruCache<u32, PageCache>>,
    io_lock: Mutex<()>,
    evict_listeners: Mutex<LinkedList<EvictListenerAdapter>>,
    backing: Option<FileNode>,
    len: AtomicU64,
}

impl CachedFileShared {
    pub fn new(len: u64, backing: FileNode) -> Self {
        Self {
            page_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(DISK_PAGE_CACHE_CAP).unwrap(),
            )),
            io_lock: Mutex::new(()),
            evict_listeners: Mutex::new(LinkedList::default()),
            backing: Some(backing),
            len: AtomicU64::new(len),
        }
    }

    pub fn new_unbounded(len: u64) -> Self {
        Self {
            page_cache: Mutex::new(LruCache::unbounded()),
            io_lock: Mutex::new(()),
            evict_listeners: Mutex::new(LinkedList::default()),
            backing: None,
            len: AtomicU64::new(len),
        }
    }

    fn len(&self) -> u64 {
        self.len.load(Ordering::Acquire)
    }

    fn update_len_max(&self, len: u64) {
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

    fn set_len(&self, len: u64) {
        self.len.store(len, Ordering::Release);
    }

    fn backing(&self) -> VfsResult<&FileNode> {
        self.backing.as_ref().ok_or(VfsError::InvalidInput)
    }

    fn writeback(&self) -> VfsResult<alloc::vec::Vec<u32>> {
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

    fn writeback_pages(&self, pns: &[u32]) -> VfsResult<()> {
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

    fn sync(&self, data_only: bool) -> VfsResult<()> {
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
            self.backing()?.write_at(&data, offset)?;

            {
                let mut guard = self.page_cache.lock();
                for page in &snapshots[run_start..run_end] {
                    if let Some(current) = guard.get_mut(&page.pn)
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
            let Some(page) = guard.get_mut(pn) else {
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
            if let Some(page) = guard.get_mut(pn) {
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
    /// `page_cache` is released before acquiring `evict_listeners`,
    /// eliminating the latent deadlock risk that exists when listeners
    /// are called under the cache lock.
    #[cfg(feature = "vfs")]
    fn try_evict_clean_pages(&self, max: usize) -> usize {
        let limit = max.min(256);

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
            let mut all_ok = true;
            for listener in self.evict_listeners.lock().iter() {
                if !(listener.listener)(pn, &page) {
                    all_ok = false;
                    break;
                }
            }
            if all_ok {
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
static RECLAIM_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "vfs")]
pub fn page_cache_reclaim(num_pages: usize) -> usize {
    if RECLAIM_IN_PROGRESS.swap(true, Ordering::AcqRel) {
        return 0;
    }
    let _guard = ReclaimGuard;

    let mut reclaimed = 0;
    let target = num_pages.max(16) * 2;
    let mut file_count = 0;

    if let Some(guard) = GLOBAL_CACHED_FILES.try_read() {
        for file in guard.iter() {
            let freed = file.try_evict_clean_pages(target - reclaimed);
            reclaimed += freed;
            file_count += 1;
            if reclaimed >= target {
                break;
            }
        }
    } else {
        return 0;
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
fn register_cached_file(file: &Arc<CachedFileShared>) {
    let mut guard = GLOBAL_CACHED_FILES.write();
    guard.retain(|cached| Arc::strong_count(cached) > 1 || cached.has_dirty_pages());
    guard.push(file.clone());
}

#[cfg(feature = "vfs")]
pub fn sync_all_cached_files(_data_only: bool) -> VfsResult<()> {
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

    let mut guard = GLOBAL_CACHED_FILES.write();
    guard.retain(|cached| Arc::strong_count(cached) > 1 || cached.has_dirty_pages());

    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// A file handle with an LRU page cache for buffered I/O.
pub struct CachedFile {
    inner: Location,
    shared: Arc<CachedFileShared>,
    in_memory: bool,
}

impl Clone for CachedFile {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            shared: self.shared.clone(),
            in_memory: self.in_memory,
        }
    }
}

enum FileUserData {
    Strong(Arc<CachedFileShared>),
}

impl FileUserData {
    pub fn get(&self) -> Arc<CachedFileShared> {
        match self {
            FileUserData::Strong(strong) => strong.clone(),
        }
    }
}

impl CachedFile {
    /// Returns an existing cached file for `location`, or creates a new one.
    pub fn get_or_create(location: Location) -> VfsResult<Self> {
        let in_memory = location.filesystem().name() == "tmpfs";

        let existing = {
            let guard = location.user_data();
            guard
                .get::<FileUserData>()
                .as_deref()
                .map(FileUserData::get)
        };
        if let Some(shared) = existing {
            return Ok(Self {
                inner: location,
                shared,
                in_memory,
            });
        }

        let len = location.len()?;
        #[cfg(feature = "ext4")]
        let inode_key =
            should_share_cached_file_by_inode(&location).then(|| cached_file_key(&location));
        #[cfg(feature = "ext4")]
        let inode_shared = inode_key.and_then(lookup_inode_cached_file);
        #[cfg(not(feature = "ext4"))]
        let inode_shared: Option<Arc<CachedFileShared>> = None;
        let (created, user_data) = if let Some(shared) = inode_shared {
            (shared.clone(), FileUserData::Strong(shared))
        } else if in_memory {
            let shared = Arc::new(CachedFileShared::new_unbounded(len));
            (shared.clone(), FileUserData::Strong(shared))
        } else {
            let backing = location.entry().as_file()?.clone();
            let shared = Arc::new(CachedFileShared::new(len, backing));
            (shared.clone(), FileUserData::Strong(shared))
        };

        let (shared, is_new) = {
            let mut guard = location.user_data();
            if let Some(shared) = guard
                .get::<FileUserData>()
                .as_deref()
                .map(FileUserData::get)
            {
                (shared, false)
            } else {
                guard.insert(user_data);
                (created, true)
            }
        };

        // In-memory files (tmpfs) have no backing store, so evicting clean
        // pages would lose data. Only register disk-backed files for reclaim.
        #[cfg(feature = "vfs")]
        if is_new && !in_memory {
            register_cached_file(&shared);
        }
        #[cfg(not(feature = "vfs"))]
        let _ = is_new;
        #[cfg(feature = "ext4")]
        if is_new && let Some(key) = inode_key {
            insert_inode_cached_file(key, &shared);
        }

        Ok(Self {
            inner: location,
            shared,
            in_memory,
        })
    }

    /// Returns `true` if both handles refer to the same shared state.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    /// Returns the current cached file length.
    pub fn len(&self) -> u64 {
        self.shared.len()
    }

    /// Returns whether the current cached file length is zero.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns `true` if this file is backed by an in-memory filesystem (e.g. tmpfs).
    pub fn in_memory(&self) -> bool {
        self.in_memory
    }

    /// Returns the current length (in bytes) of the backing file.
    pub fn file_len(&self) -> VfsResult<u64> {
        self.inner.len()
    }

    /// Registers a listener that is called when a page is evicted from cache.
    ///
    /// Returns a handle that can later be passed to
    /// [`remove_evict_listener`](Self::remove_evict_listener).
    pub fn add_evict_listener<F>(&self, listener: F) -> usize
    where
        F: Fn(u32, &PageCache) -> bool + Send + Sync + 'static,
    {
        self.add_page_listener(listener, |_| true)
    }

    /// Registers a listener for page eviction and dirty writeback protection.
    ///
    /// The writeback callback is invoked before a dirty cached page is
    /// snapshotted and written to backing storage. Shared mmap users should
    /// remove writable PTEs here so later writes fault and advance the dirty
    /// generation before the cache can be marked clean.
    pub fn add_page_listener<E, W>(&self, evict: E, writeback_protect: W) -> usize
    where
        E: Fn(u32, &PageCache) -> bool + Send + Sync + 'static,
        W: Fn(u32) -> bool + Send + Sync + 'static,
    {
        let pointer = Box::new(EvictListener {
            listener: Arc::new(evict),
            writeback_protect: Arc::new(writeback_protect),
            link: LinkedListAtomicLink::new(),
        });
        let handle = pointer.as_ref() as *const EvictListener as usize;
        self.shared.evict_listeners.lock().push_back(pointer);
        handle
    }

    /// # Safety
    /// The handle must be valid, that means:
    /// - It must be returned by a previous call to `add_evict_listener` on the same `CachedFile`.
    /// - It must not be removed by a previous call to `remove_evict_listener`.
    pub unsafe fn remove_evict_listener(&self, handle: usize) {
        let mut guard = self.shared.evict_listeners.lock();
        let mut cursor = unsafe { guard.cursor_mut_from_ptr(handle as *const EvictListener) };
        cursor.remove();
    }

    fn evict_cache(&self, file: &FileNode, pn: u32, page: &mut PageCache) -> VfsResult<()> {
        for listener in self.shared.evict_listeners.lock().iter() {
            // In the LRU-eviction path (triggered by page_or_insert), the
            // populate process holds AddrSpace and handles the unmap via
            // PopulateCallback.  The listener's return value is irrelevant
            // here — if try_lock fails, the caller is the populate process
            // itself and it will unmap the old page after inserting the new one.
            let _ = (listener.listener)(pn, page);
        }
        if page.dirty {
            let page_start = pn as u64 * PAGE_SIZE as u64;
            let len = (self.shared.len().saturating_sub(page_start)).min(PAGE_SIZE as u64) as usize;
            if len > 0 {
                file.write_at(&page.data()[..len], page_start)?;
            }
            page.dirty = false;
        }
        Ok(())
    }

    fn page_or_insert<'a>(
        &self,
        file: &FileNode,
        cache: &'a mut LruCache<u32, PageCache>,
        pn: u32,
        read_backing: bool,
    ) -> VfsResult<(&'a mut PageCache, Option<(u32, PageCache)>)> {
        // TODO: Matching the result of `get_mut` confuses compiler. See
        // https://users.rust-lang.org/t/return-do-not-release-mutable-borrow/55757.
        if cache.contains(&pn) {
            return Ok((cache.get_mut(&pn).unwrap(), None));
        }
        let mut evicted = None;
        if cache.len() >= cache.cap().get() {
            // Cache is full, remove the least recently used page
            if let Some((pn, mut page)) = cache.pop_lru() {
                self.evict_cache(file, pn, &mut page)?;
                evicted = Some((pn, page));
            }
        }

        let mut page = PageCache::new()?;
        if self.in_memory || !read_backing {
            page.data().fill(0);
        } else {
            // `PageCache::new()` does not zero the freshly allocated frame, and
            // `FileNodeOps::read_at` short-reads at EOF (rsext4/fat return only the
            // bytes actually read, leaving the rest of the buffer untouched). Zero the
            // tail beyond the read length so a partial last page never exposes stale
            // physical memory past EOF — POSIX/Linux require those bytes to read as 0
            // (e.g. an mmap of a 100-byte file must see `[100, PAGE_SIZE)` as zero).
            let read = file.read_at(page.data(), pn as u64 * PAGE_SIZE as u64)?;
            page.data()[read..].fill(0);
        }
        cache.put(pn, page);
        Ok((cache.get_mut(&pn).unwrap(), evicted))
    }

    /// Marks one cached mmap page dirty through the shared cached-I/O protocol.
    pub fn mark_mmap_dirty_page(&self, pn: u32) -> VfsResult<()> {
        if self.in_memory {
            return Ok(());
        }
        let _io = self.shared.io_lock.lock();
        let mut guard = self.shared.page_cache.lock();
        guard.get_mut(&pn).ok_or(VfsError::BadState)?.mark_dirty();
        Ok(())
    }

    /// Invokes `f` with the cached page at `pn`, loading it from disk if absent.
    ///
    /// If loading the page causes an eviction, the evicted `(page_number, page)`
    /// pair is also passed to `f`.
    pub fn with_page_or_insert<R>(
        &self,
        pn: u32,
        f: impl FnOnce(&mut PageCache, Option<(u32, PageCache)>) -> VfsResult<R>,
    ) -> VfsResult<R> {
        let _io = self.shared.io_lock.lock();
        let mut guard = self.shared.page_cache.lock();
        let (page, evicted) =
            self.page_or_insert(self.inner.entry().as_file()?, &mut guard, pn, true)?;
        f(page, evicted)
    }

    /// Reads data from the file at `offset` into `dst`.
    pub fn read_at(&self, mut dst: impl Write + IoBufMut, offset: u64) -> VfsResult<usize> {
        let len = self.shared.len();
        let end = offset.saturating_add(dst.remaining_mut() as u64).min(len);
        if end <= offset {
            return Ok(0);
        }

        let file = self.inner.entry().as_file()?;
        let mut scratch = PageCache::new()?;
        let mut read = 0;
        let mut current = offset;
        while current < end {
            let pn = (current / PAGE_SIZE as u64) as u32;
            let page_start = pn as u64 * PAGE_SIZE as u64;
            let page_offset = (current - page_start) as usize;
            let chunk_len = (end - page_start).min(PAGE_SIZE as u64) as usize - page_offset;

            {
                let _io = self.shared.io_lock.lock();
                let mut guard = self.shared.page_cache.lock();
                let page = self.page_or_insert(file, &mut guard, pn, true)?.0;
                scratch.data()[..chunk_len]
                    .copy_from_slice(&page.data()[page_offset..page_offset + chunk_len]);
            }

            // `dst` may point at user memory. Copy after releasing cached-file
            // locks so a user page fault can take AddrSpace without creating a
            // cached-I/O -> AddrSpace lock order.
            dst.write_all(&scratch.data()[..chunk_len])?;
            read += chunk_len;
            current += chunk_len as u64;
        }

        Ok(read)
    }

    fn write_at_locked(&self, mut buf: impl Read + IoBuf, offset: u64) -> VfsResult<usize> {
        let file = self.inner.entry().as_file()?;
        let end = offset.saturating_add(buf.remaining() as u64);
        let old_len = self.shared.len();
        // Delayed allocation: do NOT call file.set_len() on every write().
        // The writeback path (write_inode_data) already handles block
        // allocation and inode size updates when dirty pages are flushed.
        // Calling set_len here forces a synchronous journal commit per
        // write() when extending the file, which devastates small-write
        // performance (e.g. 1KB writes → 4096 set_len → 410 commits).
        if end > old_len {
            self.shared.update_len_max(end);
        }

        let mut scratch = PageCache::new()?;
        let mut written = 0;
        let mut current = offset;
        while current < end && buf.remaining() > 0 {
            let pn = (current / PAGE_SIZE as u64) as u32;
            let page_start = pn as u64 * PAGE_SIZE as u64;
            let page_offset = (current - page_start) as usize;
            let chunk_len =
                ((PAGE_SIZE - page_offset).min(buf.remaining())).min((end - current) as usize);
            let n = buf.read(&mut scratch.data()[..chunk_len])?;
            if n == 0 {
                break;
            }
            self.shared.update_len_max(current + n as u64);

            {
                let mut guard = self.shared.page_cache.lock();
                let read_backing = page_start < old_len && !(page_offset == 0 && n == PAGE_SIZE);
                let page = self.page_or_insert(file, &mut guard, pn, read_backing)?.0;
                page.data()[page_offset..page_offset + n].copy_from_slice(&scratch.data()[..n]);
                if !self.in_memory {
                    page.mark_dirty();
                }
            }

            written += n;
            current += n as u64;
        }

        Ok(written)
    }

    /// Writes `buf` to the file at `offset`.
    pub fn write_at(&self, buf: impl Read + IoBuf, offset: u64) -> VfsResult<usize> {
        let _io = self.shared.io_lock.lock();
        self.write_at_locked(buf, offset)
    }

    /// Appends `buf` to the end of the file. Returns `(bytes_written, new_end)`.
    pub fn append(&self, buf: impl Read + IoBuf) -> VfsResult<(usize, u64)> {
        let _io = self.shared.io_lock.lock();
        let len = self.shared.len();
        self.write_at_locked(buf, len)
            .map(|written| (written, len + written as u64))
    }

    /// Truncates or extends the file to `len` bytes.
    pub fn set_len(&self, len: u64) -> VfsResult<()> {
        let _io = self.shared.io_lock.lock();
        let file = self.inner.entry().as_file()?;
        let old_len = self.shared.len();
        file.set_len(len)?;
        self.shared.set_len(len);

        let old_last_page = (old_len / PAGE_SIZE as u64) as u32;
        let new_last_page = (len / PAGE_SIZE as u64) as u32;
        if old_len < len {
            let mut guard = self.shared.page_cache.lock();
            if let Some(page) = guard.get_mut(&old_last_page) {
                let page_start = old_last_page as u64 * PAGE_SIZE as u64;
                let old_page_offset = (old_len - page_start) as usize;
                let new_page_offset = (len - page_start).min(PAGE_SIZE as u64) as usize;
                page.data()[old_page_offset..new_page_offset].fill(0);
                // Mark dirty so the zeroed gap is written back: ext4 `set_len`
                // only updates `i_size`, it does not clear the bytes on disk, so
                // a clean eviction + reload would otherwise resurrect stale data.
                page.dirty = true;
            }
        } else if len < old_len {
            let mut guard = self.shared.page_cache.lock();
            // Linux `truncate(len)` zeroes the tail of the partial last page, so a
            // later extend or `mmap` reads those bytes as zero. Without this, a
            // shrink that leaves a partial last page (e.g. sqlite's
            // `ftruncate(<-shm>, 3)`) keeps stale bytes there; a subsequent mmap of
            // the regrown file then sees the stale tail, so a fresh reader trusts a
            // stale wal-index header instead of recovering (juicefs sqlite WAL
            // cross-process reopen failure). This branch also covers shrinking
            // within a single page, where neither old branch ran at all.
            let tail = (len % PAGE_SIZE as u64) as usize;
            if tail != 0
                && let Some(page) = guard.get_mut(&new_last_page)
            {
                page.data()[tail..].fill(0);
                // Mark dirty so the zeroed tail is written back: ext4 `set_len`
                // updates `i_size` but leaves the on-disk bytes past it intact, so
                // a clean eviction + reload (or mmap fault) would otherwise reload
                // the stale tail from disk.
                page.dirty = true;
            }
            // Remove all pages that are wholly beyond the new length.
            // TODO(mivik): can this be more efficient?
            let keys = guard
                .iter()
                .map(|(k, _)| *k)
                .filter(|it| *it > new_last_page)
                .collect::<Vec<_>>();
            for pn in keys {
                if let Some(mut page) = guard.pop(&pn)
                    && !self.in_memory
                {
                    // Don't write back pages since they're discarded
                    page.dirty = false;
                    self.evict_cache(file, pn, &mut page)?;
                }
            }
        }
        Ok(())
    }

    pub fn writeback(&self) -> VfsResult<alloc::vec::Vec<u32>> {
        if self.in_memory {
            return Ok(alloc::vec::Vec::new());
        }
        self.shared.writeback()
    }

    pub fn writeback_pages(&self, pns: &[u32]) -> VfsResult<()> {
        if self.in_memory {
            return Ok(());
        }
        self.shared.writeback_pages(pns)
    }

    pub fn dirty_pages_in_range(&self, start_pn: u32, end_pn: u32) -> alloc::vec::Vec<u32> {
        let _io = self.shared.io_lock.lock();
        let guard = self.shared.page_cache.lock();
        guard
            .iter()
            .filter_map(|(&pn, page)| {
                if page.dirty && pn >= start_pn && pn < end_pn {
                    Some(pn)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn clear_dirty_pages(&self, pns: &[u32]) {
        let _io = self.shared.io_lock.lock();
        let mut guard = self.shared.page_cache.lock();
        for pn in pns {
            if let Some(page) = guard.get_mut(pn) {
                page.dirty = false;
                page.dirty_generation = page.dirty_generation.wrapping_add(1);
            }
        }
    }

    /// Flushes all cached pages back to disk.
    pub fn sync(&self, data_only: bool) -> VfsResult<()> {
        if self.in_memory {
            return Ok(());
        }
        self.shared.sync(data_only)
    }

    /// Returns a reference to the underlying [`Location`].
    pub fn location(&self) -> &Location {
        &self.inner
    }
}

#[cfg(feature = "ext4")]
fn should_share_cached_file_by_inode(location: &Location) -> bool {
    location.filesystem().name() == "ext4"
}

#[cfg(feature = "ext4")]
fn filesystem_key(filesystem: &dyn FilesystemOps) -> usize {
    filesystem as *const dyn FilesystemOps as *const () as usize
}

#[cfg(feature = "ext4")]
fn cached_file_key(location: &Location) -> CachedFileKey {
    (filesystem_key(location.filesystem()), location.inode())
}

#[cfg(feature = "ext4")]
fn lookup_inode_cached_file(key: CachedFileKey) -> Option<Arc<CachedFileShared>> {
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
fn insert_inode_cached_file(key: CachedFileKey, shared: &Arc<CachedFileShared>) {
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

impl Drop for CachedFile {
    fn drop(&mut self) {
        // Linux close(2) does not imply fsync(2). Disk-backed page cache is
        // retained by the inode user_data and written by explicit sync paths.
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::os::memory::test_support::with_test_page_provider;

    #[test]
    fn page_cache_paddr_reports_bad_state_when_translation_is_missing() {
        with_test_page_provider(false, |_| {
            let page = PageCache::new().unwrap();
            assert_eq!(page.paddr().unwrap_err(), VfsError::BadState);
        });
    }

    #[test]
    fn writeback_protect_listener_runs_without_cached_io_lock() {
        let shared = Arc::new(CachedFileShared::new_unbounded(0));
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed = observed_unlocked.clone();
        let listener_shared = shared.clone();

        shared
            .evict_listeners
            .lock()
            .push_back(Box::new(EvictListener {
                listener: Arc::new(|_, _| true),
                writeback_protect: Arc::new(move |_| {
                    observed.store(
                        listener_shared.io_lock_is_free_for_test(),
                        Ordering::Release,
                    );
                    true
                }),
                link: LinkedListAtomicLink::new(),
            }));

        shared.invoke_writeback_protect_for_test(&[0]).unwrap();

        assert!(observed_unlocked.load(Ordering::Acquire));
    }

    #[test]
    fn writeback_protect_listener_runs_without_listener_lock() {
        let shared = Arc::new(CachedFileShared::new_unbounded(0));
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed = observed_unlocked.clone();
        let listener_shared = shared.clone();

        shared
            .evict_listeners
            .lock()
            .push_back(Box::new(EvictListener {
                listener: Arc::new(|_, _| true),
                writeback_protect: Arc::new(move |_| {
                    observed.store(
                        listener_shared.listener_lock_is_free_for_test(),
                        Ordering::Release,
                    );
                    true
                }),
                link: LinkedListAtomicLink::new(),
            }));

        shared.invoke_writeback_protect_for_test(&[0]).unwrap();

        assert!(observed_unlocked.load(Ordering::Acquire));
    }

    #[test]
    fn writeback_protect_does_not_hold_listener_lock_while_invoking_callbacks() {
        let shared = Arc::new(CachedFileShared::new_unbounded(0));
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed = observed_unlocked.clone();
        let listener_shared = shared.clone();

        shared
            .evict_listeners
            .lock()
            .push_back(Box::new(EvictListener {
                listener: Arc::new(|_, _| true),
                writeback_protect: Arc::new(move |_| {
                    observed.store(
                        listener_shared.evict_listeners.try_lock().is_some(),
                        Ordering::Release,
                    );
                    true
                }),
                link: LinkedListAtomicLink::new(),
            }));

        shared.protect_dirty_pages_before_writeback(&[0]).unwrap();

        assert!(observed_unlocked.load(Ordering::Acquire));
    }
}
