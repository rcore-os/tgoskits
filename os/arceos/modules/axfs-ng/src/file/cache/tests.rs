//! Page-cache locking and memory-provider regression tests.

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

    shared.add_page_listener(
        |_, _| true,
        move |_| {
            observed.store(
                listener_shared.io_lock_is_free_for_test(),
                Ordering::Release,
            );
            true
        },
    );

    shared.invoke_writeback_protect_for_test(&[0]).unwrap();

    assert!(observed_unlocked.load(Ordering::Acquire));
}

#[test]
fn writeback_protect_listener_runs_without_listener_lock() {
    let shared = Arc::new(CachedFileShared::new_unbounded(0));
    let observed_unlocked = Arc::new(AtomicBool::new(false));
    let observed = observed_unlocked.clone();
    let listener_shared = shared.clone();

    shared.add_page_listener(
        |_, _| true,
        move |_| {
            observed.store(
                listener_shared.listener_lock_is_free_for_test(),
                Ordering::Release,
            );
            true
        },
    );

    shared.invoke_writeback_protect_for_test(&[0]).unwrap();

    assert!(observed_unlocked.load(Ordering::Acquire));
}

#[test]
fn writeback_protect_does_not_hold_listener_lock_while_invoking_callbacks() {
    let shared = Arc::new(CachedFileShared::new_unbounded(0));
    let observed_unlocked = Arc::new(AtomicBool::new(false));
    let observed = observed_unlocked.clone();
    let listener_shared = shared.clone();

    shared.add_page_listener(
        |_, _| true,
        move |_| {
            observed.store(
                listener_shared.evict_listeners.try_lock().is_some(),
                Ordering::Release,
            );
            true
        },
    );

    shared.protect_dirty_pages_before_writeback(&[0]).unwrap();

    assert!(observed_unlocked.load(Ordering::Acquire));
}

#[test]
fn eviction_does_not_hold_listener_lock_while_invoking_callbacks() {
    with_test_page_provider(true, |_| {
        let shared = Arc::new(CachedFileShared::new_unbounded(0));
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed = observed_unlocked.clone();
        let listener_shared = shared.clone();

        shared.add_page_listener(
            move |_, _| {
                observed.store(
                    listener_shared.evict_listeners.try_lock().is_some(),
                    Ordering::Release,
                );
                true
            },
            |_| true,
        );

        let page = PageCache::new().unwrap();
        assert!(shared.notify_page_eviction(0, &page));
        assert!(observed_unlocked.load(Ordering::Acquire));
    });
}

#[test]
#[cfg(feature = "vfs")]
fn reclaim_eviction_does_not_hold_listener_lock_while_invoking_callbacks() {
    with_test_page_provider(true, |_| {
        let shared = Arc::new(CachedFileShared::new_unbounded(0));
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let observed = observed_unlocked.clone();
        let listener_shared = shared.clone();

        shared.add_page_listener(
            move |_, _| {
                observed.store(
                    listener_shared.evict_listeners.try_lock().is_some(),
                    Ordering::Release,
                );
                true
            },
            |_| true,
        );
        shared.page_cache.lock().put(0, PageCache::new().unwrap());

        assert_eq!(shared.try_evict_clean_pages(1), 1);
        assert!(observed_unlocked.load(Ordering::Acquire));
    });
}

#[test]
#[cfg(feature = "vfs")]
fn reclaim_reserves_the_page_number_until_listener_decision() {
    with_test_page_provider(true, |_| {
        let shared = Arc::new(CachedFileShared::new_unbounded(0));
        let replacement_inserted = Arc::new(AtomicBool::new(false));
        let inserted = replacement_inserted.clone();
        let listener_shared = shared.clone();

        shared.add_page_listener(
            move |pn, _| {
                if let Some(_io) = listener_shared.io_lock.try_lock() {
                    let mut replacement = PageCache::new().unwrap();
                    replacement.data()[0] = 0xa5;
                    replacement.mark_dirty();
                    listener_shared.page_cache.lock().put(pn, replacement);
                    inserted.store(true, Ordering::Release);
                }
                false
            },
            |_| true,
        );
        let mut original = PageCache::new().unwrap();
        original.data()[0] = 0x11;
        shared.page_cache.lock().put(0, original);

        assert_eq!(shared.try_evict_clean_pages(1), 0);
        assert!(
            !replacement_inserted.load(Ordering::Acquire),
            "reclaim must serialize the pop/listener/reinsert transaction with cache I/O"
        );
        let mut cache = shared.page_cache.lock();
        assert_eq!(cache.peek_mut(&0).unwrap().data()[0], 0x11);
    });
}

#[test]
#[cfg(feature = "vfs")]
fn global_cache_registry_deduplicates_inode_shared_cache_state() {
    let shared = Arc::new(CachedFileShared::new_unbounded(0));

    register_cached_file(&shared);
    register_cached_file(&shared);

    let occurrences = GLOBAL_CACHED_FILES
        .read()
        .iter()
        .filter(|registered| Arc::ptr_eq(registered, &shared))
        .count();
    GLOBAL_CACHED_FILES
        .write()
        .retain(|registered| !Arc::ptr_eq(registered, &shared));
    assert_eq!(
        occurrences, 1,
        "hard-link aliases must not pin duplicate global cache registry entries"
    );
}

#[test]
fn eviction_refusal_is_reported_to_the_cache_owner() {
    with_test_page_provider(true, |_| {
        let shared = CachedFileShared::new_unbounded(0);
        shared.add_page_listener(|_, _| false, |_| true);

        let page = PageCache::new().unwrap();
        assert!(!shared.notify_page_eviction(0, &page));
    });
}
