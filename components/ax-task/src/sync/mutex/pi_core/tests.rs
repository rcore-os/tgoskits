use super::*;

#[test]
fn borrowed_core_view_keeps_external_fields_authoritative() {
    let owner = AtomicU64::new(0);
    let generation = AtomicU64::new(0);
    let wait_state = AtomicU8::new(WAIT_STORAGE_UNINITIALIZED);
    let wait_words = UnsafeCell::new([MaybeUninit::uninit(); PI_MUTEX_WAIT_STORAGE_WORDS]);
    let core = PiMutexCoreView::from_parts(&owner, &generation, &wait_state, &wait_words);
    let task = PiTaskId::new(7).unwrap();

    assert_eq!(core.try_acquire(task), Ok(PiMutexAcquire::Acquired));
    assert_eq!(owner.load(Ordering::Relaxed), task.get());
    let lock = core.mutex_ref().unwrap();
    let recovered = unsafe {
        // SAFETY: `raw` remains bounded by all local backing fields.
        lock.raw().core()
    };
    assert_eq!(recovered.mutex_ref().unwrap().id(), lock.id());
    assert!(recovered.is_owned_by(task));
    assert!(
        unsafe {
            // SAFETY: this test established `task` as the physical owner.
            recovered.try_release_for_thread(task)
        }
        .unwrap()
    );
    assert_eq!(owner.load(Ordering::Relaxed), 0);

    let waiter = unsafe {
        // SAFETY: this local storage is used only with `u64`, which fits
        // the published inline size and alignment.
        core.wait_storage().get_or_init(|| 0x5a5a_u64)
    };
    assert_eq!(*waiter, 0x5a5a);
    assert!(core.wait_storage().is_initialized());
}
