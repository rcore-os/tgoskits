use core::{io::BorrowedBuf, mem::MaybeUninit};

use super::*;

#[test]
fn missing_page_read_releases_both_owners_for_an_independent_miss() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 12]));
        let cached = reopen_cached_file(backing.clone());
        let other = cached.clone();
        *backing.after_read.lock().unwrap() = Some(Box::new(move || {
            assert!(other.shared.io_lock_is_free_for_test());
            assert!(other.shared.page_cache_lock_is_free_for_test());
            assert_eq!(read_bytes(&other, PAGE_SIZE as u64 * 8, 1), vec![0x5a]);
        }));
        assert_eq!(read_bytes(&cached, 0, 1), vec![0x5a]);
        assert_eq!(backing.state.lock().unwrap().read_calls, 2);
    });
}

#[test]
fn captured_fill_cannot_overwrite_a_concurrent_cached_write() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x11; PAGE_SIZE * 2]));
        let cached = reopen_cached_file(backing.clone());
        let writer = cached.clone();
        *backing.after_read.lock().unwrap() = Some(Box::new(move || {
            assert!(writer.shared.io_lock_is_free_for_test());
            writer
                .write_at(vec![0x99; PAGE_SIZE].as_slice(), 0)
                .unwrap();
        }));
        assert_eq!(read_bytes(&cached, 0, PAGE_SIZE), vec![0x99; PAGE_SIZE]);
        assert!(cached.shared.page_cache.lock().get_mut(&0).unwrap().dirty);
        cached.sync(false).unwrap();
    });
}

#[test]
fn captured_fill_cannot_resurrect_pages_after_truncate_and_regrowth() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x11; PAGE_SIZE * 3]));
        let cached = reopen_cached_file(backing.clone());
        let writer = cached.clone();
        *backing.after_read.lock().unwrap() = Some(Box::new(move || {
            assert!(writer.shared.io_lock_is_free_for_test());
            writer.set_len(0).unwrap();
            writer
                .write_at(vec![0x77; PAGE_SIZE * 3].as_slice(), 0)
                .unwrap();
        }));
        assert_eq!(
            read_bytes(&cached, 0, PAGE_SIZE * 3),
            vec![0x77; PAGE_SIZE * 3]
        );
        cached.sync(false).unwrap();
    });
}

#[test]
fn concurrent_truncate_ends_read_without_publishing_old_tail() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x11; PAGE_SIZE * 2]));
        let cached = reopen_cached_file(backing.clone());
        let writer = cached.clone();
        *backing.after_read.lock().unwrap() = Some(Box::new(move || {
            assert!(writer.shared.io_lock_is_free_for_test());
            writer.set_len(0).unwrap();
        }));
        assert!(read_bytes(&cached, 0, PAGE_SIZE * 2).is_empty());
        assert!(cached.shared.page_cache.lock().is_empty());
    });
}

#[test]
fn replacement_fill_reads_without_holding_cache_or_endpoint_index() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 2]));
        let cached = reopen_cached_file(backing.clone());
        cached
            .shared
            .page_cache
            .lock()
            .set_reclaim_target(core::num::NonZeroUsize::new(1).unwrap());
        let shared = Arc::downgrade(&cached.shared);
        *backing.after_read.lock().unwrap() = Some(Box::new(move || {
            let shared = shared.upgrade().unwrap();
            assert!(shared.page_cache_lock_is_free_for_test());
            assert!(shared.endpoint_lock_is_free_for_test());
            assert!(shared.io_lock_is_free_for_test());
        }));
        assert_eq!(read_bytes(&cached, 0, 1), vec![0x5a]);
        assert!(cached.is_page_cached(0));
        assert_eq!(cached.shared.page_cache.lock().len(), 1);
    });
}

#[test]
fn buffered_reads_reclaim_clean_pages_with_live_mapping_endpoint() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 8]));
        let cached = reopen_cached_file(backing);
        cached
            .shared
            .page_cache
            .lock()
            .set_reclaim_target(core::num::NonZeroUsize::new(2).unwrap());
        let evictions = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&evictions);
        let _endpoint = install_shared_test_endpoint(&cached.shared, move |event| match event {
            CacheMappingEvent::Evict(page) if page.page_number() == 0 => CacheMappingResult::Busy,
            CacheMappingEvent::Evict(_) => {
                observed.fetch_add(1, Ordering::Relaxed);
                CacheMappingResult::Retired
            }
            CacheMappingEvent::WritebackProtect(_) => CacheMappingResult::Protected,
        });

        for page in 0..8 {
            assert_eq!(read_bytes(&cached, (page * PAGE_SIZE) as u64, 1), [0x5a]);
        }

        assert!(evictions.load(Ordering::Relaxed) > 0);
        assert!(cached.is_page_cached(0));
        assert!(cached.shared.page_cache.lock().len() <= 2);
    });
}

#[test]
fn mapped_cache_trim_preserves_unrelated_inflight_fill() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 129]));
        let cached = reopen_cached_file(backing.clone());
        assert_eq!(read_bytes(&cached, 0, 1), [0x5a]);
        cached
            .shared
            .page_cache
            .lock()
            .set_reclaim_target(NonZeroUsize::new(1).unwrap());
        let _endpoint = install_shared_test_endpoint(&cached.shared, |event| match event {
            CacheMappingEvent::Evict(page) if page.page_number() == 0 => CacheMappingResult::Busy,
            CacheMappingEvent::Evict(_) => CacheMappingResult::Retired,
            CacheMappingEvent::WritebackProtect(_) => CacheMappingResult::Protected,
        });
        let other = cached.clone();
        *backing.after_read.lock().unwrap() = Some(Box::new(move || {
            assert_eq!(read_bytes(&other, PAGE_SIZE as u64 * 128, 1), [0x5a]);
        }));

        cached
            .populate_page_window(cached.inner.entry().as_file().unwrap(), 64, 1)
            .unwrap();
        assert!(cached.is_page_cached(64));
        assert_eq!(backing.state.lock().unwrap().read_calls, 3);
    });
}

#[test]
fn overlapping_misses_wait_for_one_backing_read() {
    check_coalesced_fill(false);
}

#[test]
fn failed_fill_wakes_all_waiters_and_releases_its_slot_for_retry() {
    check_coalesced_fill(true);
}

#[test]
fn partial_preparation_oom_releases_every_unpublished_page() {
    with_test_page_provider(true, |provider| {
        let backing = Arc::new(CacheTestFile::new(vec![0x33; PAGE_SIZE * 4]));
        let cached = reopen_cached_file(backing);
        provider.fail_after(1);
        let mut bytes = [MaybeUninit::uninit(); 1];
        let mut dst = BorrowedBuf::from(&mut bytes[..]);
        assert_eq!(
            cached.read_buf_at(dst.unfilled(), 0),
            Err(VfsError::NoMemory)
        );
        assert_eq!(dst.len(), 0);
        assert_eq!(provider.alloc_count(), 1);
        assert_eq!(provider.dealloc_count(), 1);
        assert!(cached.shared.page_cache.lock().is_empty());
        assert!(cached.shared.pending_fills.is_empty());
        provider.fail_after(usize::MAX);
        assert_eq!(read_bytes(&cached, 0, 1), vec![0x33]);
    });
}

#[test]
fn readahead_smaller_cache_keeps_the_demand_page_resident() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x33; PAGE_SIZE * 4]));
        let cached = reopen_cached_file(backing);
        cached
            .shared
            .page_cache
            .lock()
            .set_reclaim_target(NonZeroUsize::new(1).unwrap());
        // Inspect publication directly: the demand must survive its own run,
        // independently of a timeout or a repeatedly faulting read loop.
        cached
            .populate_page_window(cached.inner.entry().as_file().unwrap(), 0, 4)
            .unwrap();
        assert!(cached.shared.page_cache.lock().contains(&0));
        assert_eq!(read_bytes(&cached, 0, 1), vec![0x33]);
    });
}

fn check_coalesced_fill(fail: bool) {
    with_test_page_provider(true, |provider| {
        crate::os::task::install_test_runtime_ops();
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 4]));
        let cached = reopen_cached_file(backing.clone());
        let (started, observed) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        *backing.after_read.lock().unwrap() = Some(Box::new(move || {
            started.send(()).unwrap();
            resume.recv_timeout(Duration::from_secs(5)).unwrap();
        }));
        let first = cached.clone();
        let first = std::thread::spawn(move || read_one(&first, 0));
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let second = cached.clone();
        let second = std::thread::spawn(move || read_one(&second, PAGE_SIZE as u64));
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while cached.shared.pending_fills.waiter_count() == 0
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        let joined_pending = cached.shared.pending_fills.waiter_count() == 1;
        if fail {
            provider.fail_after(0);
        }
        // Always release and join both threads before asserting the rendezvous.
        release.send(()).unwrap();
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert!(
            joined_pending,
            "second miss never attached to the captured read"
        );
        let expected = if fail {
            Err(VfsError::NoMemory)
        } else {
            Ok(0x5a)
        };
        assert_eq!(first, expected);
        assert_eq!(second, expected);
        assert_eq!(backing.state.lock().unwrap().read_calls, 1);
        assert!(cached.shared.pending_fills.is_empty());
        if fail {
            assert!(cached.shared.page_cache.lock().is_empty());
            provider.fail_after(usize::MAX);
            assert_eq!(read_one(&cached, 0), Ok(0x5a));
        }
    });
}

fn read_one(cached: &CachedFile, offset: u64) -> VfsResult<u8> {
    let mut bytes = [MaybeUninit::uninit(); 1];
    let mut dst = BorrowedBuf::from(&mut bytes[..]);
    let count = cached.read_buf_at(dst.unfilled(), offset)?;
    assert_eq!(count, 1);
    Ok(dst.filled()[0])
}

fn read_bytes(cached: &CachedFile, offset: u64, length: usize) -> Vec<u8> {
    let mut bytes = vec![MaybeUninit::uninit(); length];
    let mut dst = BorrowedBuf::from(bytes.as_mut_slice());
    let count = cached.read_buf_at(dst.unfilled(), offset).unwrap();
    assert_eq!(count, dst.len());
    dst.filled().to_vec()
}
