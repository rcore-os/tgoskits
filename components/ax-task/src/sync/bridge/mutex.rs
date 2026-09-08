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
    pub caller: &'static Location<'static>,
}

/// Acquires an external PI mutex through the native Linux-RT-style blocking path.
pub fn mutex_acquire(request: MutexAcquireRequest<'_>) {
    let lockdep = prepare_lockdep(&request, false);
    let algorithm = PiMutexAlgorithm::new(request.storage.core(), request.next_waiter_sequence);
    algorithm.lock_pi();
    finish_lockdep(lockdep, true);
}

/// Tries to acquire an external PI mutex without blocking.
pub fn mutex_try_acquire(request: MutexAcquireRequest<'_>) -> bool {
    let lockdep = prepare_lockdep(&request, true);
    let algorithm = PiMutexAlgorithm::new(request.storage.core(), request.next_waiter_sequence);
    let acquired = algorithm.try_lock_pi();
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
fn prepare_lockdep(request: &MutexAcquireRequest<'_>, is_try: bool) -> LockdepAcquire {
    LockdepAcquire::prepare_view(LockdepAcquireRequest {
        map: LockdepMapView::new(request.class.class_id, request.class.class_key),
        addr: request.lock_addr,
        subclass: request.subclass,
        is_try,
        caller: request.caller,
    })
}

#[cfg(not(feature = "lockdep"))]
#[derive(Clone, Copy)]
struct LockdepAcquire;

#[cfg(not(feature = "lockdep"))]
fn prepare_lockdep(request: &MutexAcquireRequest<'_>, is_try: bool) -> LockdepAcquire {
    let _ = (request, is_try);
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
