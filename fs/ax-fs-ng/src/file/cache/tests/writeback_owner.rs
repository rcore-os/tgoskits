use super::*;

#[test]
fn every_writeback_entry_retains_exclusion_across_unlocked_protection_callbacks() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE]));
        let cached = reopen_cached_file(backing);
        let observed = Arc::new(StdMutex::new(Vec::new()));
        let listener_observed = observed.clone();
        let shared = Arc::downgrade(&cached.shared);
        cached.add_page_listener(
            |_, _| true,
            move |_| {
                let shared = shared.upgrade().unwrap();
                listener_observed.lock().unwrap().push((
                    shared.writeback_lock.try_lock().is_none(),
                    shared.io_lock_is_free_for_test(),
                    shared.page_cache_lock_is_free_for_test(),
                    shared.listener_lock_is_free_for_test(),
                ));
                true
            },
        );

        cached.write_at(&[1][..], 0).unwrap();
        cached.writeback().unwrap();
        cached.write_at(&[2][..], 0).unwrap();
        cached.writeback_pages(&[0]).unwrap();
        cached.write_at(&[3][..], 0).unwrap();
        cached.sync(false).unwrap();
        #[cfg(any(feature = "vfs", feature = "ext4"))]
        {
            cached.write_at(&[4][..], 0).unwrap();
            cached.shared.writeback_dirty_for_global_sync().unwrap();
        }
        let expected = if cfg!(any(feature = "vfs", feature = "ext4")) {
            4
        } else {
            3
        };
        assert_eq!(
            *observed.lock().unwrap(),
            vec![(true, true, true, true); expected]
        );
        assert!(cached.shared.writeback_lock.try_lock().is_some());
    });
}

#[test]
fn failed_writeback_releases_owner_without_cleaning_dirty_data() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        cached.write_at(&[7][..], 0).unwrap();
        backing.fail_next_write();
        assert_eq!(cached.sync(false), Err(VfsError::Io));
        assert!(cached.shared.writeback_lock.try_lock().is_some());
        assert_eq!(cached.dirty_pages_in_range(0, 1), [0]);
        cached.sync(false).unwrap();
        assert_eq!(backing.state.lock().unwrap().physical_data[0], 7);
    });
}
