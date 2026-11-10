use super::*;

#[derive(Clone, Copy, Debug)]
enum WritebackEntry {
    All,
    Selected,
    Sync,
    #[cfg(any(feature = "ext4", feature = "vfs"))]
    Global,
    #[cfg(any(feature = "ext4", feature = "vfs"))]
    Periodic,
}

fn entries() -> Vec<WritebackEntry> {
    vec![
        WritebackEntry::All,
        WritebackEntry::Selected,
        WritebackEntry::Sync,
        #[cfg(any(feature = "ext4", feature = "vfs"))]
        WritebackEntry::Global,
        #[cfg(any(feature = "ext4", feature = "vfs"))]
        WritebackEntry::Periodic,
    ]
}

fn run_entry(cached: &CachedFile, entry: WritebackEntry) -> VfsResult<()> {
    match entry {
        WritebackEntry::All => cached.writeback().map(|_| ()),
        WritebackEntry::Selected => cached.writeback_pages(&[0]),
        WritebackEntry::Sync => cached.sync(false),
        #[cfg(any(feature = "ext4", feature = "vfs"))]
        WritebackEntry::Global => cached.shared.writeback_dirty_for_global_sync(),
        #[cfg(any(feature = "ext4", feature = "vfs"))]
        WritebackEntry::Periodic => cached.shared.writeback_dirty_for_periodic(),
    }
}

fn assert_read_progress(cached: &CachedFile) {
    assert!(cached.shared.io_lock_is_free_for_test());
    assert!(cached.shared.page_cache_lock_is_free_for_test());
    assert!(cached.shared.writeback_lock.try_lock().is_none());
    let pin = cached.pin_cached_page(0).unwrap();
    assert_ne!(pin.paddr(), 0);
    drop(pin);
    let mut bytes = [0; 1];
    assert_eq!(cached.read_at(&mut bytes[..], 0).unwrap(), 1);
    assert_eq!(bytes, [0x41]);
}

#[test]
fn writeback_entries_retain_the_round_and_release_io_during_backing_writes() {
    with_test_page_provider(true, |_| {
        for entry in entries() {
            let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE]));
            let cached = reopen_cached_file(backing.clone());
            cached.write_at(&[0x41][..], 0).unwrap();
            let reader = cached.clone();
            let observed = Arc::new(AtomicBool::new(false));
            let observation = observed.clone();
            *backing.before_write.lock().unwrap() = Some(Box::new(move || {
                assert_read_progress(&reader);
                observation.store(true, Ordering::Release);
                Ok(())
            }));
            run_entry(&cached, entry).unwrap();
            assert!(observed.load(Ordering::Acquire));
            assert_eq!(backing.state.lock().unwrap().physical_data[0], 0x41);
            assert!(cached.dirty_pages_in_range(0, 1).unwrap().is_empty());
            assert!(cached.shared.writeback_lock.try_lock().is_some());
            assert_eq!(cached.shared.page_cache.lock().get_mut(&0).unwrap().pins, 0);
        }
    });
}

#[test]
fn explicit_empty_sync_releases_io_and_failed_sync_releases_the_round() {
    with_test_page_provider(true, |_| {
        for entry in [
            WritebackEntry::All,
            WritebackEntry::Selected,
            WritebackEntry::Sync,
        ] {
            let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE]));
            let cached = reopen_cached_file(backing.clone());
            cached.write_at(&[0x41][..], 0).unwrap();
            cached.sync(false).unwrap();
            let reader = cached.clone();
            *backing.before_sync.lock().unwrap() = Some(Box::new(move || {
                assert_read_progress(&reader);
                Err(VfsError::Io)
            }));
            assert_eq!(run_entry(&cached, entry), Err(VfsError::Io));
            assert!(cached.shared.writeback_lock.try_lock().is_some());
            cached.sync(false).unwrap();
        }

        static DISK_FILESYSTEM: CacheTestFilesystem = CacheTestFilesystem { name: "vfat" };
        for entry in entries() {
            let backing = Arc::new(CacheTestFile::new_on(vec![0; PAGE_SIZE], &DISK_FILESYSTEM));
            let cached = reopen_cached_file(backing.clone());
            cached.write_at(&[0x41][..], 0).unwrap();
            let owner = Arc::downgrade(&cached.shared);
            let editor = Arc::downgrade(&backing);
            let sync_attempted = Arc::new(AtomicBool::new(false));
            let attempted = sync_attempted.clone();
            *backing.before_sync.lock().unwrap() = Some(Box::new(move || {
                attempted.store(true, Ordering::Release);
                Err(VfsError::Io)
            }));
            let result = run_entry(&cached, entry);
            assert_eq!(
                result,
                if sync_attempted.load(Ordering::Acquire) {
                    Err(VfsError::Io)
                } else {
                    // Registered writeback may leave the editor pending after
                    // writing the pages; it must still retain its real owner.
                    Ok(())
                }
            );
            assert!(cached.dirty_pages_in_range(0, 1).unwrap().is_empty());
            drop(cached);
            drop(backing);
            super::super::reclaim::prune_cached_files();

            let retained = owner.upgrade().unwrap_or_else(|| {
                panic!("{entry:?}: failed or pending editor lost its cache owner")
            });
            let backing = editor
                .upgrade()
                .expect("the cache must retain its actual backing");
            let reopened = reopen_cached_file(backing);
            assert!(Arc::ptr_eq(&retained, &reopened.shared));
            drop(retained);
            let reopened = if !sync_attempted.load(Ordering::Acquire) {
                assert_eq!(reopened.sync(false), Err(VfsError::Io));
                drop(reopened);
                super::super::reclaim::prune_cached_files();
                let backing = editor
                    .upgrade()
                    .expect("failed editor must remain retryable");
                let reopened = reopen_cached_file(backing);
                assert!(
                    owner
                        .upgrade()
                        .is_some_and(|owner| Arc::ptr_eq(&owner, &reopened.shared))
                );
                reopened
            } else {
                reopened
            };
            reopened.sync(false).unwrap();
            let mut byte = [0];
            reopened.read_at(&mut byte[..], 0).unwrap();
            assert_eq!(byte, [0x41]);
            drop(reopened);
            super::super::reclaim::prune_cached_files();
            assert!(owner.upgrade().is_none());
            assert!(editor.upgrade().is_none());
        }
    });
}

#[test]
fn failed_backing_write_after_redirty_preserves_current_contents_for_retry() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        cached.write_at(&[0x41][..], 0).unwrap();
        let writer = cached.clone();
        *backing.before_write.lock().unwrap() = Some(Box::new(move || {
            assert_read_progress(&writer);
            writer.write_at(&[0x72][..], 0)?;
            Err(VfsError::Io)
        }));
        assert_eq!(cached.sync(false), Err(VfsError::Io));
        assert!(cached.shared.writeback_lock.try_lock().is_some());
        assert_eq!(cached.dirty_pages_in_range(0, 1).unwrap(), [0]);
        assert_eq!(cached.shared.page_cache.lock().get_mut(&0).unwrap().pins, 0);
        cached.sync(false).unwrap();
        assert_eq!(backing.state.lock().unwrap().physical_data[0], 0x72);
    });
}

#[test]
fn writeback_protection_retains_the_round_without_io_index_or_endpoint_locks() {
    with_test_page_provider(true, |_| {
        for entry in entries() {
            let backing = Arc::new(CacheTestFile::new(vec![0; PAGE_SIZE]));
            let cached = reopen_cached_file(backing);
            cached.write_at(&[0x41][..], 0).unwrap();
            let shared = Arc::downgrade(&cached.shared);
            let observed = Arc::new(AtomicBool::new(false));
            let observation = observed.clone();
            let endpoint = test_mapping_endpoint(move |event| match event {
                CacheMappingEvent::WritebackProtect(_) => {
                    let shared = shared.upgrade().unwrap();
                    assert!(shared.writeback_lock.try_lock().is_none());
                    assert!(shared.io_lock_is_free_for_test());
                    assert!(shared.page_cache_lock_is_free_for_test());
                    assert!(shared.endpoint_lock_is_free_for_test());
                    observation.store(true, Ordering::Release);
                    CacheMappingResult::Protected
                }
                CacheMappingEvent::Evict(_) => CacheMappingResult::Retired,
            });
            cached.install_mapping_endpoint(&endpoint).unwrap();
            run_entry(&cached, entry).unwrap();
            assert!(observed.load(Ordering::Acquire));
            assert!(cached.shared.writeback_lock.try_lock().is_some());
        }
    });
}
