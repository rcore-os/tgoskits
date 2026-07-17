//! Generation-aware cached-file handles and buffered I/O operations.

use alloc::{sync::Arc, vec, vec::Vec};

use ax_io::prelude::*;
use axfs_ng_vfs::{FileNode, Location, VfsError, VfsResult};
use lru::LruCache;

use super::shared::CachedFileShared;
#[cfg(feature = "vfs")]
use super::shared::register_cached_file;
#[cfg(feature = "ext4")]
use super::shared::{
    cached_file_key, insert_inode_cached_file, lookup_inode_cached_file,
    should_share_cached_file_by_inode,
};
use crate::{
    FsOpenHandleLease, FsOperationLease,
    file::{
        location::{FileLocation, GenerationBoundLocation, UnmanagedLocation},
        operation::LocationOperationView,
        page::PageCache,
    },
    lifecycle::FsGenerationAccess,
    os::memory::PAGE_SIZE,
};

const CACHE_READ_BATCH_PAGES: u64 = 256;

/// A file handle with an LRU page cache for buffered I/O.
#[derive(Clone)]
pub struct CachedFile {
    inner: Location,
    shared: Arc<CachedFileShared>,
    in_memory: bool,
    authority: Option<ManagedCachedFileAuthority>,
}

#[derive(Clone)]
struct ManagedCachedFileAuthority {
    access: FsGenerationAccess,
    lease: FsOpenHandleLease,
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
    pub fn get_or_create(location: UnmanagedLocation) -> VfsResult<Self> {
        Self::get_or_create_with_authority(location.into_inner(), None)
    }

    pub(crate) fn get_or_create_generation_bound(
        location: GenerationBoundLocation,
        lease: FsOpenHandleLease,
    ) -> VfsResult<Self> {
        let (location, access) = location.into_parts();
        Self::get_or_create_with_authority(
            location,
            Some(ManagedCachedFileAuthority { access, lease }),
        )
    }

    fn get_or_create_with_authority(
        location: Location,
        authority: Option<ManagedCachedFileAuthority>,
    ) -> VfsResult<Self> {
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
                authority,
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
            authority,
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
        let _operation = self.begin_operation()?;
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
        self.shared.add_page_listener(evict, writeback_protect)
    }

    /// # Safety
    /// The handle must be valid, that means:
    /// - It must be returned by a previous call to `add_evict_listener` on the same `CachedFile`.
    /// - It must not be removed by a previous call to `remove_evict_listener`.
    pub unsafe fn remove_evict_listener(&self, handle: usize) {
        unsafe { self.shared.remove_page_listener(handle) };
    }

    fn evict_cache(&self, pn: u32, page: &mut PageCache) -> VfsResult<bool> {
        if !self.shared.notify_page_eviction(pn, page) {
            return Ok(false);
        }
        if page.dirty {
            let page_start = pn as u64 * PAGE_SIZE as u64;
            let len = (self.shared.len().saturating_sub(page_start)).min(PAGE_SIZE as u64) as usize;
            if len > 0 {
                self.shared
                    .write_backing_all_at(&page.data()[..len], page_start)?;
            }
            page.dirty = false;
        }
        Ok(true)
    }

    fn page_or_insert<'a>(
        &self,
        file: &FileNode,
        cache: &'a mut LruCache<u32, PageCache>,
        pn: u32,
        read_backing: bool,
        allow_deferred_eviction: bool,
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
                if self.evict_cache(pn, &mut page)? {
                    drop(page);
                } else if allow_deferred_eviction && !page.dirty {
                    evicted = Some((pn, page));
                } else {
                    cache.put(pn, page);
                    return Err(VfsError::ResourceBusy);
                }
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
        let _operation = self.begin_operation()?;
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
        let _operation = self.begin_operation()?;
        let _io = self.shared.io_lock.lock();
        let mut guard = self.shared.page_cache.lock();
        let (page, evicted) =
            self.page_or_insert(self.inner.entry().as_file()?, &mut guard, pn, true, true)?;
        f(page, evicted)
    }

    /// Reads data from the file at `offset` into `dst`.
    pub fn read_at(&self, mut dst: impl Write + IoBufMut, offset: u64) -> VfsResult<usize> {
        let _operation = self.begin_operation()?;
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
            let page_number = current / PAGE_SIZE as u64;
            let pn = u32::try_from(page_number).map_err(|_| VfsError::InvalidInput)?;
            let page_start = page_number * PAGE_SIZE as u64;
            let page_offset = (current - page_start) as usize;
            let chunk_len = (end - page_start).min(PAGE_SIZE as u64) as usize - page_offset;

            let io = self.shared.prepare_page_insert(pn)?;
            let mut guard = self.shared.page_cache.lock();
            if let Some(page) = guard.get_mut(&pn) {
                scratch.data()[..chunk_len]
                    .copy_from_slice(&page.data()[page_offset..page_offset + chunk_len]);
                drop(guard);
                drop(io);

                // `dst` may point at user memory. Copy after releasing cached-file
                // locks so a user page fault can take AddrSpace without creating a
                // cached-I/O -> AddrSpace lock order.
                dst.write_all(&scratch.data()[..chunk_len])?;
                read += chunk_len;
                current += chunk_len as u64;
                continue;
            }

            // Snapshot a bounded Linux-style readahead window while cached I/O
            // is serialized. The window is independent of this syscall's user
            // buffer: small sequential reads should still fill one efficient
            // backing run. Existing pages delimit it so readahead never
            // overwrites dirty or mmap-visible cache state.
            let file_end_page = (len - 1) / PAGE_SIZE as u64 + 1;
            let run_limit = page_number
                .checked_add(CACHE_READ_BATCH_PAGES)
                .ok_or(VfsError::InvalidInput)?
                .min(file_end_page);
            let mut run_end = page_number + 1;
            while run_end < run_limit {
                let candidate = u32::try_from(run_end).map_err(|_| VfsError::InvalidInput)?;
                if guard.contains(&candidate) {
                    break;
                }
                run_end += 1;
            }
            drop(guard);

            let run_pages =
                usize::try_from(run_end - page_number).map_err(|_| VfsError::InvalidInput)?;
            let run_len = run_pages
                .checked_mul(PAGE_SIZE)
                .ok_or(VfsError::InvalidInput)?;
            let mut run_data = vec![0; run_len];
            let mut filled = 0;
            while filled < run_data.len() {
                let backing_offset = page_start
                    .checked_add(filled as u64)
                    .ok_or(VfsError::InvalidInput)?;
                let count = file.read_at(&mut run_data[filled..], backing_offset)?;
                if count == 0 {
                    break;
                }
                if count > run_data.len() - filled {
                    return Err(VfsError::BadState);
                }
                filled += count;
            }

            let mut guard = self.shared.page_cache.lock();
            for page_index in 0..run_pages {
                let cached_pn = u32::try_from(page_number + page_index as u64)
                    .map_err(|_| VfsError::InvalidInput)?;
                if guard.contains(&cached_pn) {
                    return Err(VfsError::BadState);
                }
                let (page, evicted) =
                    self.page_or_insert(file, &mut guard, cached_pn, false, false)?;
                debug_assert!(evicted.is_none());
                let data_start = page_index * PAGE_SIZE;
                page.data()
                    .copy_from_slice(&run_data[data_start..data_start + PAGE_SIZE]);
            }
            drop(guard);
            drop(io);

            // A cache miss owns the complete kernel buffer after releasing all
            // cache locks, so one bounded run can be copied to user memory in a
            // single faultable operation.
            let run_copy_len = (end - current).min((run_len - page_offset) as u64) as usize;
            dst.write_all(&run_data[page_offset..page_offset + run_copy_len])?;
            read += run_copy_len;
            current += run_copy_len as u64;
        }

        Ok(read)
    }

    fn write_at_locked<'a>(
        &'a self,
        mut buf: impl Read + IoBuf,
        offset: u64,
        mut io: crate::os::sync::PiMutexGuard<'a, ()>,
    ) -> VfsResult<usize> {
        let file = self.inner.entry().as_file()?;
        let end = offset.saturating_add(buf.remaining() as u64);
        let old_len = self.shared.len();
        if end > old_len {
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
            let n = buf.read(&mut scratch.data()[..chunk_len])?;
            if n == 0 {
                break;
            }
            self.shared.update_len_max(current + n as u64);

            {
                io = self.shared.prepare_page_insert_locked(pn, io)?;
                let mut guard = self.shared.page_cache.lock();
                let read_backing = page_start < old_len && !(page_offset == 0 && n == PAGE_SIZE);
                let page = self
                    .page_or_insert(file, &mut guard, pn, read_backing, false)?
                    .0;
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
        let _operation = self.begin_operation()?;
        let io = self.shared.io_lock.lock();
        self.write_at_locked(buf, offset, io)
    }

    /// Appends `buf` to the end of the file. Returns `(bytes_written, new_end)`.
    pub fn append(&self, buf: impl Read + IoBuf) -> VfsResult<(usize, u64)> {
        let _operation = self.begin_operation()?;
        let io = self.shared.io_lock.lock();
        let len = self.shared.len();
        self.write_at_locked(buf, len, io)
            .map(|written| (written, len + written as u64))
    }

    /// Truncates or extends the file to `len` bytes.
    pub fn set_len(&self, len: u64) -> VfsResult<()> {
        let _operation = self.begin_operation()?;
        self.set_len_active(len)
    }

    pub(crate) fn set_len_during(
        &self,
        len: u64,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<()> {
        if let Some(authority) = self.authority.as_ref() {
            let operation = operation.ok_or(VfsError::BadState)?;
            authority
                .access
                .validate_operation(operation)
                .map_err(|error| error.into_ax_error())?;
        }
        self.set_len_active(len)
    }

    fn set_len_active(&self, len: u64) -> VfsResult<()> {
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
                if !self.in_memory {
                    page.mark_dirty();
                }
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
                if !self.in_memory {
                    page.mark_dirty();
                }
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
                    let _ = self.evict_cache(pn, &mut page)?;
                }
            }
        }
        Ok(())
    }

    pub fn writeback(&self) -> VfsResult<alloc::vec::Vec<u32>> {
        let _operation = self.begin_operation()?;
        if self.in_memory {
            return Ok(alloc::vec::Vec::new());
        }
        self.shared.writeback()
    }

    pub fn writeback_pages(&self, pns: &[u32]) -> VfsResult<()> {
        let _operation = self.begin_operation()?;
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
        let _operation = self.begin_operation()?;
        if self.in_memory {
            return Ok(());
        }
        self.shared.sync(data_only)
    }

    pub(crate) fn location_ref(&self) -> &Location {
        &self.inner
    }

    /// Returns the location together with its generation or unmanaged proof.
    pub fn file_location(&self) -> FileLocation {
        match self.authority.as_ref() {
            Some(authority) => FileLocation::Managed(GenerationBoundLocation::from_access(
                self.inner.clone(),
                authority.access.clone(),
            )),
            None => FileLocation::Unmanaged(
                UnmanagedLocation::try_new(self.inner.clone())
                    .expect("an unmanaged cache is constructed only from a checked location"),
            ),
        }
    }

    pub(crate) fn begin_operation(&self) -> VfsResult<Option<FsOperationLease>> {
        self.authority
            .as_ref()
            .map(|authority| authority.lease.begin_operation())
            .transpose()
            .map_err(|error| error.into_ax_error())
    }

    pub(crate) fn is_generation_bound(&self) -> bool {
        self.authority.is_some()
    }

    /// Runs one restricted operation while retaining cache generation access.
    pub fn with_operation<T>(
        &self,
        operation: impl for<'operation> FnOnce(LocationOperationView<'operation>) -> VfsResult<T>,
    ) -> VfsResult<T> {
        let operation_lease = self.begin_operation()?;
        let view = match operation_lease.as_ref() {
            Some(operation_lease) => LocationOperationView::managed(&self.inner, operation_lease),
            None => LocationOperationView::unmanaged(&self.inner),
        };
        operation(view)
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
    use alloc::{sync::Arc, vec, vec::Vec};
    use core::{
        any::Any,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::Duration,
    };

    use ax_kspin::SpinNoPreempt;
    use axfs_ng_vfs::{
        DeviceId, DirEntry, FileNode, FileNodeOps, Filesystem, FilesystemDetachPolicy,
        FilesystemOps, FsIoEvents, FsPollable, Metadata, MetadataUpdate, Mountpoint, NodeFlags,
        NodeOps, NodePermission, NodeType, Reference, StatFs, VfsError, VfsResult, WeakDirEntry,
        path::MAX_NAME_LEN,
    };

    use super::*;
    use crate::os::memory::test_support::with_test_page_provider;

    struct CountingFilesystem {
        name: &'static str,
        root: std::sync::OnceLock<WeakDirEntry>,
    }

    struct CountingFile {
        filesystem: Arc<CountingFilesystem>,
        bytes: SpinNoPreempt<Vec<u8>>,
        read_calls: AtomicUsize,
        largest_read: AtomicUsize,
        syncs: AtomicUsize,
        write_calls: AtomicUsize,
        largest_write: AtomicUsize,
        max_write: AtomicUsize,
        fail_sync: AtomicBool,
    }

    impl CountingFile {
        fn read_calls(&self) -> usize {
            self.read_calls.load(Ordering::Acquire)
        }

        fn largest_read(&self) -> usize {
            self.largest_read.load(Ordering::Acquire)
        }

        fn syncs(&self) -> usize {
            self.syncs.load(Ordering::Acquire)
        }

        fn bytes(&self) -> Vec<u8> {
            self.bytes.lock().clone()
        }

        fn write_calls(&self) -> usize {
            self.write_calls.load(Ordering::Acquire)
        }

        fn largest_write(&self) -> usize {
            self.largest_write.load(Ordering::Acquire)
        }
    }

    impl FilesystemOps for CountingFilesystem {
        fn name(&self) -> &str {
            self.name
        }

        fn detach_policy(&self) -> FilesystemDetachPolicy {
            FilesystemDetachPolicy::NonDetachable
        }

        fn root_dir(&self) -> DirEntry {
            self.root
                .get()
                .and_then(WeakDirEntry::upgrade)
                .expect("the test root must remain mounted")
        }

        fn stat(&self) -> VfsResult<StatFs> {
            Ok(StatFs {
                fs_type: 0,
                block_size: PAGE_SIZE as u32,
                blocks: 1,
                blocks_free: 1,
                blocks_available: 1,
                file_count: 1,
                free_file_count: 0,
                name_length: MAX_NAME_LEN as u32,
                fragment_size: PAGE_SIZE as u32,
                mount_flags: 0,
            })
        }
    }

    impl NodeOps for CountingFile {
        fn inode(&self) -> u64 {
            1
        }

        fn metadata(&self) -> VfsResult<Metadata> {
            Ok(Metadata {
                inode: 1,
                device: 0,
                nlink: 1,
                mode: NodePermission::from_bits_truncate(0o644),
                node_type: NodeType::RegularFile,
                uid: 0,
                gid: 0,
                size: self.bytes.lock().len() as u64,
                block_size: PAGE_SIZE as u64,
                blocks: 1,
                rdev: DeviceId::default(),
                atime: Duration::ZERO,
                mtime: Duration::ZERO,
                ctime: Duration::ZERO,
            })
        }

        fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
            Ok(())
        }

        fn filesystem(&self) -> &dyn FilesystemOps {
            &*self.filesystem
        }

        fn sync(&self, _data_only: bool) -> VfsResult<()> {
            self.syncs.fetch_add(1, Ordering::AcqRel);
            if self.fail_sync.load(Ordering::Acquire) {
                Err(VfsError::Io)
            } else {
                Ok(())
            }
        }

        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }

        fn flags(&self) -> NodeFlags {
            NodeFlags::BLOCKING
        }
    }

    impl FsPollable for CountingFile {
        fn poll(&self) -> FsIoEvents {
            FsIoEvents::IN | FsIoEvents::OUT
        }

        fn register(&self, _context: &mut core::task::Context<'_>, _events: FsIoEvents) {}
    }

    impl FileNodeOps for CountingFile {
        fn read_at(&self, buffer: &mut [u8], offset: u64) -> VfsResult<usize> {
            self.read_calls.fetch_add(1, Ordering::AcqRel);
            let bytes = self.bytes.lock();
            let start = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
            if start >= bytes.len() {
                return Ok(0);
            }
            let len = buffer.len().min(bytes.len() - start);
            self.largest_read.fetch_max(len, Ordering::AcqRel);
            buffer[..len].copy_from_slice(&bytes[start..start + len]);
            Ok(len)
        }

        fn write_at(&self, buffer: &[u8], offset: u64) -> VfsResult<usize> {
            self.write_calls.fetch_add(1, Ordering::AcqRel);
            let accepted = buffer.len().min(self.max_write.load(Ordering::Acquire));
            self.largest_write.fetch_max(accepted, Ordering::AcqRel);
            let start = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
            let end = start.checked_add(accepted).ok_or(VfsError::InvalidInput)?;
            let mut bytes = self.bytes.lock();
            let new_len = end.max(bytes.len());
            bytes.resize(new_len, 0);
            bytes[start..end].copy_from_slice(&buffer[..accepted]);
            Ok(accepted)
        }

        fn append(&self, buffer: &[u8]) -> VfsResult<(usize, u64)> {
            let mut bytes = self.bytes.lock();
            bytes.extend_from_slice(buffer);
            Ok((buffer.len(), bytes.len() as u64))
        }

        fn set_len(&self, len: u64) -> VfsResult<()> {
            let len = usize::try_from(len).map_err(|_| VfsError::InvalidInput)?;
            self.bytes.lock().resize(len, 0);
            Ok(())
        }

        fn set_symlink(&self, _target: &str) -> VfsResult<()> {
            Err(VfsError::InvalidInput)
        }
    }

    #[test]
    fn cached_growth_and_writes_persist_only_at_explicit_sync() {
        with_test_page_provider(true, |_| {
            let (cached, backing) = cached_file_fixture();

            let mut expected = Vec::new();
            for page_index in 0..3 {
                let page = vec![page_index as u8 + 1; PAGE_SIZE];
                assert_eq!(
                    cached
                        .write_at(&page[..], (page_index * PAGE_SIZE) as u64)
                        .unwrap(),
                    PAGE_SIZE
                );
                expected.extend_from_slice(&page);
            }
            let appended = vec![0xa5; PAGE_SIZE];
            assert_eq!(
                cached.append(&appended[..]).unwrap(),
                (PAGE_SIZE, (4 * PAGE_SIZE) as u64)
            );
            expected.extend_from_slice(&appended);
            cached.set_len((5 * PAGE_SIZE) as u64).unwrap();
            expected.resize(5 * PAGE_SIZE, 0);

            assert_eq!(backing.syncs(), 0);
            assert_eq!(backing.bytes(), vec![0; 5 * PAGE_SIZE]);

            cached.sync(false).unwrap();

            assert_eq!(backing.syncs(), 1);
            assert_eq!(backing.bytes(), expected);
        });
    }

    #[test]
    fn failed_explicit_sync_is_reported_and_retryable() {
        with_test_page_provider(true, |_| {
            let (cached, backing) = cached_file_fixture();
            let page = vec![0x5a; PAGE_SIZE];
            cached.write_at(&page[..], 0).unwrap();
            backing.fail_sync.store(true, Ordering::Release);

            assert_eq!(cached.sync(false), Err(VfsError::Io));
            assert_eq!(backing.syncs(), 1);
            assert_eq!(backing.bytes(), page);

            backing.fail_sync.store(false, Ordering::Release);
            cached.sync(false).unwrap();
            assert_eq!(backing.syncs(), 2);
        });
    }

    #[test]
    fn cache_pressure_writes_back_a_contiguous_page_batch() {
        with_test_page_provider(true, |_| {
            let (cached, backing) = cached_file_fixture();
            let page = vec![0x5a; PAGE_SIZE];

            for page_index in 0..=1024 {
                cached
                    .write_at(&page[..], (page_index * PAGE_SIZE) as u64)
                    .expect("write cached page");
            }

            assert_eq!(backing.write_calls(), 3);
            assert_eq!(
                backing.largest_write(),
                256 * PAGE_SIZE,
                "cache pressure must merge a bounded contiguous LRU batch"
            );
            assert_eq!(backing.syncs(), 0, "pressure writeback is not fsync");
        });
    }

    #[test]
    fn cold_read_fills_the_cache_with_bounded_contiguous_runs() {
        with_test_page_provider(true, |_| {
            let page_count = 1024;
            let expected = (0..page_count * PAGE_SIZE)
                .map(|offset| (offset / PAGE_SIZE) as u8)
                .collect::<Vec<_>>();
            let (cached, backing) = cached_file_fixture_with_bytes(expected.clone());
            let mut actual = vec![0; expected.len()];

            assert_eq!(cached.read_at(&mut actual[..], 0).unwrap(), actual.len());

            assert_eq!(actual, expected);
            assert_eq!(
                backing.read_calls(),
                page_count / 256,
                "a cold sequential read should issue one backing read per bounded cache run"
            );
            assert_eq!(backing.largest_read(), 256 * PAGE_SIZE);
        });
    }

    #[test]
    fn sequential_small_reads_use_a_syscall_independent_readahead_window() {
        with_test_page_provider(true, |_| {
            let page_count = 1024;
            let expected = (0..page_count * PAGE_SIZE)
                .map(|offset| (offset / PAGE_SIZE) as u8)
                .collect::<Vec<_>>();
            let (cached, backing) = cached_file_fixture_with_bytes(expected.clone());
            let mut actual = vec![0; expected.len()];
            let syscall_len = 8 * PAGE_SIZE;

            for offset in (0..actual.len()).step_by(syscall_len) {
                let end = (offset + syscall_len).min(actual.len());
                assert_eq!(
                    cached
                        .read_at(&mut actual[offset..end], offset as u64)
                        .unwrap(),
                    end - offset
                );
            }

            assert_eq!(actual, expected);
            assert_eq!(
                backing.read_calls(),
                page_count / 256,
                "readahead must not be bounded by the current user read size"
            );
            assert_eq!(backing.largest_read(), 256 * PAGE_SIZE);
        });
    }

    #[test]
    fn cold_read_runs_stop_at_an_existing_dirty_page() {
        with_test_page_provider(true, |_| {
            let mut expected = vec![0x11; 3 * PAGE_SIZE];
            let (cached, backing) = cached_file_fixture_with_bytes(expected.clone());
            let dirty = vec![0xa5; PAGE_SIZE];
            cached
                .write_at(&dirty[..], PAGE_SIZE as u64)
                .expect("cache one dirty page without reading its full-page backing");
            expected[PAGE_SIZE..2 * PAGE_SIZE].copy_from_slice(&dirty);
            assert_eq!(backing.read_calls(), 0);

            let mut actual = vec![0; expected.len()];
            assert_eq!(cached.read_at(&mut actual[..], 0).unwrap(), actual.len());

            assert_eq!(actual, expected);
            assert_eq!(backing.read_calls(), 2);
            assert_eq!(backing.largest_read(), PAGE_SIZE);
        });
    }

    #[test]
    fn writeback_retries_short_backing_writes() {
        with_test_page_provider(true, |_| {
            let (cached, backing) = cached_file_fixture();
            let page = vec![0x5a; PAGE_SIZE];
            cached.write_at(&page[..], 0).expect("cache one page");
            backing.max_write.store(PAGE_SIZE / 2, Ordering::Release);

            cached.sync(false).expect("write back short writes");

            assert_eq!(backing.write_calls(), 2);
            assert_eq!(backing.bytes(), page);
            assert_eq!(backing.syncs(), 1);
        });
    }

    #[test]
    fn in_memory_truncate_keeps_cache_pages_clean() {
        with_test_page_provider(true, |_| {
            let (cached, _) = cached_file_fixture_for_filesystem("tmpfs", Vec::new());
            let page = vec![0x5a; PAGE_SIZE];
            cached.write_at(&page[..], 0).unwrap();

            cached.set_len(1).unwrap();
            cached.set_len(PAGE_SIZE as u64).unwrap();

            let cache = cached.shared.page_cache.lock();
            assert!(
                !cache.peek(&0).expect("cached tmpfs page").dirty,
                "tmpfs has no backing writeback path, so truncate must not mark its cache dirty"
            );
        });
    }

    fn cached_file_fixture() -> (CachedFile, Arc<CountingFile>) {
        cached_file_fixture_with_bytes(Vec::new())
    }

    fn cached_file_fixture_with_bytes(bytes: Vec<u8>) -> (CachedFile, Arc<CountingFile>) {
        cached_file_fixture_for_filesystem("cached-writeback-test", bytes)
    }

    fn cached_file_fixture_for_filesystem(
        name: &'static str,
        bytes: Vec<u8>,
    ) -> (CachedFile, Arc<CountingFile>) {
        let filesystem = Arc::new(CountingFilesystem {
            name,
            root: std::sync::OnceLock::new(),
        });
        let backing = Arc::new(CountingFile {
            filesystem: filesystem.clone(),
            bytes: SpinNoPreempt::new(bytes),
            read_calls: AtomicUsize::new(0),
            largest_read: AtomicUsize::new(0),
            syncs: AtomicUsize::new(0),
            write_calls: AtomicUsize::new(0),
            largest_write: AtomicUsize::new(0),
            max_write: AtomicUsize::new(usize::MAX),
            fail_sync: AtomicBool::new(false),
        });
        let entry = DirEntry::new_file(
            FileNode::new(backing.clone()),
            NodeType::RegularFile,
            Reference::root(),
        );
        filesystem.root.set(entry.downgrade()).unwrap();
        let mountpoint = Mountpoint::new_root(&Filesystem::new(filesystem));
        let location = UnmanagedLocation::try_new(mountpoint.root_location()).unwrap();
        let cached = CachedFile::get_or_create(location).unwrap();
        (cached, backing)
    }
}
