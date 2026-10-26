// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Sleepable Linux mutex operations for the source kernel's 64-bit/SMP
//! layout (`owner` at offset 0). The owner word stores the scheduler's stable
//! task identity; waiters recheck the particular mutex after every wake.

use core::{
    mem::size_of,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{config::CONFIG_DEBUG_MUTEXES, types::Mutex};

const _: [(); 24] = [(); size_of::<Mutex>()];
const OWNER_OFFSET: usize = 0;
const _: [(); 0] = [(); OWNER_OFFSET];
const _: [(); 8] = [(); core::mem::align_of::<Mutex>()];

/// Linux `mutex_destroy()` is compiled away when DEBUG_MUTEXES is disabled.
#[inline]
pub fn mutex_destroy(_mutex: &mut Mutex) {
    if CONFIG_DEBUG_MUTEXES {
        panic!("DEBUG_MUTEXES requires the Linux mutex debugging backend");
    }
}

/// # Safety
/// `mutex` must be non-null, properly aligned to at least 8 bytes, and point to a valid `Mutex`
/// whose owner field remains allocated and accessed concurrently only via atomic operations
/// for the duration of `'a`.
#[inline]
unsafe fn owner_word<'a>(mutex: *mut Mutex) -> &'a AtomicUsize {
    assert!(!mutex.is_null());
    // Linux v7.2.3 include/linux/mutex_types.h places atomic_long_t owner at
    // byte offset zero. Mutex is x86_64 c_ulong-sized and naturally aligned.
    // SAFETY: Adding 0 byte offset does not exceed allocation bounds, and `mutex`
    // is aligned to at least 8 bytes as asserted by compile-time assertions.
    let owner = unsafe { (mutex.cast::<u8>().add(OWNER_OFFSET)).cast::<usize>() };
    // SAFETY: `owner` points to the `owner` field at offset 0 of `mutex`. The caller guarantees
    // `mutex` is live, aligned to at least 8 bytes, and accessed concurrently only via atomic operations.
    unsafe { AtomicUsize::from_ptr(owner) }
}

pub trait MutexPointer {
    fn mutex_ptr(self) -> *mut Mutex;
}

impl MutexPointer for *mut Mutex {
    fn mutex_ptr(self) -> *mut Mutex {
        self
    }
}

impl MutexPointer for *const Mutex {
    fn mutex_ptr(self) -> *mut Mutex {
        self.cast_mut()
    }
}

impl MutexPointer for &mut Mutex {
    fn mutex_ptr(self) -> *mut Mutex {
        self
    }
}

impl MutexPointer for &Mutex {
    fn mutex_ptr(self) -> *mut Mutex {
        self as *const Mutex as *mut Mutex
    }
}

fn current_task_identity() -> usize {
    crate::kernel_services::current_task_id()
        .and_then(|task| usize::try_from(task).ok())
        .filter(|task| *task != 0)
        .expect("Linux mutex has no scheduler task identity")
}

fn current_owner() -> usize {
    assert!(
        crate::task_runtime::can_block_current(),
        "Linux mutex operation requires a task context"
    );
    current_task_identity()
}

/// Initialize the source C-layout mutex. Caller must have exclusive access
/// and must not reinitialize a held mutex, matching Linux's contract.
///
/// # Safety
/// The pointer selected by `mutex` must designate writable, properly aligned
/// `Mutex` storage with no concurrent users; it must not be reinitialized
/// while held or while waiters may be using it.
pub unsafe fn mutex_init<M: MutexPointer>(mutex: M) {
    let mutex = mutex.mutex_ptr();
    assert!(!mutex.is_null());
    // CONFIG_LOCKDEP, CONFIG_DEBUG_MUTEXES and PREEMPT_RT are disabled; the
    // source mutex's owner, raw wait-lock, osq tail and first-waiter fields
    // therefore initialize to zero/null as in __mutex_init_generic().
    // SAFETY: The caller guarantees `mutex` points to writable, properly aligned `Mutex` storage
    // of `size_of::<Mutex>()` bytes without concurrent access.
    unsafe { core::ptr::write_bytes(mutex.cast::<u8>(), 0, size_of::<Mutex>()) };
}

/// Try to acquire without sleeping, preserving Linux's owner identity.
///
/// # Safety
/// `mutex` must designate a live, properly aligned mutex initialized by
/// `mutex_init` and not concurrently destroyed or reinitialized.
pub unsafe fn mutex_trylock<M: MutexPointer>(mutex: M) -> bool {
    let Some(owner) = crate::kernel_services::current_task_id()
        .and_then(|task| usize::try_from(task).ok())
        .filter(|task| *task != 0)
    else {
        return false;
    };
    let mutex = mutex.mutex_ptr();
    // SAFETY: `mutex` is guaranteed by caller to be live, aligned, and initialized.
    unsafe { owner_word(mutex) }
        .compare_exchange(0, owner, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// Acquire the mutex, sleeping on contention and rechecking after every wake.
///
/// # Safety
/// `mutex` must designate a live, properly aligned mutex initialized by
/// `mutex_init` and not concurrently destroyed or reinitialized.
pub unsafe fn mutex_lock<M: MutexPointer>(mutex: M) {
    let task = current_owner();
    let mutex = mutex.mutex_ptr();
    // SAFETY: `mutex` is guaranteed by caller to be a live, aligned, and initialized `Mutex`.
    let owner = unsafe { owner_word(mutex) };
    loop {
        match owner.compare_exchange(0, task, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => return,
            Err(held_by) if held_by == task => {
                panic!("recursive Linux mutex lock");
            }
            Err(_) => {
                let available = || owner.load(Ordering::Acquire) == 0;
                let _ = crate::wait::wait_until_ready(None, &available);
            }
        }
    }
}

/// Release the mutex, checking that only its owner unlocks it.
///
/// # Safety
/// `mutex` must designate a live initialized mutex currently held by the
/// calling task.
pub unsafe fn mutex_unlock<M: MutexPointer>(mutex: M) {
    // Linux mutex_unlock() never sleeps; it only checks that the caller is the
    // owner, so it needs the current task identity and not a blocking context.
    let task = current_task_identity();
    let mutex = mutex.mutex_ptr();
    // SAFETY: `mutex` is guaranteed by caller to be a live, aligned, initialized `Mutex` held by the calling task.
    let owner = unsafe { owner_word(mutex) };
    assert_eq!(
        owner.swap(0, Ordering::Release),
        task,
        "Linux mutex unlocked by a non-owner"
    );
    crate::wait::wake_all();
}

#[inline]
/// # Safety
/// `mutex` must designate a live initialized mutex that is not concurrently
/// destroyed or reinitialized.
pub unsafe fn mutex_is_locked<M: MutexPointer>(mutex: M) -> bool {
    let mutex = mutex.mutex_ptr();
    assert!(!mutex.is_null());
    // SAFETY: `mutex` is guaranteed by caller to be non-null, aligned, and initialized.
    let owner = unsafe { owner_word(mutex) };
    owner.load(Ordering::Acquire) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_word_uses_the_first_c_ulong_and_locked_state() {
        let mut mutex = core::mem::MaybeUninit::<Mutex>::zeroed();
        let mutex_ptr = mutex.as_mut_ptr();
        // SAFETY: zeroed source mutex storage and exclusive local ownership.
        unsafe { mutex_init(mutex_ptr) };
        // SAFETY: the owner word is exactly the first field by Linux ABI.
        unsafe { owner_word(mutex_ptr) }.store(7, Ordering::Release);
        // SAFETY: valid local mutex storage.
        assert!(unsafe { mutex_is_locked(mutex_ptr) });
        // SAFETY: valid local mutex storage.
        unsafe { owner_word(mutex_ptr) }.store(0, Ordering::Release);
        // SAFETY: valid local mutex storage.
        assert!(!unsafe { mutex_is_locked(mutex_ptr) });
    }
}

/// Interruptible acquire: sleeps like [`mutex_lock`], and gives up with
/// `-EINTR` when the sleeping task is interrupted by a signal, as
/// `mutex_lock_interruptible()` does.
///
/// # Safety
/// `mutex` must designate a live, properly aligned mutex initialized by
/// `mutex_init` and not concurrently destroyed or reinitialized.
pub unsafe fn mutex_lock_interruptible_impl<M: MutexPointer>(mutex: M) -> i32 {
    let task = current_owner();
    let mutex = mutex.mutex_ptr();
    // SAFETY: `mutex` is guaranteed by caller to be a live, aligned, initialized `Mutex`.
    let owner = unsafe { owner_word(mutex) };
    loop {
        match owner.compare_exchange(0, task, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => return 0,
            Err(held_by) if held_by == task => {
                panic!("recursive Linux mutex lock");
            }
            Err(_) => {
                // Re-check the signal state every tick, since a signal does
                // not notify the mutex waiters.
                if crate::signal::signal_pending_current() {
                    return -crate::config::EINTR;
                }
                let ready = || {
                    owner.load(Ordering::Acquire) == 0 || crate::signal::signal_pending_current()
                };
                let _ = crate::wait::wait_timeout_until(Some(10_000_000), &ready);
            }
        }
    }
}

// C ABI names. The Rust helpers above keep their identifiers, so the exported
// symbols are attached with `export_name`.

/// Linux `mutex_unlock()`.
///
/// # Safety
/// `lock` must be a live initialized mutex currently held by the calling task.
#[unsafe(export_name = "mutex_unlock")]
pub unsafe extern "C" fn c_mutex_unlock(lock: *mut Mutex) {
    // SAFETY: Caller guarantees `lock` is a live initialized mutex held by the calling task.
    unsafe { mutex_unlock(lock) };
}

/// Linux `mutex_destroy()`.
///
/// # Safety
/// `lock` must be live initialized mutex storage with no owner or waiters.
#[unsafe(export_name = "mutex_destroy")]
pub unsafe extern "C" fn c_mutex_destroy(lock: *mut Mutex) {
    // SAFETY: Caller guarantees `lock` is non-null, aligned, points to live `Mutex` storage with no concurrent users.
    mutex_destroy(unsafe { &mut *lock });
}

/// Linux `__mutex_init()`: lockdep and debug names are compiled out.
/// Linux `mutex_init()`: the C macro expands to `__mutex_init()` with the
/// debug name and key compiled out, so only the lock is initialized.
///
/// # Safety
/// `lock` must point to writable, properly aligned mutex storage with no
/// concurrent users and must not refer to a held mutex.
#[unsafe(export_name = "mutex_init")]
pub unsafe extern "C" fn c_mutex_init(lock: *mut core::ffi::c_void) {
    // SAFETY: Caller guarantees `lock` points to writable, properly aligned Mutex storage with no concurrent users.
    unsafe { mutex_init(lock.cast::<Mutex>()) };
}

#[unsafe(no_mangle)]
/// # Safety
/// `lock` must point to writable, properly aligned mutex storage with no
/// concurrent users and must not refer to a held mutex.
pub unsafe extern "C" fn __mutex_init(
    lock: *mut Mutex,
    _name: *const u8,
    _key: *mut core::ffi::c_void,
) {
    // SAFETY: Caller guarantees `lock` points to writable, properly aligned Mutex storage with no concurrent users.
    unsafe { mutex_init(lock) };
}

/// Linux `mutex_lock_interruptible()`.
///
/// # Safety
/// `lock` must point to a live initialized mutex not concurrently destroyed
/// or reinitialized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mutex_lock_interruptible(lock: *mut Mutex) -> i32 {
    // SAFETY: Caller guarantees `lock` points to a live initialized Mutex not concurrently destroyed.
    unsafe { mutex_lock_interruptible_impl(lock) }
}

/// Linux `mutex_lock_interruptible_nested()`; the subclass only feeds lockdep.
///
/// # Safety
/// `lock` must point to a live initialized mutex not concurrently destroyed
/// or reinitialized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mutex_lock_interruptible_nested(lock: *mut Mutex, _subclass: u32) -> i32 {
    // SAFETY: Caller guarantees `lock` points to a live initialized Mutex not concurrently destroyed.
    unsafe { mutex_lock_interruptible_impl(lock) }
}
