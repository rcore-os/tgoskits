use alloc::{sync::Arc, vec::Vec as AllocVec};
use core::{
    mem,
    sync::atomic::{AtomicBool, Ordering},
};

use axfs_ng_vfs::VfsResult;
use heapless::Vec as InlineVec;

use super::{CachedFileShared, PageCache};

const MAX_RECLAIM_BATCH: usize = 256;

struct ReclaimGuard;

impl Drop for ReclaimGuard {
    fn drop(&mut self) {
        RECLAIM_IN_PROGRESS.store(false, Ordering::Release);
    }
}

struct CachedFileRegistry {
    files: ax_sync::SpinRwLock<AllocVec<Arc<CachedFileShared>>>,
}

impl CachedFileRegistry {
    const fn new() -> Self {
        Self {
            files: ax_sync::SpinRwLock::new(AllocVec::new()),
        }
    }

    #[cfg(feature = "ext4")]
    fn release_unlinked(&self, file: &Arc<CachedFileShared>) {
        let removed = {
            let mut registry = self.files.write();
            registry
                .iter()
                .position(|cached| Arc::ptr_eq(cached, file))
                .map(|index| registry.remove(index))
        };
        drop(removed);
    }

    fn prune(&self) {
        self.prune_with(|| {});
    }

    fn prune_with(&self, before_restore: impl FnOnce()) {
        // Cached-file destruction can take a sleepable filesystem lock.
        let mut files = {
            let mut registry = self.files.write();
            mem::take(&mut *registry)
        };
        files.retain(|cached| Arc::strong_count(cached) > 1 || cached.has_dirty_pages());
        before_restore();
        for file in files {
            let mut registry = self.files.write();
            // Unlink publishes this flag before taking the registry lock.
            // Recheck under that same lock: either restoration observes it,
            // or unlink subsequently removes the restored registration.
            if file.unlinked.load(Ordering::Acquire) {
                drop(registry);
                drop(file);
            } else {
                registry.push(file);
            }
        }
    }
}

static GLOBAL_CACHED_FILES: CachedFileRegistry = CachedFileRegistry::new();
static RECLAIM_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

fn visit_registered_cached_file<R>(
    index: usize,
    visit: impl FnOnce(&Arc<CachedFileShared>) -> R,
) -> Option<R> {
    // Retain registry read ownership instead of cloning an Arc that could
    // become the last file owner during concurrent pruning. The visitor only
    // uses try-lock clean eviction and never allocates or invokes callbacks.
    let registry = GLOBAL_CACHED_FILES.files.try_read()?;
    registry.get(index).map(visit)
}

/// Reclaims clean disk-backed cache pages without holding listener callbacks
/// under the page-cache lock.
pub fn page_cache_reclaim(num_pages: usize) -> usize {
    if RECLAIM_IN_PROGRESS.swap(true, Ordering::AcqRel) {
        return 0;
    }
    let _guard = ReclaimGuard;

    let mut reclaimed = 0;
    let target = num_pages.max(16).saturating_mul(2);
    let mut visited_files = 0;
    let scan_len = {
        let Some(registry) = GLOBAL_CACHED_FILES.files.try_read() else {
            return 0;
        };
        registry.len()
    };

    // Borrow each registry owner while trying the file locks; pressure reclaim
    // cannot become a cached file's final owner or allocate a snapshot Vec.
    // Concurrent pruning may move an entry between indices; reclaim is a
    // best-effort scan, so a later allocator retry can revisit a skipped entry.
    for index in 0..scan_len {
        let Some(freed) = visit_registered_cached_file(index, |file| {
            file.try_evict_clean_pages(target - reclaimed)
        }) else {
            continue;
        };

        reclaimed += freed;
        visited_files += 1;
        if reclaimed >= target {
            break;
        }
    }
    // The remaining quota goes to the block-layer cache trees; like the
    // page cache above, only clean folios are reclaimable here.
    #[cfg(any(feature = "ext4", feature = "fat"))]
    if reclaimed < target {
        let freed = crate::block::cache::reclaim_clean_folios(target - reclaimed);
        if freed > 0 {
            debug!("page_cache_reclaim: evicted {freed} clean block-cache folios");
        }
        reclaimed += freed;
    }

    if reclaimed > 0 {
        debug!(
            "page_cache_reclaim: evicted {} clean pages across {} files",
            reclaimed, visited_files
        );
    }
    reclaimed
}

pub(super) fn register_cached_file(file: &Arc<CachedFileShared>) {
    prune_cached_files();
    GLOBAL_CACHED_FILES.files.write().push(file.clone());
}

/// Drops the reclaim registry's ownership after the backing inode is reaped.
///
/// The removed `Arc` is dropped only after releasing the registry spin lock:
/// destroying a cached file can take its sleepable page-cache lock.
#[cfg(feature = "ext4")]
pub(super) fn release_unlinked_cached_file(file: &Arc<CachedFileShared>) {
    GLOBAL_CACHED_FILES.release_unlinked(file);
}

pub fn sync_all_cached_files(_data_only: bool) -> VfsResult<()> {
    let files = GLOBAL_CACHED_FILES.files.read().clone();
    let mut first_error = None;
    for file in &files {
        if let Err(error) = file.writeback_dirty_for_global_sync()
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }

    drop(files);
    prune_cached_files();
    first_error.map_or(Ok(()), Err)
}

fn prune_cached_files() {
    GLOBAL_CACHED_FILES.prune();
}

impl CachedFileShared {
    /// Scans the LRU and evicts up to `max` clean pages.
    ///
    /// This allocator-pressure path is allocation-free and only detaches pages
    /// from files without a live mapping endpoint.
    fn try_evict_clean_pages(&self, max: usize) -> usize {
        self.try_evict_clean_pages_with(max, || {})
    }

    fn try_evict_clean_pages_with(&self, max: usize, before_detach: impl FnOnce()) -> usize {
        // Hold endpoint exclusion until every victim has left the cache.
        // A Weak with zero strong refs cannot be resurrected; inspecting it
        // avoids acquiring an Arc whose last Drop might run arbitrary code in
        // allocator-pressure context. Leave tombstone cleanup to normal I/O.
        let Some(installed) = self.mapping_endpoint.try_lock() else {
            return 0;
        };
        if installed
            .as_ref()
            .is_some_and(|endpoint| endpoint.strong_count() != 0)
        {
            return 0;
        }
        before_detach();

        let limit = max.min(MAX_RECLAIM_BATCH);
        let mut pending: InlineVec<PageCache, MAX_RECLAIM_BATCH> = InlineVec::new();
        let Some(mut cache) = self.page_cache.try_lock() else {
            return 0;
        };
        let mut to_pop = [0u32; MAX_RECLAIM_BATCH];
        let mut count = 0;
        for (&pn, page) in cache.iter().rev() {
            if !page.dirty && page.pins == 0 && count < limit {
                to_pop[count] = pn;
                count += 1;
            }
        }
        for &pn in &to_pop[..count] {
            if let Some(page) = cache.pop(&pn) {
                // There is one push per selected key and count <= capacity.
                if pending.push(page).is_err() {
                    unreachable!("reclaim batch exceeds its selected victim count");
                }
            }
        }

        let evicted = pending.len();
        drop(cache);
        drop(installed);
        drop(pending);
        evicted
    }
}

#[cfg(test)]
struct ReclaimTestEndpoint {
    invoked: Arc<AtomicBool>,
}

#[cfg(test)]
impl super::CacheMappingEndpoint for ReclaimTestEndpoint {
    fn publish(&self, _event: super::CacheMappingEvent) -> super::CacheMappingResult {
        self.invoked.store(true, Ordering::Release);
        super::CacheMappingResult::Retired
    }
}

#[cfg(test)]
fn pressure_reclaim_is_allocation_free_and_skips_live_mappings_for_test() -> bool {
    const RECLAIM_PAGES: usize = 32;

    let file = Arc::new(CachedFileShared::new_unbounded(
        (RECLAIM_PAGES * crate::os::memory::PAGE_SIZE) as u64,
    ));
    for page_number in 0..RECLAIM_PAGES as u32 {
        file.page_cache
            .lock()
            .put(page_number, PageCache::detached_for_test());
    }

    let invoked = Arc::new(AtomicBool::new(false));
    let endpoint: Arc<dyn super::CacheMappingEndpoint> = Arc::new(ReclaimTestEndpoint {
        invoked: Arc::clone(&invoked),
    });
    *file.mapping_endpoint.lock() = Some(Arc::downgrade(&endpoint));

    let protected = file.try_evict_clean_pages(RECLAIM_PAGES);
    let protected_pages = file.page_cache.lock().len();
    drop(endpoint);
    let reclaimed = file.try_evict_clean_pages(RECLAIM_PAGES);
    protected == 0
        && protected_pages == RECLAIM_PAGES
        && reclaimed == RECLAIM_PAGES
        && !invoked.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };

    use super::*;

    std::thread_local! {
        static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
    }

    struct ObservedAllocator;

    // SAFETY: all allocation semantics are delegated unchanged to System.
    // The const thread-local Cell cannot allocate or observe another thread.
    unsafe impl GlobalAlloc for ObservedAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let _ = ALLOCATIONS.try_with(|count| {
                if let Some(value) = count.get() {
                    count.set(Some(value + 1));
                }
            });
            // SAFETY: preserve the caller's GlobalAlloc layout contract.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: this allocation came from System with the same layout.
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: ObservedAllocator = ObservedAllocator;

    #[test]
    fn pressure_reclaim_endpoint_race_never_reallocates_cache_nodes() {
        let file = CachedFileShared::new_unbounded(4096);
        file.page_cache
            .lock()
            .put(0, PageCache::detached_for_test());
        let endpoint: Arc<dyn super::super::CacheMappingEndpoint> = Arc::new(ReclaimTestEndpoint {
            invoked: Arc::new(AtomicBool::new(false)),
        });
        let mut installed_during_reclaim = false;
        ALLOCATIONS.with(|count| count.set(Some(0)));
        let reclaimed = file.try_evict_clean_pages_with(1, || {
            if let Some(mut installed) = file.mapping_endpoint.try_lock() {
                *installed = Some(Arc::downgrade(&endpoint));
                installed_during_reclaim = true;
            }
        });
        let allocations = ALLOCATIONS.with(|count| count.replace(None).unwrap());
        assert_eq!(
            allocations, 0,
            "allocator-pressure rollback must not allocate new LRU nodes"
        );
        assert!(
            !installed_during_reclaim,
            "endpoint publication must be excluded until detachment is complete"
        );
        assert_eq!(reclaimed, 1);
        assert!(file.page_cache.lock().is_empty());
    }

    #[test]
    fn allocator_pressure_reclaim_skips_mapped_pages_without_callbacks() {
        assert!(pressure_reclaim_is_allocation_free_and_skips_live_mappings_for_test());
    }

    #[cfg(feature = "ext4")]
    #[test]
    fn registry_does_not_restore_unlinked_cached_file_after_pruning() {
        // Use an isolated registry: concurrent global sync may legitimately
        // retain a temporary owner after unlink has removed the registration.
        let registry = CachedFileRegistry::new();
        let cached = Arc::new(CachedFileShared::new_unbounded(0));
        let lifetime = Arc::downgrade(&cached);
        registry.files.write().push(cached.clone());
        registry.prune_with(|| {
            cached.mark_unlinked();
            registry.release_unlinked(&cached);
        });
        drop(cached);

        assert!(
            lifetime.upgrade().is_none(),
            "pruning must not restore an unlinked inode's cache ownership"
        );
    }
}
