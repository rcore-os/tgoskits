use core::{io::BorrowedBuf, mem::MaybeUninit};

use super::*;

#[test]
fn sequential_cache_hits_schedule_the_next_window_without_backing_io() {
    with_test_page_provider(true, |_| {
        crate::os::task::install_test_runtime_ops();
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 64]));
        let cached = reopen_cached_file(backing.clone());
        super::super::prefetch::tests::with_queue(|queue| {
            // Installation may fail before the first sequential request.
            assert_eq!(queue.start(), Err(crate::BlockError::NoMemory));
            for page in 0..3 {
                assert_eq!(
                    read_bytes(&cached, (page * PAGE_SIZE) as u64, PAGE_SIZE),
                    vec![0x5a; PAGE_SIZE]
                );
            }
            // The cache-hit caller only queued work; it did not perform I/O.
            assert_eq!(backing.state.lock().unwrap().read_calls, 1);
            assert!(!cached.is_page_cached(4));
            assert_eq!(queue.spawn_attempts(), 2);
            queue.run_one();
            assert!(cached.is_page_cached(4));
            assert_eq!(backing.state.lock().unwrap().read_calls, 2);
            for page in 3..=20 {
                assert_eq!(
                    read_bytes(&cached, (page * PAGE_SIZE) as u64, PAGE_SIZE),
                    vec![0x5a; PAGE_SIZE]
                );
            }
            // Later windows reuse the running worker and still defer I/O.
            assert_eq!(queue.spawn_attempts(), 2);
            assert_eq!(backing.state.lock().unwrap().read_calls, 2);
            assert!(!cached.is_page_cached(36));
            queue.run_one();
            assert!(cached.is_page_cached(36));
            assert_eq!(
                read_bytes(&cached, 36 * PAGE_SIZE as u64, PAGE_SIZE),
                vec![0x5a; PAGE_SIZE]
            );
            assert_eq!(backing.state.lock().unwrap().read_calls, 3);
        });
    });
}

#[test]
fn speculative_admission_failures_retry_without_another_demand_miss() {
    with_test_page_provider(true, |_| {
        crate::os::task::install_test_runtime_ops();
        super::super::prefetch::tests::with_queue(|queue| {
            let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 64]));
            let cached = reopen_cached_file(backing.clone());
            for page in 0..3 {
                read_bytes(&cached, (page * PAGE_SIZE) as u64, PAGE_SIZE);
            }
            assert_eq!(queue.queued_len(), 0, "first worker startup fails");
            read_bytes(&cached, 3 * PAGE_SIZE as u64, 1);
            assert_eq!(
                queue.queued_len(),
                1,
                "the next resident hit must retry startup"
            );
            backing.fail_next_read.store(true, Ordering::Release);
            queue.run_one();
            assert!(!cached.is_page_cached(4));
            // Repeated partial reads in the last resident page remain sequential.
            read_bytes(&cached, 3 * PAGE_SIZE as u64 + 1, 1);
            assert_eq!(
                queue.queued_len(),
                1,
                "failed backing I/O must re-arm the window"
            );
            queue.run_one();
            assert!(cached.is_page_cached(4));
        });
    });
}

#[test]
fn full_speculative_queue_rearms_the_rejected_window() {
    with_test_page_provider(true, |_| {
        crate::os::task::install_test_runtime_ops();
        super::super::prefetch::tests::with_queue(|queue| {
            assert_eq!(queue.start(), Err(crate::BlockError::NoMemory));
            queue.start().unwrap();
            let mut files = Vec::new();
            for _ in 0..17 {
                let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 64]));
                let cached = reopen_cached_file(backing);
                for page in 0..3 {
                    read_bytes(&cached, (page * PAGE_SIZE) as u64, PAGE_SIZE);
                }
                files.push(cached);
            }
            assert_eq!(queue.queued_len(), 16);
            queue.run_one();
            read_bytes(files.last().unwrap(), 3 * PAGE_SIZE as u64, PAGE_SIZE);
            assert_eq!(
                queue.queued_len(),
                16,
                "queue-full must not consume the rejected trigger"
            );
            while queue.queued_len() != 0 {
                queue.run_one();
            }
            assert!(files.last().unwrap().is_page_cached(4));
        });
    });
}

#[test]
fn speculative_fill_skips_resident_prefix_and_retries_mapping_contention() {
    with_test_page_provider(true, |_| {
        crate::os::task::install_test_runtime_ops();
        super::super::prefetch::tests::with_queue(|queue| {
            assert_eq!(queue.start(), Err(crate::BlockError::NoMemory));
            let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE * 64]));
            let cached = reopen_cached_file(backing.clone());
            let other = CachedFile::get_or_create(cached.location().clone()).unwrap();
            for page in 0..3 {
                read_bytes(&cached, (page * PAGE_SIZE) as u64, PAGE_SIZE);
            }
            cached
                .shared
                .mapping_update_in_progress
                .store(true, Ordering::Release);
            queue.run_one();
            assert!(!cached.is_page_cached(5));
            cached
                .shared
                .mapping_update_in_progress
                .store(false, Ordering::Release);
            read_bytes(&cached, 3 * PAGE_SIZE as u64, 1);
            assert_eq!(
                queue.queued_len(),
                1,
                "mapping contention must preserve the trigger"
            );
            // Execute speculation while the other handle owns the first page's
            // backing read. The worker must defer rather than wait or consume it.
            let executor = *queue;
            *backing.after_read.lock().unwrap() = Some(Box::new(move || executor.run_one()));
            assert_eq!(read_bytes(&other, 4 * PAGE_SIZE as u64, 1), vec![0x5a]);
            assert!(!cached.is_page_cached(5));
            read_bytes(&cached, 3 * PAGE_SIZE as u64 + 1, 1);
            assert_eq!(
                queue.queued_len(),
                1,
                "fill contention must preserve the trigger"
            );
            queue.run_one();
            assert!(
                cached.is_page_cached(5),
                "a resident prefix must not suppress the missing tail"
            );
            assert_eq!(backing.state.lock().unwrap().read_calls, 3);
        });
    });
}

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
