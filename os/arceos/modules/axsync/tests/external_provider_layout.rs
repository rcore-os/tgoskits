use core::mem::{align_of, size_of};

use ax_sync::interface::{PI_MUTEX_WAIT_STORAGE_WORDS, PiMutexStorage};

#[test]
fn pi_mutex_external_storage_carries_the_native_generation_and_inline_waiter_state() {
    let storage = PiMutexStorage::new();

    assert_eq!(
        storage
            .owner_word()
            .load(core::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(
        storage
            .generation()
            .load(core::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(
        storage
            .wait_state()
            .load(core::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(PI_MUTEX_WAIT_STORAGE_WORDS, 5);
    assert_eq!(align_of::<PiMutexStorage>(), align_of::<usize>());
    assert!(size_of::<PiMutexStorage>() >= size_of::<[usize; 8]>());
}
