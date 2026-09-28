use super::*;

#[test]
fn growing_writes_retain_dirty_pages_until_explicit_writeback() {
    assert_growing_file_retains_pages(|cached, bytes, offset| {
        cached.write_at(bytes, offset).unwrap()
    });
}

#[test]
fn growing_appends_retain_dirty_pages_until_explicit_writeback() {
    assert_growing_file_retains_pages(|cached, bytes, offset| {
        let (written, end) = cached.append(bytes).unwrap();
        assert_eq!(end, offset + written as u64);
        written
    });
}

#[test]
fn explicit_resize_updates_bounded_target_without_allocating_pages() {
    with_test_page_provider(true, |provider| {
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        let cached = reopen_cached_file(backing.clone());
        let large_len = MAX_DISK_PAGE_CACHE_BYTES + PAGE_SIZE as u64;

        cached.set_len(large_len).unwrap();
        assert_eq!(cached.len(), large_len);
        assert_eq!(
            cached.shared.page_cache.lock().cap().get(),
            (MAX_DISK_PAGE_CACHE_BYTES / PAGE_SIZE as u64) as usize,
        );
        assert_eq!(provider.alloc_count(), 0);

        cached.set_len(0).unwrap();
        assert_eq!(cached.len(), 0);
        assert_eq!(
            cached.shared.page_cache.lock().cap().get(),
            MIN_DISK_PAGE_CACHE_PAGES,
        );
        assert_eq!(provider.alloc_count(), 0);
        assert_eq!(backing.state.lock().unwrap().write_calls, 0);
    });
}

#[test]
fn failed_growth_keeps_previous_length_and_retention_target() {
    with_test_page_provider(true, |provider| {
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        let cached = reopen_cached_file(backing.clone());
        backing.fail_next_set_len();

        assert_eq!(cached.set_len(MAX_DISK_PAGE_CACHE_BYTES), Err(VfsError::Io));
        assert_eq!(cached.len(), 0);
        assert_eq!(backing.state.lock().unwrap().logical_len, 0);
        assert_eq!(
            cached.shared.page_cache.lock().cap().get(),
            MIN_DISK_PAGE_CACHE_PAGES,
        );
        assert_eq!(provider.alloc_count(), 0);
    });
}

#[test]
fn memory_file_growth_and_truncate_keep_unbounded_retention() {
    with_test_page_provider(true, |provider| {
        let shared = CachedFileShared::new_unbounded(0);
        shared.update_len_max(MAX_DISK_PAGE_CACHE_BYTES);
        assert_eq!(shared.page_cache.lock().cap(), NonZeroUsize::MAX);
        shared.set_len(0);
        assert_eq!(shared.len(), 0);
        assert_eq!(shared.page_cache.lock().cap(), NonZeroUsize::MAX);
        assert_eq!(provider.alloc_count(), 0);
    });
}

fn assert_growing_file_retains_pages(write: impl Fn(&CachedFile, &[u8], u64) -> usize) {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(Vec::new()));
        let cached = reopen_cached_file(backing.clone());
        let page_count = MIN_DISK_PAGE_CACHE_PAGES + 1;
        let mut contents = Vec::new();
        for page_number in 0..page_count {
            let bytes = vec![(page_number % 251) as u8; PAGE_SIZE];
            assert_eq!(write(&cached, &bytes, contents.len() as u64), PAGE_SIZE);
            contents.extend_from_slice(&bytes);
        }

        assert_eq!(
            backing.state.lock().unwrap().write_calls,
            0,
            "growth within the disk retention bound must not force dirty eviction",
        );
        assert_eq!(
            cached.dirty_pages_in_range(0, page_count as u32).len(),
            page_count
        );
        let mut read = vec![0; contents.len()];
        assert_eq!(
            cached.read_at(read.as_mut_slice(), 0).unwrap(),
            contents.len()
        );
        assert_eq!(read, contents);
        assert_eq!(backing.state.lock().unwrap().read_calls, 0);

        cached.writeback().unwrap();
        assert_eq!(backing.state.lock().unwrap().physical_data, contents);
        assert!(cached.dirty_pages_in_range(0, page_count as u32).is_empty());
    });
}
