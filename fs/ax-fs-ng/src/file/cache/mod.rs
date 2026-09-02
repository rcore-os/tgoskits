mod readahead;
#[cfg(feature = "vfs")]
mod reclaim;
mod resize;
mod writeback;

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use ax_io::prelude::*;
use axfs_ng_vfs::{FileNode, FilesystemOps, Location, VfsError, VfsResult};
use lru::LruCache;
use readahead::ReadAheadState;
#[cfg(feature = "vfs")]
pub use reclaim::{page_cache_reclaim, sync_all_cached_files};

use super::page::PageCache;
use crate::os::{memory::PAGE_SIZE, sync::SleepMutex as Mutex};

const DISK_PAGE_CACHE_CAP: usize = 512;

type CachedFileKey = (usize, u64);
type InodeCacheIndex = BTreeMap<CachedFileKey, Weak<CachedFileShared>>;

static CACHED_FILE_BY_INODE: ax_lazyinit::LazyLock<Mutex<InodeCacheIndex>> =
    ax_lazyinit::LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Stable identity of one page-cache ownership domain.
///
/// Clones and independently opened handles that resolve to the same
/// [`CachedFileShared`] carry the same identity.  A newly created cache owner
/// always receives a fresh identity, so users such as shared-futex lookup do
/// not need to turn an `Arc` address into a public integer key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CachedFileIdentity(u64);

impl CachedFileIdentity {
    fn allocate() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let Ok(identity) = NEXT_ID.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        }) else {
            panic!("cached-file identity space exhausted");
        };
        Self(identity)
    }

    /// Returns the numeric component of this opaque identity.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Physical identity of a frame currently owned by one [`CachedFile`].
///
/// This is an observation token, not ownership.  The cache keeps the matching
/// [`PageCache`] indexed, pinned, or detached in a local eviction batch while a
/// mapping endpoint processes the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedFrameIdentity(usize);

impl CachedFrameIdentity {
    const fn new(paddr: usize) -> Self {
        Self(paddr)
    }

    pub const fn paddr(self) -> usize {
        self.0
    }
}

/// Exact file-cache page named by a mapping lifecycle event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePageIdentity {
    file: CachedFileIdentity,
    file_epoch: u64,
    page_number: u32,
    frame: CachedFrameIdentity,
}

impl CachePageIdentity {
    const fn new(
        file: CachedFileIdentity,
        file_epoch: u64,
        page_number: u32,
        frame: CachedFrameIdentity,
    ) -> Self {
        Self {
            file,
            file_epoch,
            page_number,
            frame,
        }
    }

    pub const fn file(self) -> CachedFileIdentity {
        self.file
    }

    pub const fn file_epoch(self) -> u64 {
        self.file_epoch
    }

    pub const fn page_number(self) -> u32 {
        self.page_number
    }

    pub const fn frame(self) -> CachedFrameIdentity {
        self.frame
    }
}

/// Mapping operation published after releasing page-cache and cached-I/O locks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMappingEvent {
    Evict(CachePageIdentity),
    WritebackProtect(CachePageIdentity),
}

impl CacheMappingEvent {
    pub const fn page(self) -> CachePageIdentity {
        match self {
            Self::Evict(page) | Self::WritebackProtect(page) => page,
        }
    }

    const fn no_endpoint_result(self) -> CacheMappingResult {
        match self {
            Self::Evict(_) => CacheMappingResult::Retired,
            Self::WritebackProtect(_) => CacheMappingResult::Protected,
        }
    }
}

/// Completion state returned by the sole mapping owner for a cached file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMappingResult {
    Retired,
    Protected,
    Busy,
    /// The PTE was unpublished but its TLB obligation is not acknowledged yet.
    Quarantined,
    /// The endpoint could not prove either completion or an exact rollback.
    Failed,
}

/// Why a best-effort file-cache pageout could not reclaim every candidate.
///
/// The cache retains every candidate it could not reclaim.  Linux
/// `MADV_PAGEOUT` deliberately does not expose these transient reclaim
/// failures as syscall errors, while internal callers still need a typed fact
/// for metrics, retry policy, and invariant auditing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePageoutDeferred {
    Writeback(VfsError),
    Eviction(VfsError),
}

/// Result of one bounded, best-effort pageout request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePageoutResult {
    reclaimed: usize,
    deferred: Option<CachePageoutDeferred>,
}

impl CachePageoutResult {
    const fn complete(reclaimed: usize) -> Self {
        Self {
            reclaimed,
            deferred: None,
        }
    }

    const fn deferred(reclaimed: usize, reason: CachePageoutDeferred) -> Self {
        Self {
            reclaimed,
            deferred: Some(reason),
        }
    }

    pub const fn reclaimed(self) -> usize {
        self.reclaimed
    }

    pub const fn deferred_reason(self) -> Option<CachePageoutDeferred> {
        self.deferred
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InvalidateCleanOutcome {
    invalidated: usize,
    busy: bool,
}

/// Reverse-mapping boundary owned by the VM subsystem using this page cache.
///
/// One live endpoint is installed per [`CachedFileIdentity`].  Implementations
/// must not enter cached I/O from `publish`; the cache invokes it without any
/// page-cache, cached-I/O, or endpoint-publication lock held.
pub trait CacheMappingEndpoint: Send + Sync {
    fn publish(&self, event: CacheMappingEvent) -> CacheMappingResult;
}

/// A transient, typed pin on one page-cache frame.
///
/// The cache index remains the physical owner.  The pin only prevents reclaim
/// or truncate from detaching that owner while a caller publishes a PTE; it
/// never exposes mutable page data outside the cache lock.
pub struct CachedPagePin {
    shared: Arc<CachedFileShared>,
    page_number: u32,
    paddr: usize,
}

impl CachedPagePin {
    pub const fn paddr(&self) -> usize {
        self.paddr
    }
}

impl Drop for CachedPagePin {
    fn drop(&mut self) {
        let mut cache = self.shared.page_cache.lock();
        let Some(page) = cache.get_mut(&self.page_number) else {
            // Reclaim/truncate must reject pinned entries, so disappearance is
            // an ownership protocol violation.  The underlying frame cannot be
            // repaired here because its cache owner is already unknown.
            warn!(
                "pinned cached page {} disappeared before pin release",
                self.page_number
            );
            return;
        };
        if page.pins == 0 {
            warn!("cached page pin underflow for page {}", self.page_number);
            return;
        }
        page.pins -= 1;
    }
}

/// Serializes a file mapping-layout change against new mmap PTE publication.
///
/// This cannot be the cached-I/O mutex: an invalidator enters an address space,
/// while the fault path reaches cached I/O from that address space. Faults
/// instead observe this publication barrier and retry after it is released.
struct MappingUpdateGuard {
    shared: Arc<CachedFileShared>,
}

impl Drop for MappingUpdateGuard {
    fn drop(&mut self) {
        self.shared
            .mapping_update_in_progress
            .store(false, Ordering::Release);
    }
}

struct CachedFileShared {
    identity: CachedFileIdentity,
    page_cache: Mutex<LruCache<u32, PageCache>>,
    io_lock: Mutex<()>,
    mapping_endpoint: Mutex<Option<Weak<dyn CacheMappingEndpoint>>>,
    backing: Option<FileNode>,
    len: AtomicU64,
    /// Generation of mapping-changing file operations (truncate, hole punch,
    /// collapse/insert).  File-backed VMAs snapshot this value so stale page
    /// objects cannot be mistaken for the current file view.
    mapping_epoch: AtomicU64,
    mapping_update_in_progress: AtomicBool,
    unlinked: AtomicBool,
}

impl CachedFileShared {
    pub fn new(len: u64, backing: FileNode) -> Self {
        Self {
            identity: CachedFileIdentity::allocate(),
            page_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(DISK_PAGE_CACHE_CAP).unwrap(),
            )),
            io_lock: Mutex::new(()),
            mapping_endpoint: Mutex::new(None),
            backing: Some(backing),
            len: AtomicU64::new(len),
            mapping_epoch: AtomicU64::new(0),
            mapping_update_in_progress: AtomicBool::new(false),
            unlinked: AtomicBool::new(false),
        }
    }

    pub fn new_unbounded(len: u64) -> Self {
        Self {
            identity: CachedFileIdentity::allocate(),
            page_cache: Mutex::new(LruCache::unbounded()),
            io_lock: Mutex::new(()),
            mapping_endpoint: Mutex::new(None),
            backing: None,
            len: AtomicU64::new(len),
            mapping_epoch: AtomicU64::new(0),
            mapping_update_in_progress: AtomicBool::new(false),
            unlinked: AtomicBool::new(false),
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

    fn prepare_mapping_epoch(&self) -> VfsResult<u64> {
        self.mapping_epoch
            .load(Ordering::Acquire)
            .checked_add(1)
            .ok_or(VfsError::ValueOverflow)
    }

    fn publish_mapping_epoch(&self, epoch: u64) {
        self.mapping_epoch.store(epoch, Ordering::Release);
    }

    fn prepared_mapping_epoch_is_current(&self, epoch: u64) -> bool {
        self.mapping_epoch.load(Ordering::Acquire) == epoch - 1
    }

    fn set_len(&self, len: u64) {
        self.len.store(len, Ordering::Release);
    }

    fn backing(&self) -> VfsResult<&FileNode> {
        self.backing.as_ref().ok_or(VfsError::InvalidInput)
    }

    /// Takes a strong snapshot of the endpoint capability and releases the
    /// publication lock before any VM code can run.
    fn mapping_endpoint(&self) -> Option<Arc<dyn CacheMappingEndpoint>> {
        let mut installed = self.mapping_endpoint.lock();
        match installed.as_ref().and_then(Weak::upgrade) {
            Some(endpoint) => Some(endpoint),
            None => {
                *installed = None;
                None
            }
        }
    }

    fn has_mapping_endpoint(&self) -> bool {
        self.mapping_endpoint().is_some()
    }

    fn publish_mapping_event(&self, event: CacheMappingEvent) -> CacheMappingResult {
        let Some(endpoint) = self.mapping_endpoint() else {
            return event.no_endpoint_result();
        };
        endpoint.publish(event)
    }

    fn cache_page_identity(&self, page_number: u32, paddr: usize) -> CachePageIdentity {
        CachePageIdentity::new(
            self.identity,
            self.mapping_epoch.load(Ordering::Acquire),
            page_number,
            CachedFrameIdentity::new(paddr),
        )
    }

    #[cfg(all(feature = "ext4", feature = "vfs"))]
    fn mark_unlinked(&self) {
        self.unlinked.store(true, Ordering::Release);
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
    fn endpoint_lock_is_free_for_test(&self) -> bool {
        self.mapping_endpoint.try_lock().is_some()
    }

    #[cfg(test)]
    fn page_cache_lock_is_free_for_test(&self) -> bool {
        self.page_cache.try_lock().is_some()
    }
}

impl Drop for CachedFileShared {
    fn drop(&mut self) {
        if !self.unlinked.load(Ordering::Acquire) {
            return;
        }
        for (_, page) in self.page_cache.lock().iter_mut() {
            page.dirty = false;
        }
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
        let inode_key =
            should_share_cached_file_by_inode(&location).then(|| cached_file_key(&location));
        let candidate = if in_memory {
            Arc::new(CachedFileShared::new_unbounded(len))
        } else {
            let backing = location.entry().as_file()?.clone();
            Arc::new(CachedFileShared::new(len, backing))
        };
        let (created, owner_created) = if let Some(key) = inode_key {
            publish_inode_cached_file(key, candidate)
        } else {
            (candidate, true)
        };
        let user_data = FileUserData::Strong(created.clone());

        let shared = {
            let mut guard = location.user_data();
            if let Some(shared) = guard
                .get::<FileUserData>()
                .as_deref()
                .map(FileUserData::get)
            {
                shared
            } else {
                guard.insert(user_data);
                created
            }
        };

        // tmpfs and ramfs have no backing store, so evicting clean pages would
        // lose data. Only register disk-backed files for reclaim.
        #[cfg(feature = "vfs")]
        if owner_created && !in_memory {
            reclaim::register_cached_file(&shared);
        }
        #[cfg(not(feature = "vfs"))]
        let _ = owner_created;

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

    /// Returns the stable identity of the shared page-cache owner.
    pub fn identity(&self) -> CachedFileIdentity {
        self.shared.identity
    }

    /// Returns the current cached file length.
    pub fn len(&self) -> u64 {
        self.shared.len()
    }

    /// Returns whether a file page currently has a page-cache object.
    ///
    /// This is a snapshot query for `mincore`; it does not update LRU order,
    /// perform I/O, or manufacture a cache entry.
    pub fn is_page_cached(&self, page_number: u32) -> bool {
        self.shared.page_cache.lock().contains(&page_number)
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

    /// Current generation of the file's page-to-offset mapping.
    pub fn mapping_epoch(&self) -> u64 {
        self.shared.mapping_epoch.load(Ordering::Acquire)
    }

    fn cache_page_identity(&self, page_number: u32, paddr: usize) -> CachePageIdentity {
        self.shared.cache_page_identity(page_number, paddr)
    }

    /// Installs the sole VM reverse-mapping endpoint for this cached file.
    ///
    /// Reinstalling the same endpoint is idempotent.  A different live endpoint
    /// is rejected because two independent mapping owners would make truncate,
    /// eviction, and writeback protection impossible to complete atomically.
    /// The cache stores only a `Weak` capability; dropping the VM domain proves
    /// that no mappings remain and allows a later owner to be installed.
    pub fn install_mapping_endpoint(
        &self,
        endpoint: &Arc<dyn CacheMappingEndpoint>,
    ) -> VfsResult<()> {
        let mut installed = self.shared.mapping_endpoint.lock();
        match installed.as_ref().and_then(Weak::upgrade) {
            Some(current) if Arc::ptr_eq(&current, endpoint) => Ok(()),
            Some(_) => Err(VfsError::AlreadyExists),
            None => {
                *installed = Some(Arc::downgrade(endpoint));
                Ok(())
            }
        }
    }

    /// Invokes the endpoint for an indexed page while holding a transient cache
    /// pin. A cache miss means no PTE can still own that frame and is therefore
    /// already retired.
    pub(crate) fn invalidate_page_mappings(&self, pn: u32) -> VfsResult<bool> {
        let Some(pin) = self.pin_cached_page_if_present(pn)? else {
            return Ok(true);
        };
        let event = CacheMappingEvent::Evict(self.cache_page_identity(pn, pin.paddr()));
        let result = self.shared.publish_mapping_event(event);
        drop(pin);
        match result {
            CacheMappingResult::Retired => Ok(true),
            CacheMappingResult::Busy | CacheMappingResult::Quarantined => Ok(false),
            CacheMappingResult::Protected | CacheMappingResult::Failed => Err(VfsError::BadState),
        }
    }

    /// Invalidates clean, unpinned cache pages in `[start_pn, end_pn)`.
    ///
    /// Candidates are detached from the cache index first. Mapping listeners
    /// are invoked only after releasing that lock, and a page is restored when
    /// any listener cannot retire all of its reverse mappings. Dirty or pinned
    /// pages make the request busy rather than being dropped behind an active
    /// writer or PTE publisher.
    fn invalidate_clean_pages_inner(
        &self,
        start_pn: u32,
        end_pn: u32,
    ) -> VfsResult<InvalidateCleanOutcome> {
        if start_pn > end_pn {
            return Err(VfsError::InvalidInput);
        }
        if start_pn == end_pn {
            return Ok(InvalidateCleanOutcome {
                invalidated: 0,
                busy: false,
            });
        }
        if self.in_memory {
            // The page cache is the backing store for tmpfs-like files.  With
            // no swap object to retain the contents, dropping a clean page
            // would lose file data rather than merely invalidate a cache copy.
            return Ok(InvalidateCleanOutcome {
                invalidated: 0,
                busy: false,
            });
        }

        let mut pending = Vec::new();
        let mut busy = false;
        {
            let mut cache = self.shared.page_cache.lock();
            let candidate_count = cache
                .iter()
                .filter(|(pn, page)| {
                    **pn >= start_pn && **pn < end_pn && !page.dirty && page.pins == 0
                })
                .count();
            pending
                .try_reserve(candidate_count)
                .map_err(|_| VfsError::NoMemory)?;

            let mut candidates = Vec::new();
            candidates
                .try_reserve(candidate_count)
                .map_err(|_| VfsError::NoMemory)?;
            for (&pn, page) in cache.iter() {
                if pn < start_pn || pn >= end_pn {
                    continue;
                }
                if page.dirty || page.pins != 0 {
                    busy = true;
                } else {
                    candidates.push(pn);
                }
            }
            for pn in candidates {
                if let Some(page) = cache.pop(&pn) {
                    pending.push((pn, page));
                }
            }
        }

        let mut invalidated = 0;
        let mut first_error = None;
        for (pn, page) in pending {
            let result = match page.paddr() {
                Ok(paddr) => self.shared.publish_mapping_event(CacheMappingEvent::Evict(
                    self.cache_page_identity(pn, paddr),
                )),
                Err(error) => {
                    first_error.get_or_insert(error);
                    CacheMappingResult::Failed
                }
            };
            match result {
                CacheMappingResult::Retired => invalidated += 1,
                CacheMappingResult::Busy | CacheMappingResult::Quarantined => {
                    busy = true;
                    self.shared.page_cache.lock().put(pn, page);
                }
                CacheMappingResult::Protected | CacheMappingResult::Failed => {
                    first_error.get_or_insert(VfsError::BadState);
                    self.shared.page_cache.lock().put(pn, page);
                }
            }
        }

        first_error.map_or(Ok(InvalidateCleanOutcome { invalidated, busy }), Err)
    }

    pub fn invalidate_clean_pages(&self, start_pn: u32, end_pn: u32) -> VfsResult<usize> {
        let outcome = self.invalidate_clean_pages_inner(start_pn, end_pn)?;
        if outcome.busy {
            Err(VfsError::ResourceBusy)
        } else {
            Ok(outcome.invalidated)
        }
    }

    /// Writes back dirty candidates and then attempts typed rmap eviction.
    ///
    /// Operational writeback or eviction failures are returned as `Deferred`:
    /// no dirty cache owner is discarded, and a later reclaimer may retry.
    /// Ownership/protocol corruption remains an error rather than being hidden
    /// behind Linux's best-effort syscall semantics.
    pub fn pageout_pages(&self, start_pn: u32, end_pn: u32) -> VfsResult<CachePageoutResult> {
        if start_pn > end_pn {
            return Err(VfsError::InvalidInput);
        }
        if self.in_memory {
            return Err(VfsError::OperationNotSupported);
        }

        let dirty = self.dirty_pages_in_range(start_pn, end_pn);
        if !dirty.is_empty()
            && let Err(error) = self.writeback_pages(&dirty)
        {
            return match error {
                VfsError::BadState
                | VfsError::FilesystemCorrupted
                | VfsError::InvalidData
                | VfsError::InvalidInput
                | VfsError::ValueOverflow => Err(error),
                _ => Ok(CachePageoutResult::deferred(
                    0,
                    CachePageoutDeferred::Writeback(error),
                )),
            };
        }

        match self.invalidate_clean_pages_inner(start_pn, end_pn) {
            Ok(outcome) if outcome.busy => Ok(CachePageoutResult::deferred(
                outcome.invalidated,
                CachePageoutDeferred::Eviction(VfsError::ResourceBusy),
            )),
            Ok(outcome) => Ok(CachePageoutResult::complete(outcome.invalidated)),
            Err(
                error @ (VfsError::BadState
                | VfsError::FilesystemCorrupted
                | VfsError::InvalidData
                | VfsError::InvalidInput
                | VfsError::ValueOverflow),
            ) => Err(error),
            Err(error) => Ok(CachePageoutResult::deferred(
                0,
                CachePageoutDeferred::Eviction(error),
            )),
        }
    }

    fn page_or_insert<'a>(
        &self,
        file: &FileNode,
        cache: &'a mut LruCache<u32, PageCache>,
        pn: u32,
        read_backing: bool,
        allow_deferred_evict: bool,
    ) -> VfsResult<(&'a mut PageCache, Option<(u32, PageCache)>)> {
        // TODO: Matching the result of `get_mut` confuses compiler. See
        // https://users.rust-lang.org/t/return-do-not-release-mutable-borrow/55757.
        if cache.contains(&pn) {
            return Ok((cache.get_mut(&pn).unwrap(), None));
        }
        let mut evicted = None;
        if cache.len() >= cache.cap().get() {
            if self.shared.has_mapping_endpoint() && !allow_deferred_evict {
                // Internal buffered-I/O paths cannot call unknown mmap
                // invalidators while holding `page_cache`/`io_lock`, and they
                // have no way to return an eviction token to their caller.
                // Leave the old page resident for the explicit reclaim worker.
                return Err(VfsError::ResourceBusy);
            }
            // Cache is full, remove the least recently used page
            if let Some((pn, page)) = cache.pop_lru() {
                if page.dirty || page.pins != 0 {
                    // Dirty writeback and pinned-page retirement are explicit
                    // operations.  Never block on backing I/O or invalidate a
                    // live frame while holding the cache index lock.
                    cache.put(pn, page);
                    return Err(VfsError::ResourceBusy);
                }
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
            self.page_or_insert(file, &mut guard, pn, false, false)?;
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
            let page = self
                .page_or_insert(file, &mut guard, page_number, false, false)?
                .0;
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
        if self
            .shared
            .mapping_update_in_progress
            .load(Ordering::Acquire)
        {
            return Err(VfsError::ResourceBusy);
        }
        let _io = self.shared.io_lock.lock();
        if self
            .shared
            .mapping_update_in_progress
            .load(Ordering::Acquire)
        {
            return Err(VfsError::ResourceBusy);
        }
        let mut guard = self.shared.page_cache.lock();
        guard.get_mut(&pn).ok_or(VfsError::BadState)?.mark_dirty();
        Ok(())
    }

    /// Loads and transiently pins one cache page for PTE publication.
    ///
    /// Backing I/O happens without the page-cache index lock.  The returned pin
    /// carries only frame identity, so callers cannot invoke unknown code while
    /// borrowing mutable cache state.
    pub fn pin_page_or_insert(&self, pn: u32) -> VfsResult<CachedPagePin> {
        if self
            .shared
            .mapping_update_in_progress
            .load(Ordering::Acquire)
        {
            return Err(VfsError::ResourceBusy);
        }
        let _io = self.shared.io_lock.lock();
        if self
            .shared
            .mapping_update_in_progress
            .load(Ordering::Acquire)
        {
            return Err(VfsError::ResourceBusy);
        }
        self.populate_page_window(self.inner.entry().as_file()?, pn, 1)?;
        self.pin_cached_page(pn)
    }

    fn begin_mapping_update(&self) -> VfsResult<MappingUpdateGuard> {
        self.shared
            .mapping_update_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| VfsError::ResourceBusy)?;
        Ok(MappingUpdateGuard {
            shared: self.shared.clone(),
        })
    }

    /// Pins an already-resident cache page without performing backing I/O.
    ///
    /// Address-space retirement uses this after validating an installed PTE:
    /// the pin keeps the cache-owned frame alive until that PTE's tagged TLB
    /// receipt is acknowledged.  A missing entry is an ownership mismatch,
    /// not a request to fault the page back in while holding MM metadata.
    pub fn pin_cached_page(&self, pn: u32) -> VfsResult<CachedPagePin> {
        let mut cache = self.shared.page_cache.lock();
        let page = cache.get_mut(&pn).ok_or(VfsError::BadState)?;
        let paddr = page.paddr()?;
        page.pins = page.pins.checked_add(1).ok_or(VfsError::ValueOverflow)?;
        Ok(CachedPagePin {
            shared: self.shared.clone(),
            page_number: pn,
            paddr,
        })
    }

    fn pin_cached_page_if_present(&self, pn: u32) -> VfsResult<Option<CachedPagePin>> {
        let mut cache = self.shared.page_cache.lock();
        let Some(page) = cache.get_mut(&pn) else {
            return Ok(None);
        };
        let paddr = page.paddr()?;
        page.pins = page.pins.checked_add(1).ok_or(VfsError::ValueOverflow)?;
        Ok(Some(CachedPagePin {
            shared: self.shared.clone(),
            page_number: pn,
            paddr,
        }))
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
            let next_epoch = self.shared.prepare_mapping_epoch()?;
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
            self.shared.publish_mapping_epoch(next_epoch);
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

fn should_share_cached_file_by_inode(location: &Location) -> bool {
    matches!(location.filesystem().name(), "ext4" | "tmpfs" | "ramfs")
}

fn filesystem_key(filesystem: &dyn FilesystemOps) -> usize {
    filesystem as *const dyn FilesystemOps as *const () as usize
}

fn cached_file_key(location: &Location) -> CachedFileKey {
    (filesystem_key(location.filesystem()), location.inode())
}

fn publish_inode_cached_file(
    key: CachedFileKey,
    candidate: Arc<CachedFileShared>,
) -> (Arc<CachedFileShared>, bool) {
    let mut cache = CACHED_FILE_BY_INODE.lock();
    match cache.get(&key).and_then(Weak::upgrade) {
        Some(shared) => (shared, false),
        None => {
            cache.insert(key, Arc::downgrade(&candidate));
            (candidate, true)
        }
    }
}

#[cfg(feature = "ext4")]
pub(crate) fn forget_cached_file_key(filesystem: &dyn FilesystemOps, inode: u64) {
    if filesystem.name() == "ext4" {
        let cached = CACHED_FILE_BY_INODE
            .lock()
            .remove(&(filesystem_key(filesystem), inode))
            .and_then(|cached| cached.upgrade());
        #[cfg(feature = "vfs")]
        if let Some(cached) = cached {
            cached.mark_unlinked();
            reclaim::release_unlinked_cached_file(&cached);
        }
        #[cfg(not(feature = "vfs"))]
        let _ = cached;
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
