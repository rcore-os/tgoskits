use ax_sync::{SpinLock, SpinRwLock};

#[test]
fn bridge_spin_lock_round_trip() {
    let lock = SpinLock::new(1usize);
    *lock.lock() += 1;
    assert_eq!(*lock.lock(), 2);
}

#[test]
fn bridge_rwlock_round_trip() {
    let lock = SpinRwLock::new(1usize);
    *lock.write() += 1;
    assert_eq!(*lock.read(), 2);
}
