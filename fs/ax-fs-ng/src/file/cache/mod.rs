mod readahead;
#[cfg(feature = "vfs")]
mod reclaim;
mod resize;
mod writeback;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
#[cfg(feature = "ext4")]
use alloc::{collections::BTreeMap, sync::Weak};
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
use readahead::ReadAheadState;
#[cfg(feature = "vfs")]
pub use reclaim::{page_cache_reclaim, sync_all_cached_files};

use super::page::PageCache;
use crate::os::{memory::PAGE_SIZE, sync::SleepMutex as Mutex};

const DISK_PAGE_CACHE_CAP: usize = 512;

#[cfg(feature = "ext4")]
type CachedFileKey = (usize, u64);
#[cfg(feature = "ext4")]
type InodeCacheIndex = BTreeMap<CachedFileKey, Weak<CachedFileShared>>;

#[cfg(feature = "ext4")]
static CACHED_FILE_BY_INODE: ax_lazyinit::LazyLock<Mutex<InodeCacheIndex>> =
    ax_lazyinit::LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Eviction listener callback. Returns `true` if the listener successfully
/// invalidated all mappings for the evicted page.
type EvictListenerFn = Arc<dyn Fn(u32, &PageCache) -> bool + Send + Sync>;
type WritebackProtectListenerFn = Arc<dyn Fn(u32) -> bool + Send + Sync>;

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

    #[cfg(test)]
    fn page_cache_lock_is_free_for_test(&self) -> bool {
        self.page_cache.try_lock().is_some()
    }
}

/// A file handle with an LRU page cache for buffered I/O.
pub struct CachedFile {
    inner: Location,
    shared: Arc<CachedFileShared>,
    readahead: Arc<Mutex<ReadAheadState>>,
    in_memory: bool,
}

impl Clone for CachedFile {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            shared: self.shared.clone(),
            readahead: self.readahead.clone(),
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

fn filesystem_uses_unbounded_page_cache(name: &str) -> bool {
    matches!(name, "tmpfs" | "ramfs")
}

impl CachedFile {
    /// Returns an existing cached file for `location`, or creates a new one.
    pub fn get_or_create(location: Location) -> VfsResult<Self> {
        let in_memory = filesystem_uses_unbounded_page_cache(location.filesystem().name());

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
                readahead: Arc::new(Mutex::new(ReadAheadState::new())),
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

        // tmpfs and ramfs have no backing store, so evicting clean pages would
        // lose data. Only register disk-backed files for reclaim.
        #[cfg(feature = "vfs")]
        if is_new && !in_memory {
            reclaim::register_cached_file(&shared);
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
            readahead: Arc::new(Mutex::new(ReadAheadState::new())),
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

    /// Loads one bounded contiguous cache window beginning at `pn`.
    ///
    /// The caller holds `io_lock`, so page-cache writers cannot race the
    /// backing read. The cache lock is deliberately released while the backing
    /// filesystem blocks on IRQ-driven I/O.
    fn populate_page_window(&self, file: &FileNode, pn: u32, window_pages: usize) -> VfsResult<()> {
        if self.in_memory {
            let mut guard = self.shared.page_cache.lock();
            self.page_or_insert(file, &mut guard, pn, false)?;
            return Ok(());
        }

        let file_len = self.shared.len();
        let first_page = u64::from(pn);
        let file_pages = file_len.div_ceil(PAGE_SIZE as u64);
        if first_page >= file_pages {
            return Err(VfsError::InvalidInput);
        }

        let max_pages = window_pages.max(1);
        let candidate_end = first_page
            .saturating_add(max_pages as u64)
            .min(file_pages)
            .min(u64::from(u32::MAX) + 1);
        let run_pages = {
            let guard = self.shared.page_cache.lock();
            if guard.contains(&pn) {
                return Ok(());
            }
            let mut page = first_page;
            while page < candidate_end {
                let page_number = u32::try_from(page).map_err(|_| VfsError::InvalidInput)?;
                if guard.contains(&page_number) {
                    break;
                }
                page += 1;
            }
            usize::try_from(page - first_page).map_err(|_| VfsError::InvalidInput)?
        };
        if run_pages == 0 {
            return Ok(());
        }

        let run_len = run_pages
            .checked_mul(PAGE_SIZE)
            .ok_or(VfsError::InvalidInput)?;
        let mut data = Vec::new();
        data.try_reserve_exact(run_len)
            .map_err(|_| VfsError::NoMemory)?;
        data.resize(run_len, 0);
        let file_offset = first_page
            .checked_mul(PAGE_SIZE as u64)
            .ok_or(VfsError::InvalidInput)?;
        let readable = usize::try_from(file_len.saturating_sub(file_offset))
            .unwrap_or(usize::MAX)
            .min(run_len);
        file.read_at(&mut data[..readable], file_offset)?;

        let mut guard = self.shared.page_cache.lock();
        for index in 0..run_pages {
            let page_number = pn
                .checked_add(u32::try_from(index).map_err(|_| VfsError::InvalidInput)?)
                .ok_or(VfsError::InvalidInput)?;
            if guard.contains(&page_number) {
                continue;
            }
            let page = self.page_or_insert(file, &mut guard, page_number, false)?.0;
            let start = index * PAGE_SIZE;
            page.data().copy_from_slice(&data[start..start + PAGE_SIZE]);
        }
        Ok(())
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
        let window_pages = if self.in_memory {
            1
        } else {
            self.readahead.lock().plan(offset, end).window_pages
        };

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
                self.populate_page_window(file, pn, window_pages)?;
                let mut guard = self.shared.page_cache.lock();
                let page = guard.get_mut(&pn).ok_or(VfsError::BadState)?;
                scratch.data()[..chunk_len]
                    .copy_from_slice(&page.data()[page_offset..page_offset + chunk_len]);
            }

            // `dst` may point at user memory. Copy after releasing cached-file
            // locks so a user page fault can take AddrSpace without creating a
            // cached-I/O -> AddrSpace lock order.
            dst.write_all(&scratch.data()[..chunk_len])
                .map_err(crate::io_error_to_vfs_error)?;
            read += chunk_len;
            current += chunk_len as u64;
        }

        Ok(read)
    }

    fn write_at_locked(&self, mut buf: impl Read + IoBuf, offset: u64) -> VfsResult<usize> {
        let file = self.inner.entry().as_file()?;
        let end = offset.saturating_add(buf.remaining() as u64);
        let old_len = self.shared.len();
        if end > old_len {
            if !old_len.is_multiple_of(PAGE_SIZE as u64) {
                let page_number = (old_len / PAGE_SIZE as u64) as u32;
                let page_start = u64::from(page_number) * PAGE_SIZE as u64;
                self.zero_partial_page_locked(
                    file,
                    page_number,
                    (old_len - page_start) as usize,
                    (end - page_start).min(PAGE_SIZE as u64) as usize,
                )?;
            }
            file.set_len(end)?;
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
            let n = buf
                .read(&mut scratch.data()[..chunk_len])
                .map_err(crate::io_error_to_vfs_error)?;
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
mod tests;
