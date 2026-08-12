use ax_runtime::{Mutex, SpinLock, SpinRwLock};

#[test]
fn host_runtime_executes_every_lock_family_through_ax_task() {
    let spin = SpinLock::new(1usize);
    *spin.lock() += 1;
    assert_eq!(*spin.lock(), 2);

    let rwlock = SpinRwLock::new(3usize);
    *rwlock.write() += 1;
    assert_eq!(*rwlock.read(), 4);

    let mutex = Mutex::new(5usize);
    *mutex.lock() += 1;
    assert_eq!(*mutex.lock(), 6);
}
