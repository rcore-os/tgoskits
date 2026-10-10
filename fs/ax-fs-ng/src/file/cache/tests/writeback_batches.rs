use super::*;

#[test]
fn selected_sparse_writeback_preserves_holes_and_clips_the_eof_tail() {
    with_test_page_provider(true, |_| {
        let len = PAGE_SIZE * 6 + 37;
        let backing = Arc::new(CacheTestFile::new(vec![0; len]));
        let cached = reopen_cached_file(backing.clone());
        let bytes = vec![0x5a; len];
        cached.write_at(bytes.as_slice(), 0).unwrap();
        cached.writeback_pages(&[6, 0, 4, 3, 0, u32::MAX]).unwrap();
        let state = backing.state.lock().unwrap();
        assert_eq!(
            state.write_ranges,
            [
                (0, PAGE_SIZE),
                (3 * PAGE_SIZE, 2 * PAGE_SIZE),
                (6 * PAGE_SIZE, 37)
            ]
        );
        for number in 0..7 {
            let start = number * PAGE_SIZE;
            let end = (start + PAGE_SIZE).min(len);
            let expected = if [0, 3, 4, 6].contains(&number) {
                0x5a
            } else {
                0
            };
            assert!(
                state.physical_data[start..end]
                    .iter()
                    .all(|byte| *byte == expected)
            );
        }
        drop(state);
        let mut dirty = cached.dirty_pages_in_range(0, 7).unwrap();
        dirty.sort_unstable();
        assert_eq!(dirty, [1, 2, 5]);
        cached.sync(false).unwrap();
        assert_eq!(backing.state.lock().unwrap().physical_data, bytes);
    });
}

#[test]
fn a_failed_second_batch_leaves_the_unwritten_tail_dirty_for_retry() {
    with_test_page_provider(true, |_| {
        let len = PAGE_SIZE * 258 + 37;
        let backing = Arc::new(CacheTestFile::new(vec![0; len]));
        let cached = reopen_cached_file(backing.clone());
        let bytes = vec![0x5a; len];
        cached.write_at(bytes.as_slice(), 0).unwrap();
        let next_write = backing.clone();
        let reader = cached.clone();
        *backing.before_write.lock().unwrap() = Some(Box::new(move || {
            *next_write.before_write.lock().unwrap() = Some(Box::new(move || {
                assert!(reader.shared.io_lock_is_free_for_test());
                assert!(reader.dirty_pages_in_range(0, 256).unwrap().is_empty());
                Err(VfsError::Io)
            }));
            Ok(())
        }));
        assert_eq!(cached.sync(false), Err(VfsError::Io));
        let mut dirty = cached.dirty_pages_in_range(0, 259).unwrap();
        dirty.sort_unstable();
        assert_eq!(dirty, [256, 257, 258]);
        assert!(cached.shared.writeback_lock.try_lock().is_some());
        assert!(
            cached
                .shared
                .page_cache
                .lock()
                .iter()
                .all(|(_, page)| page.pins == 0)
        );
        cached.sync(false).unwrap();
        assert_eq!(backing.state.lock().unwrap().physical_data, bytes);
        assert!(cached.dirty_pages_in_range(0, 259).unwrap().is_empty());
    });
}
