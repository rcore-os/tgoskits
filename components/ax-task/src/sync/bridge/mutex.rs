//! Complete external PI-mutex transactions backed by native scheduler state.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    panic::Location,
    sync::atomic::{AtomicU8, AtomicU64},
};

use super::lockdep::LockClass;
use crate::sync::mutex::{
    PI_MUTEX_WAIT_STORAGE_WORDS, PiMutexAlgorithm, PiMutexCoreView, destroy_pi_mutex_storage,
};
#[cfg(feature = "lockdep")]
use crate::sync::{
    lockdep::LockdepMapView,
    mutex::lockdep::{LockdepAcquire, LockdepAcquireRequest},
};

/// Borrowed fixed storage for one external PI mutex.
#[derive(Clone, Copy)]
pub struct PiMutexStorage<'lock> {
    pub owner: &'lock AtomicU64,
    pub generation: &'lock AtomicU64,
    pub wait_state: &'lock AtomicU8,
    pub wait_words: &'lock UnsafeCell<[MaybeUninit<usize>; PI_MUTEX_WAIT_STORAGE_WORDS]>,
}

impl<'lock> PiMutexStorage<'lock> {
    fn core(self) -> PiMutexCoreView<'lock> {
        PiMutexCoreView::from_parts(
            self.owner,
            self.generation,
            self.wait_state,
            self.wait_words,
        )
    }
}

/// Exclusive fixed storage borrowed while an external PI mutex is destroyed.
pub struct PiMutexStorageMut<'lock> {
    pub owner: &'lock mut AtomicU64,
    pub generation: &'lock mut AtomicU64,
    pub wait_state: &'lock mut AtomicU8,
    pub wait_words: &'lock mut UnsafeCell<[MaybeUninit<usize>; PI_MUTEX_WAIT_STORAGE_WORDS]>,
}

/// One complete external PI-mutex acquisition request.
pub struct MutexAcquireRequest<'lock> {
    pub storage: PiMutexStorage<'lock>,
    pub next_waiter_sequence: &'lock AtomicU64,
    pub class: LockClass<'lock>,
    pub lock_addr: usize,
    pub subclass: u32,
    pub is_try: bool,
    pub caller: &'static Location<'static>,
}

/// Acquires an external PI mutex through the native Linux-RT-style algorithm.
pub fn mutex_acquire(request: MutexAcquireRequest<'_>) -> bool {
    let lockdep = prepare_lockdep(&request);
    let algorithm = PiMutexAlgorithm::new(request.storage.core(), request.next_waiter_sequence);
    let acquired = if request.is_try {
        algorithm.try_lock_pi()
    } else {
        algorithm.lock_pi();
        true
    };
    finish_lockdep(lockdep, acquired);
    acquired
}

/// Releases an external PI mutex and completes any scheduler-owned handoff.
pub fn mutex_release(storage: PiMutexStorage<'_>, lock_addr: usize) {
    release_lockdep(lock_addr);
    unsafe {
        // SAFETY: the external raw-mutex guard proves current owns this lock
        // through the complete scheduler handoff transaction.
        PiMutexAlgorithm::unlock_core(storage.core());
    }
}

/// Releases one deliberately leaked external PI-mutex guard.
pub fn mutex_force_release(storage: PiMutexStorage<'_>, lock_addr: usize) {
    mutex_release(storage, lock_addr);
}

/// Returns whether the current task owns an external PI mutex.
pub fn mutex_is_owned_by_current(storage: PiMutexStorage<'_>) -> bool {
    PiMutexAlgorithm::core_is_owned_by_current(storage.core())
}

/// Returns whether an external PI mutex has an owner or pending handoff.
pub fn mutex_is_locked(storage: PiMutexStorage<'_>) -> bool {
    PiMutexAlgorithm::core_is_locked(storage.core())
}

/// Destroys scheduler-owned inline waiter state after the wrapper is unique.
pub fn mutex_destroy(storage: PiMutexStorageMut<'_>) {
    destroy_pi_mutex_storage(
        storage.owner,
        storage.generation,
        storage.wait_state,
        storage.wait_words,
    );
}

#[cfg(feature = "lockdep")]
fn prepare_lockdep(request: &MutexAcquireRequest<'_>) -> LockdepAcquire {
    LockdepAcquire::prepare_view(LockdepAcquireRequest {
        map: LockdepMapView::new(request.class.class_id, request.class.class_key),
        addr: request.lock_addr,
        subclass: request.subclass,
        is_try: request.is_try,
        caller: request.caller,
    })
}

#[cfg(not(feature = "lockdep"))]
#[derive(Clone, Copy)]
struct LockdepAcquire;

#[cfg(not(feature = "lockdep"))]
fn prepare_lockdep(request: &MutexAcquireRequest<'_>) -> LockdepAcquire {
    let _ = request;
    LockdepAcquire
}

fn finish_lockdep(lockdep: LockdepAcquire, acquired: bool) {
    #[cfg(feature = "lockdep")]
    lockdep.finish(acquired);

    #[cfg(not(feature = "lockdep"))]
    let _ = (lockdep, acquired);
}

fn release_lockdep(lock_addr: usize) {
    #[cfg(feature = "lockdep")]
    crate::sync::mutex::lockdep::release_external(lock_addr);

    #[cfg(not(feature = "lockdep"))]
    let _ = lock_addr;
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
    use std::sync::Mutex;

    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn external_fast_path_uses_native_owner_and_lockdep() {
        let _serial = TEST_LOCK.lock().unwrap();
        let _runtime = crate::test_runtime::InstalledDefaultTaskRuntime::new();
        let mut owner = AtomicU64::new(0);
        let mut generation = AtomicU64::new(0);
        let mut wait_state = AtomicU8::new(0);
        let mut wait_words = UnsafeCell::new([MaybeUninit::uninit(); PI_MUTEX_WAIT_STORAGE_WORDS]);
        let sequence = AtomicU64::new(0);
        let class_id = AtomicU32::new(0);
        let class_key = AtomicPtr::new(core::ptr::null_mut());
        let lock_addr = core::ptr::from_ref(&owner) as usize;
        let storage = PiMutexStorage {
            owner: &owner,
            generation: &generation,
            wait_state: &wait_state,
            wait_words: &wait_words,
        };

        assert!(mutex_acquire(MutexAcquireRequest {
            storage,
            next_waiter_sequence: &sequence,
            class: LockClass {
                class_id: &class_id,
                class_key: &class_key,
            },
            lock_addr,
            subclass: 0,
            is_try: false,
            caller: Location::caller(),
        }));
        assert_ne!(owner.load(Ordering::Relaxed), 0);
        assert_eq!(generation.load(Ordering::Relaxed), 0);
        assert!(mutex_is_owned_by_current(storage));
        assert!(mutex_is_locked(storage));
        #[cfg(feature = "lockdep")]
        assert!(crate::sync::lockdep::current_task_held_lock_snapshot().contains_addr(lock_addr));

        mutex_release(storage, lock_addr);
        assert_eq!(owner.load(Ordering::Relaxed), 0);
        assert!(!mutex_is_locked(storage));
        #[cfg(feature = "lockdep")]
        assert!(!crate::sync::lockdep::current_task_held_lock_snapshot().contains_addr(lock_addr));

        mutex_destroy(PiMutexStorageMut {
            owner: &mut owner,
            generation: &mut generation,
            wait_state: &mut wait_state,
            wait_words: &mut wait_words,
        });
        assert_eq!(generation.load(Ordering::Relaxed), 0);
    }
}
