use alloc::{boxed::Box, sync::Arc};
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
