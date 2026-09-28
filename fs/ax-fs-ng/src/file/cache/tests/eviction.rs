use super::*;

#[test]
fn eviction_completes_short_writes_before_releasing_dirty_page() {
    with_test_page_provider(true, |_| {
        let (cached, backing) = one_page_cache();
        backing.state.lock().unwrap().max_write = 128;
        cached.with_page_or_insert(1, |_, _| Ok(())).unwrap();
        assert_eq!(
            &backing.state.lock().unwrap().physical_data[..PAGE_SIZE],
            vec![0xa5; PAGE_SIZE],
        );
        assert!(cached.shared.page_cache.lock().contains(&1));
    });
}

#[test]
fn failed_eviction_retains_the_only_dirty_copy_for_retry() {
    with_test_page_provider(true, |_| {
        let (cached, backing) = one_page_cache();
        backing.fail_next_write();
        assert_eq!(
            cached.with_page_or_insert(1, |_, _| Ok(())),
            Err(VfsError::Io),
        );
        assert_retained_dirty_page(&cached);
        cached.with_page_or_insert(1, |_, _| Ok(())).unwrap();
        assert_eq!(
            &backing.state.lock().unwrap().physical_data[..PAGE_SIZE],
            vec![0xa5; PAGE_SIZE],
        );
    });
}

#[test]
fn zero_progress_eviction_keeps_dirty_page_in_cache() {
    with_test_page_provider(true, |_| {
        let (cached, backing) = one_page_cache();
        backing.state.lock().unwrap().max_write = 0;
        assert_eq!(
            cached.with_page_or_insert(1, |_, _| Ok(())),
            Err(VfsError::Io),
        );
        assert_retained_dirty_page(&cached);
        backing.state.lock().unwrap().max_write = usize::MAX;
        cached.sync(false).unwrap();
    });
}

#[test]
fn failed_replacement_read_does_not_remove_or_notify_old_page() {
    with_test_page_provider(true, |_| {
        let (cached, backing) = one_page_cache();
        let notified = Arc::new(AtomicBool::new(false));
        let listener_notified = notified.clone();
        cached.add_evict_listener(move |_, _| {
            listener_notified.store(true, Ordering::Release);
            true
        });
        backing.fail_next_read.store(true, Ordering::Release);
        assert_eq!(
            cached.with_page_or_insert(1, |_, _| Ok(())),
            Err(VfsError::Io),
        );
        assert!(!notified.load(Ordering::Acquire));
        assert_retained_dirty_page(&cached);
        cached.sync(false).unwrap();
    });
}

#[cfg(feature = "vfs")]
#[test]
fn failed_reclaim_invalidation_reserves_the_slot_and_retains_the_frame() {
    with_test_page_provider(true, |_| {
        let (cached, _) = one_page_cache();
        cached.sync(false).unwrap();
        let address = cached
            .shared
            .page_cache
            .lock()
            .peek(&0)
            .unwrap()
            .paddr()
            .unwrap();
        let shared = Arc::downgrade(&cached.shared);
        let notified = Arc::new(AtomicBool::new(false));
        let listener_notified = notified.clone();
        cached.add_evict_listener(move |_, _| {
            let shared = shared.upgrade().unwrap();
            assert!(!shared.io_lock_is_free_for_test());
            assert!(shared.page_cache_lock_is_free_for_test());
            assert!(shared.listener_lock_is_free_for_test());
            listener_notified.store(true, Ordering::Release);
            false
        });

        assert_eq!(cached.shared.try_evict_clean_pages(1), 0);
        assert!(notified.load(Ordering::Acquire));
        let mut pages = cached.shared.page_cache.lock();
        let page = pages.get_mut(&0).unwrap();
        assert_eq!(page.paddr(), Ok(address));
        assert_eq!(&*page.data(), vec![0xa5; PAGE_SIZE]);
        assert!(!page.dirty);
    });
}

#[cfg(feature = "vfs")]
#[test]
fn reclaim_does_not_detach_pages_from_an_in_flight_io_owner() {
    with_test_page_provider(true, |_| {
        let (cached, _) = one_page_cache();
        cached.sync(false).unwrap();
        let owner = cached.shared.io_lock.lock();
        assert_eq!(cached.shared.try_evict_clean_pages(1), 0);
        assert!(cached.shared.page_cache.lock().contains(&0));
        drop(owner);
        assert_eq!(cached.shared.try_evict_clean_pages(1), 1);
        assert!(cached.shared.page_cache.lock().is_empty());
    });
}

#[test]
fn busy_lru_mapping_keeps_its_canonical_frame_and_dirty_data() {
    with_test_page_provider(true, |_| {
        let (cached, backing) = one_page_cache();
        let old = cached
            .shared
            .page_cache
            .lock()
            .peek(&0)
            .unwrap()
            .paddr()
            .unwrap();
        cached.add_evict_listener(|_, _| false);

        cached
            .with_page_or_insert(1, |_, victim| {
                assert!(
                    victim.is_none(),
                    "a still-mapped page cannot leave the cache"
                );
                Ok(())
            })
            .unwrap();

        let mut pages = cached.shared.page_cache.lock();
        assert!(pages.contains(&1));
        let original = pages.get_mut(&0).unwrap();
        assert_eq!(original.paddr(), Ok(old));
        assert!(original.dirty);
        assert_eq!(&*original.data(), vec![0xa5; PAGE_SIZE]);
        assert_eq!(
            &backing.state.lock().unwrap().physical_data[..PAGE_SIZE],
            vec![0; PAGE_SIZE]
        );
        drop(pages);
        cached.sync(false).unwrap();
    });
}

#[test]
fn failed_truncate_invalidation_retains_frame_and_retries_before_regrowth() {
    with_test_page_provider(true, |provider| {
        let (cached, backing) = one_page_cache();
        let rejected = Arc::new(AtomicBool::new(true));
        let reject = rejected.clone();
        let shared = Arc::downgrade(&cached.shared);
        cached.add_evict_listener(move |_, _| {
            let shared = shared.upgrade().unwrap();
            assert!(shared.io_lock_is_free_for_test());
            assert!(shared.page_cache_lock_is_free_for_test());
            assert!(shared.listener_lock_is_free_for_test());
            assert!(shared.mutation_lock.try_lock().is_none());
            !reject.load(Ordering::Acquire)
        });
        let freed = provider.dealloc_count();
        assert_eq!(cached.set_len(0), Err(VfsError::ResourceBusy));
        assert_eq!(cached.len(), 0);
        assert!(cached.shared.page_cache.lock().is_empty());
        assert_eq!(cached.shared.retired_pages.lock().len(), 1);
        assert_eq!(provider.dealloc_count(), freed);
        assert_eq!(backing.state.lock().unwrap().logical_len, 0);

        assert_eq!(
            cached.set_len(PAGE_SIZE as u64),
            Err(VfsError::ResourceBusy)
        );
        assert_eq!(cached.len(), 0);
        rejected.store(false, Ordering::Release);
        cached.set_len(PAGE_SIZE as u64).unwrap();
        assert!(cached.shared.retired_pages.lock().is_empty());
        assert_eq!(provider.dealloc_count(), freed + 1);
    });
}

pub(super) fn one_page_cache() -> (CachedFile, Arc<CacheTestFile>) {
    let backing = Arc::new(CacheTestFile::new(vec![0; 2 * PAGE_SIZE]));
    let cached = reopen_cached_file(backing.clone());
    cached
        .shared
        .page_cache
        .lock()
        .set_reclaim_target(NonZeroUsize::new(1).unwrap());
    cached
        .write_at(vec![0xa5; PAGE_SIZE].as_slice(), 0)
        .unwrap();
    (cached, backing)
}

fn assert_retained_dirty_page(cached: &CachedFile) {
    let mut cache = cached.shared.page_cache.lock();
    assert_eq!(cache.len(), 1);
    let page = cache
        .get_mut(&0)
        .expect("failed eviction lost the dirty page");
    assert!(page.dirty);
    assert_eq!(&*page.data(), vec![0xa5; PAGE_SIZE]);
}
