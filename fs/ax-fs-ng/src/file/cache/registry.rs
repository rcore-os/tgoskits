//! Strong ownership of disk-backed pages until writeback or safe retirement.

use alloc::{sync::Arc, vec::Vec};

use axfs_ng_vfs::VfsResult;

use super::CachedFileShared;

static GLOBAL_CACHED_FILES: ax_sync::SpinRwLock<Vec<Arc<CachedFileShared>>> =
    ax_sync::SpinRwLock::new(Vec::new());
pub(super) fn register_cached_file(file: &Arc<CachedFileShared>) {
    prune_cached_files();
    let mut registry = GLOBAL_CACHED_FILES.write();
    if !registry.iter().any(|cached| Arc::ptr_eq(cached, file)) {
        registry.push(file.clone());
    }
}

#[cfg(feature = "vfs")]
pub fn sync_all_cached_files(_data_only: bool) -> VfsResult<()> {
    let files = GLOBAL_CACHED_FILES.read().clone();
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

/// Writes a finite snapshot of this mount's pages, including open-unlinked
/// files no longer present in the inode lookup index. Metadata commit is the
/// caller's next phase; it must not hold the ext4 or commit lock here.
#[cfg(feature = "ext4")]
pub(crate) fn writeback_filesystem_pages(
    filesystem: &dyn axfs_ng_vfs::FilesystemOps,
) -> VfsResult<()> {
    let files = GLOBAL_CACHED_FILES.read().clone();
    let mut first_error = None;
    for file in &files {
        let backing = file.backing()?;
        if !core::ptr::addr_eq(backing.filesystem(), filesystem) {
            continue;
        }
        if let Err(error) = file.writeback_dirty_for_global_sync() {
            log::error!(
                "filesystem {} inode {} page writeback failed: {error:?}",
                filesystem.name(),
                backing.inode()
            );
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Releases registry ownership only after the final mount lease disappears.
/// The filesystem excludes new leases; failed dirty files remain registered.
#[cfg(feature = "ext4")]
pub(crate) fn retire_filesystem_cache(
    filesystem: &dyn axfs_ng_vfs::FilesystemOps,
) -> VfsResult<()> {
    let files = GLOBAL_CACHED_FILES.read().clone();
    let mut first_error = None;
    for file in &files {
        let backing = file.backing()?;
        if !core::ptr::addr_eq(backing.filesystem(), filesystem) {
            continue;
        }
        match file.writeback_dirty_for_global_sync() {
            Ok(()) => release_cached_file(file),
            Err(error) => {
                log::error!(
                    "ext4 inode {} cache retirement failed: {error:?}",
                    backing.inode()
                );
                first_error.get_or_insert(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn prune_cached_files() {
    let mut cursor = 0;
    loop {
        let unused = take_unused_file(&mut GLOBAL_CACHED_FILES.write(), &mut cursor);
        let Some(unused) = unused else {
            return;
        };
        // Destruction may reach sleepable filesystem locks. Live and dirty
        // owners never leave the registry while that destruction is running.
        drop(unused);
    }
}

fn take_unused_file(
    registry: &mut Vec<Arc<CachedFileShared>>,
    cursor: &mut usize,
) -> Option<CachedFileShared> {
    let mut remaining = registry.len().saturating_sub(*cursor);
    while remaining != 0 {
        remaining -= 1;
        let file = &registry[*cursor];
        let unused = Arc::strong_count(file) == 1
            && file
                .retired_pages
                .try_lock()
                .is_some_and(|pages| pages.is_empty())
            && file
                .page_cache
                .try_lock()
                .is_some_and(|pages| pages.iter().all(|(_, page)| !page.dirty));
        if unused {
            // The inode index contains weak references. Checking strong_count
            // alone is insufficient: a concurrent upgrade can acquire this
            // owner. try_unwrap atomically closes that race, or restores the
            // still-live owner before the registry lock is released.
            match Arc::try_unwrap(registry.swap_remove(*cursor)) {
                Ok(file) => return Some(file),
                Err(file) => {
                    registry.push(file);
                    continue;
                }
            }
        }
        *cursor += 1;
    }
    None
}

#[cfg(feature = "ext4")]
pub(super) fn release_cached_file(file: &Arc<CachedFileShared>) {
    let retired = {
        let mut registry = GLOBAL_CACHED_FILES.write();
        registry
            .iter()
            .position(|cached| Arc::ptr_eq(cached, file))
            .map(|index| registry.swap_remove(index))
    };
    // Destruction may acquire the sleepable filesystem lock.
    drop(retired);
}

/// Visits a bounded number of owners, releasing the registry before callbacks.
#[cfg(feature = "vfs")]
pub(super) fn reclaim_clean_pages(target: usize) -> (usize, usize) {
    let Some(limit) = GLOBAL_CACHED_FILES.try_read().map(|files| files.len()) else {
        return (0, 0);
    };
    let mut reclaimed = 0;
    let mut visited = 0;
    for index in 0..limit {
        let file = GLOBAL_CACHED_FILES
            .try_read()
            .and_then(|files| files.get(index).cloned());
        let Some(file) = file else {
            break;
        };
        reclaimed += file.try_evict_clean_pages(target.saturating_sub(reclaimed));
        visited += 1;
        if reclaimed >= target {
            break;
        }
    }
    (reclaimed, visited)
}

#[cfg(test)]
mod tests {
    use super::{super::PageCache, *};
    use crate::os::memory::test_support::with_test_page_provider;

    #[test]
    fn pruning_keeps_live_and_dirty_owners_visible_while_destroying_unused_files() {
        with_test_page_provider(true, |_| {
            let unused = Arc::new(CachedFileShared::new_unbounded(0));
            let unused_weak = Arc::downgrade(&unused);
            let live = Arc::new(CachedFileShared::new_unbounded(0));
            let dirty = Arc::new(CachedFileShared::new_unbounded(
                super::super::PAGE_SIZE as u64,
            ));
            let dirty_weak = Arc::downgrade(&dirty);
            let mut page = PageCache::new().unwrap();
            page.mark_dirty();
            dirty.page_cache.lock().put(0, page);
            let mut registry = alloc::vec![unused, live.clone(), dirty];
            let mut cursor = 0;

            let removed = take_unused_file(&mut registry, &mut cursor).unwrap();

            assert!(unused_weak.upgrade().is_none());
            assert_eq!(registry.len(), 2);
            assert!(registry.iter().any(|file| Arc::ptr_eq(file, &live)));
            assert!(dirty_weak.upgrade().is_some());
            assert!(take_unused_file(&mut registry, &mut cursor).is_none());
            drop(removed);
        });
    }
}
