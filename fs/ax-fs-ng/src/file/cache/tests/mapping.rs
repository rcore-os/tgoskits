use core::io::BorrowedBuf;

use super::*;

#[test]
fn complete_read_backing_requires_current_cache_identity_and_eof() {
    with_test_page_provider(true, |_| {
        let backing = Arc::new(CacheTestFile::new(vec![0x41; PAGE_SIZE + 9]));
        let cached = reopen_cached_file(backing);
        let pin = cached.pin_read_page(0).unwrap().unwrap();
        assert!(cached.pin_read_page(1).unwrap().is_none());
        let old = pin.backing();
        let epoch = cached.mapping_epoch();
        assert_eq!(
            cached.with_current_read_backing(0, epoch, &old, || Ok::<_, VfsError>(42)),
            Ok(Some(42))
        );
        // Physical retention is separate from a transient publication pin.
        // Truncate must first reject that publication, then invalidate it once
        // the transient pin is released without freeing retained source bytes.
        assert_eq!(cached.set_len(0), Err(VfsError::ResourceBusy));
        drop(pin);
        cached.set_len(0).unwrap();
        assert_eq!(
            cached.with_current_read_backing(0, epoch, &old, || -> VfsResult<()> {
                panic!("stale backing published")
            }),
            Ok(None)
        );
        let mut bytes = [0; PAGE_SIZE];
        old.copy_to(BorrowedBuf::from(&mut bytes[..]).unfilled())
            .unwrap();
        assert_eq!(bytes, [0x41; PAGE_SIZE]);
        cached.write_at(&[0x72; PAGE_SIZE][..], 0).unwrap();
        let new = cached.pin_read_page(0).unwrap().unwrap();
        let current_epoch = cached.mapping_epoch();
        assert_eq!(
            cached.with_current_read_backing(0, current_epoch, &old, || -> VfsResult<()> {
                panic!("replaced backing published")
            }),
            Ok(None)
        );
        assert_eq!(
            cached.with_current_read_backing(
                0,
                current_epoch,
                &new.backing(),
                || Ok::<_, VfsError>(())
            ),
            Ok(Some(()))
        );
        drop(new);
        cached.sync(false).unwrap();
    });
}
