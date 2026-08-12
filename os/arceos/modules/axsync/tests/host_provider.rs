use ax_sync::{SpinLock, SpinRwLock};

#[test]
fn host_test_feature_closes_the_lock_provider_boundary() {
    let lock = SpinLock::new(1usize);
    *lock.lock() += 1;
    assert_eq!(*lock.lock(), 2);

    let rwlock = SpinRwLock::new(3usize);
    *rwlock.write() += 4;
    assert_eq!(*rwlock.read(), 7);
}
