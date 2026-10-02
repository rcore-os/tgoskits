use core::{io::BorrowedBuf, mem::MaybeUninit};

use super::*;
use crate::file::FileBackend;

#[test]
fn resident_cache_hit_needs_no_scratch_page_or_backing_read() {
    with_test_page_provider(true, |provider| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        let mut warm = [0; 1];
        cached.read_at(&mut warm[..], 0).unwrap();
        let allocations = provider.alloc_count();
        let reads = backing.state.lock().unwrap().read_calls;

        let mut bytes = [MaybeUninit::uninit(); PAGE_SIZE];
        let mut dst = BorrowedBuf::from(&mut bytes[..]);
        let backend = FileBackend::Cached(cached);
        assert_eq!(backend.read_buf_at(dst.unfilled(), 0), Ok(PAGE_SIZE));
        assert_eq!(dst.filled(), &[0x5a; PAGE_SIZE]);
        assert_eq!(backing.state.lock().unwrap().read_calls, reads);
        assert_eq!(
            provider.alloc_count(),
            allocations,
            "a resident cache hit allocated an unnecessary intermediate page",
        );
    });
}

#[test]
fn resident_read_preserves_prefix_and_clips_cross_page_eof() {
    with_test_page_provider(true, |_| {
        let contents = (0..PAGE_SIZE + 19)
            .map(|index| index as u8)
            .collect::<Vec<_>>();
        let backing = Arc::new(CacheTestFile::new(contents.clone()));
        let cached = reopen_cached_file(backing);
        let offset = PAGE_SIZE - 7;
        let mut bytes = [MaybeUninit::uninit(); 48];
        let mut dst = BorrowedBuf::from(&mut bytes[..]);
        dst.unfilled().append(b"prefix");

        assert_eq!(cached.read_buf_at(dst.unfilled(), offset as u64), Ok(26));
        assert_eq!(&dst.filled()[..6], b"prefix");
        assert_eq!(&dst.filled()[6..], &contents[offset..]);
        assert_eq!(
            cached.read_buf_at(dst.unfilled(), contents.len() as u64),
            Ok(0)
        );
        assert_eq!(dst.len(), 32);
    });
}

#[test]
fn resident_read_crosses_multiple_pages_without_skipping_bytes() {
    with_test_page_provider(true, |_| {
        let contents = (0..PAGE_SIZE * 3 + 13)
            .map(|index| (index / 31) as u8)
            .collect::<Vec<_>>();
        let backing = Arc::new(CacheTestFile::new(contents.clone()));
        let cached = reopen_cached_file(backing);
        let mut bytes = vec![MaybeUninit::uninit(); PAGE_SIZE * 2 + 5];
        let mut dst = BorrowedBuf::from(bytes.as_mut_slice());
        assert_eq!(cached.read_buf_at(dst.unfilled(), 7), Ok(PAGE_SIZE * 2 + 5));
        assert_eq!(dst.filled(), &contents[7..PAGE_SIZE * 2 + 12]);
    });
}

#[test]
fn empty_and_eof_reads_do_not_allocate_or_touch_storage() {
    with_test_page_provider(true, |provider| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; 8]));
        let cached = reopen_cached_file(backing.clone());
        let mut bytes = [MaybeUninit::uninit(); 8];
        let mut empty = BorrowedBuf::from(&mut bytes[..0]);
        assert_eq!(cached.read_buf_at(empty.unfilled(), 0), Ok(0));
        let mut dst = BorrowedBuf::from(&mut bytes[..]);
        assert_eq!(cached.read_buf_at(dst.unfilled(), 8), Ok(0));
        assert_eq!(cached.read_buf_at(dst.unfilled(), u64::MAX), Ok(0));
        let mut generic = [0; 8];
        assert_eq!(cached.read_at(&mut generic[..], 8), Ok(0));
        assert_eq!(provider.alloc_count(), 0);
        assert_eq!(backing.state.lock().unwrap().read_calls, 0);
        assert_eq!(dst.len(), 0);
    });
}

#[test]
fn resident_cache_hit_does_not_take_unrelated_io_owner() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE]));
        let cached = reopen_cached_file(backing);
        let mut warm = [0; 1];
        cached.read_at(&mut warm[..], 0).unwrap();
        let io = cached.shared.io_lock.lock();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader_file = cached.clone();
        let reader = std::thread::spawn(move || {
            let mut bytes = [MaybeUninit::uninit(); 16];
            let mut dst = BorrowedBuf::from(&mut bytes[..]);
            let result = reader_file.read_buf_at(dst.unfilled(), 32);
            sender.send((result, dst.filled().to_vec())).unwrap();
        });
        let result = receiver.recv_timeout(Duration::from_secs(1));
        drop(io);
        reader.join().unwrap();
        let (result, bytes) = result.expect("resident hit waited for unrelated backing I/O");
        assert_eq!(result, Ok(16));
        assert_eq!(bytes, [0x5a; 16]);
    });
}

#[test]
fn backing_failure_leaves_cursor_prefix_and_no_new_cached_page() {
    with_test_page_provider(true, |provider| {
        let backing = Arc::new(CacheTestFile::new(vec![0x5a; PAGE_SIZE]));
        let cached = reopen_cached_file(backing.clone());
        backing.fail_next_read.store(true, Ordering::Release);
        let mut bytes = [MaybeUninit::uninit(); 16];
        let mut dst = BorrowedBuf::from(&mut bytes[..]);
        dst.unfilled().append(b"prefix");
        assert_eq!(cached.read_buf_at(dst.unfilled(), 0), Err(VfsError::Io));
        assert_eq!(dst.filled(), b"prefix");
        assert_eq!(provider.alloc_count(), 0);
        assert!(cached.shared.page_cache.lock().is_empty());
        assert!(cached.shared.io_lock_is_free_for_test());
    });
}

#[test]
fn direct_backend_cursor_handles_short_reads_and_eof_incrementally() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(b"contents".to_vec()));
        backing.state.lock().unwrap().max_read = 2;
        let cached = reopen_cached_file(backing.clone());
        let backend = FileBackend::Direct(cached.location().clone());
        let mut bytes = [MaybeUninit::uninit(); 20];
        let mut dst = BorrowedBuf::from(&mut bytes[..]);
        dst.unfilled().append(b"prefix");
        assert_eq!(backend.read_buf_at(dst.unfilled(), 1), Ok(7));
        assert_eq!(dst.filled(), b"prefixontents");
        assert_eq!(backing.state.lock().unwrap().read_calls, 5);
        assert_eq!(backend.read_buf_at(dst.unfilled(), 8), Ok(0));
        assert_eq!(dst.filled(), b"prefixontents");
        backing.fail_next_read.store(true, Ordering::Release);
        assert_eq!(backend.read_buf_at(dst.unfilled(), 0), Err(VfsError::Io));
        assert_eq!(dst.filled(), b"prefixontents");
    });
}

#[test]
fn short_user_writer_can_reenter_cache_without_locks_or_lost_bytes() {
    with_test_page_provider(true, |_| {
        let contents = (0..PAGE_SIZE + 23)
            .map(|index| index as u8)
            .collect::<Vec<_>>();
        let backing = Arc::new(CacheTestFile::new(contents.clone()));
        let cached = reopen_cached_file(backing);
        let mut writer = ReentrantWriter {
            cached: &cached,
            received: Vec::new(),
            capacity: contents.len(),
        };
        assert_eq!(cached.read_at(&mut writer, 0), Ok(contents.len()));
        assert_eq!(writer.received, contents);
    });
}

struct ReentrantWriter<'a> {
    cached: &'a CachedFile,
    received: Vec<u8>,
    capacity: usize,
}

impl Write for ReentrantWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> ax_io::Result<usize> {
        assert!(self.cached.shared.io_lock_is_free_for_test());
        assert!(self.cached.shared.page_cache_lock_is_free_for_test());
        assert!(self.cached.shared.endpoint_lock_is_free_for_test());
        let mut probe = [MaybeUninit::uninit(); 1];
        let mut dst = BorrowedBuf::from(&mut probe[..]);
        assert_eq!(self.cached.read_buf_at(dst.unfilled(), 0), Ok(1));
        let accepted = bytes.len().min(3);
        self.received.extend_from_slice(&bytes[..accepted]);
        Ok(accepted)
    }

    fn flush(&mut self) -> ax_io::Result<()> {
        Ok(())
    }
}

impl IoBufMut for ReentrantWriter<'_> {
    fn remaining_mut(&self) -> usize {
        self.capacity - self.received.len()
    }
}
