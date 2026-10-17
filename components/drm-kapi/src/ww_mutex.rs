// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.

//! Wait/wound (wound-wait) and wait/die mutual exclusion locks.
//!
//! Provides deadlock-detecting mutex acquisitions parameterized by an acquire
//! context (`WwAcquireCtx`). Transactions acquire locks in arbitrary order;
//! when contention occurs, timestamps (ticket stamps) determine whether a
//! contending task waits, wounds the current holder, or backs off with
//! `-EDEADLK` to break potential cycles.

extern crate alloc;

use core::{
    ffi::c_void,
    ptr::null_mut,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU16, AtomicU64, Ordering},
};

use crate::{
    locks::NoPreemptIrqSave,
    mutex::{mutex_lock, mutex_trylock, mutex_unlock},
    types::{Mutex, WwAcquireCtx},
};

/// Mutual exclusion lock supporting wait/wound and wait/die acquisition protocols.
#[repr(C)]
pub struct WwMutex {
    pub base: Mutex,
    pub ctx: *mut WwAcquireCtx,
}

const _: [(); 32] = [(); core::mem::size_of::<WwMutex>()];
const _: [(); 0] = [(); core::mem::offset_of!(WwMutex, base)];
const _: [(); 24] = [(); core::mem::offset_of!(WwMutex, ctx)];

/// Class definition for wait/wound locks specifying acquisition class characteristics.
#[repr(C)]
pub struct WwClass {
    pub stamp: AtomicU64,
    pub is_wait_die: u16,
}

/// Global reservation lock class operating under wait/die semantics.
pub static RESERVATION_WW_CLASS: WwClass = WwClass {
    stamp: AtomicU64::new(0),
    is_wait_die: 1,
};

/// Global CRTC modeset lock class operating under wound/wait semantics.
pub static CRTC_WW_CLASS: WwClass = WwClass {
    stamp: AtomicU64::new(0),
    is_wait_die: 0,
};

/// Maximum number of inline waiter tracking entries before falling back to overflow storage.
const INLINE_WAITER_CAPACITY: usize = 128;

/// Entry in the waiter registry tracking a context blocked on a specific lock.
struct WaiterSlot {
    lock: AtomicPtr<WwMutex>,
    ctx: AtomicPtr<WwAcquireCtx>,
}

impl WaiterSlot {
    const fn empty() -> Self {
        Self {
            lock: AtomicPtr::new(null_mut()),
            ctx: AtomicPtr::new(null_mut()),
        }
    }
}

static INLINE_WAITERS: [WaiterSlot; INLINE_WAITER_CAPACITY] =
    [const { WaiterSlot::empty() }; INLINE_WAITER_CAPACITY];

struct OverflowWaiters(core::cell::UnsafeCell<Option<alloc::vec::Vec<WaiterSlot>>>);

// SAFETY: All accesses to `OVERFLOW_WAITERS` are guarded by `METADATA_SPINLOCK`.
unsafe impl Sync for OverflowWaiters {}

static OVERFLOW_WAITERS: OverflowWaiters = OverflowWaiters(core::cell::UnsafeCell::new(None));
static METADATA_SPINLOCK: AtomicBool = AtomicBool::new(false);

/// RAII guard providing mutual exclusion for wait/wound metadata structures.
struct MetadataGuard {
    _irq: NoPreemptIrqSave,
}

impl MetadataGuard {
    fn acquire() -> Self {
        let irq_guard = NoPreemptIrqSave::new();
        while METADATA_SPINLOCK
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Self { _irq: irq_guard }
    }
}

impl Drop for MetadataGuard {
    fn drop(&mut self) {
        METADATA_SPINLOCK.store(false, Ordering::Release);
    }
}

/// Helper methods for tracking and querying blocked acquire contexts.
struct WaiterRegistry;

impl WaiterRegistry {
    /// Registers a context as waiting for the given lock.
    fn register(lock: *mut WwMutex, ctx: *mut WwAcquireCtx) {
        for slot in INLINE_WAITERS.iter() {
            if slot.ctx.load(Ordering::Acquire).is_null() {
                slot.lock.store(lock, Ordering::Release);
                slot.ctx.store(ctx, Ordering::Release);
                return;
            }
        }

        // SAFETY: Caller holds `MetadataGuard`, ensuring exclusive access to overflow vector.
        let overflow = unsafe { &mut *OVERFLOW_WAITERS.0.get() };
        let entries = overflow.get_or_insert_with(alloc::vec::Vec::new);
        entries.push(WaiterSlot {
            lock: AtomicPtr::new(lock),
            ctx: AtomicPtr::new(ctx),
        });
    }

    /// Unregisters a context from waiter tracking across inline and overflow slots.
    fn unregister(ctx: *mut WwAcquireCtx) {
        for slot in INLINE_WAITERS.iter() {
            if slot.ctx.load(Ordering::Acquire) == ctx {
                slot.ctx.store(null_mut(), Ordering::Release);
                slot.lock.store(null_mut(), Ordering::Release);
                return;
            }
        }

        // SAFETY: Caller holds `MetadataGuard`, ensuring exclusive access to overflow vector.
        let overflow = unsafe { &mut *OVERFLOW_WAITERS.0.get() };
        let Some(entries) = overflow.as_mut() else {
            return;
        };
        if let Some(index) = entries
            .iter()
            .position(|slot| slot.ctx.load(Ordering::Acquire) == ctx)
        {
            entries.swap_remove(index);
        }
    }

    /// Evaluates a predicate over all contexts currently waiting on the given lock.
    fn any_waiter<P: FnMut(*mut WwAcquireCtx) -> bool>(
        lock: *mut WwMutex,
        mut predicate: P,
    ) -> bool {
        for slot in INLINE_WAITERS.iter() {
            if slot.lock.load(Ordering::Acquire) == lock {
                let ctx = slot.ctx.load(Ordering::Acquire);
                if !ctx.is_null() && predicate(ctx) {
                    return true;
                }
            }
        }

        // SAFETY: Caller holds `MetadataGuard`, ensuring exclusive access to overflow vector.
        let overflow = unsafe { &*OVERFLOW_WAITERS.0.get() };
        if let Some(entries) = overflow.as_ref() {
            for slot in entries.iter() {
                if slot.lock.load(Ordering::Acquire) == lock {
                    let ctx = slot.ctx.load(Ordering::Acquire);
                    if !ctx.is_null() && predicate(ctx) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Collects all context pointers currently waiting on `lock`.
    #[cfg(test)]
    fn collect_waiters(lock: *mut WwMutex) -> alloc::vec::Vec<*mut WwAcquireCtx> {
        let mut list = alloc::vec::Vec::new();
        for slot in INLINE_WAITERS.iter() {
            if slot.lock.load(Ordering::Acquire) == lock {
                let ctx = slot.ctx.load(Ordering::Acquire);
                if !ctx.is_null() {
                    list.push(ctx);
                }
            }
        }

        // SAFETY: Caller holds `MetadataGuard`, ensuring exclusive access to overflow vector.
        let overflow = unsafe { &*OVERFLOW_WAITERS.0.get() };
        if let Some(entries) = overflow.as_ref() {
            for slot in entries.iter() {
                if slot.lock.load(Ordering::Acquire) == lock {
                    let ctx = slot.ctx.load(Ordering::Acquire);
                    if !ctx.is_null() {
                        list.push(ctx);
                    }
                }
            }
        }
        list
    }
}

/// Returns `true` if context `a` has a larger (younger) timestamp than context `b`,
/// taking modular 64-bit counter wraparound into account.
///
/// # Safety
/// Both `a` and `b` must point to valid, initialized [`WwAcquireCtx`] instances.
unsafe fn is_stamp_younger(a: *const WwAcquireCtx, b: *const WwAcquireCtx) -> bool {
    // SAFETY: Caller guarantees `a` and `b` are valid, properly aligned context pointers.
    let diff = unsafe { (*a).stamp.wrapping_sub((*b).stamp) };
    (diff as i64) > 0
}

/// Returns whether the acquire context has been marked as wounded.
///
/// # Safety
/// `ctx` must point to a valid, initialized [`WwAcquireCtx`].
unsafe fn is_context_wounded(ctx: *const WwAcquireCtx) -> bool {
    // SAFETY: Caller guarantees `ctx` points to a valid, properly aligned WwAcquireCtx.
    let wounded_ptr = unsafe { core::ptr::addr_of!((*ctx).wounded) } as *mut u16;
    // SAFETY: Pointer is aligned to 2 bytes and points to `wounded` field.
    unsafe { AtomicU16::from_ptr(wounded_ptr) }.load(Ordering::Acquire) != 0
}

/// Atomically marks the given context as wounded.
///
/// # Safety
/// `ctx` must point to a valid, initialized [`WwAcquireCtx`].
unsafe fn mark_context_wounded(ctx: *mut WwAcquireCtx) {
    // SAFETY: Caller guarantees `ctx` points to a valid, properly aligned WwAcquireCtx.
    let wounded_ptr = unsafe { core::ptr::addr_of_mut!((*ctx).wounded) };
    // SAFETY: Pointer is aligned to 2 bytes and points to `wounded` field.
    unsafe { AtomicU16::from_ptr(wounded_ptr) }.store(1, Ordering::Release);
}

/// Determines whether `ctx` must abort its lock attempt and back off with `-EDEADLK`.
///
/// # Safety
/// `lock` and `ctx` must be valid, non-null pointers, and the caller must hold `MetadataGuard`.
unsafe fn should_abort_deadlock(lock: *mut WwMutex, ctx: *mut WwAcquireCtx) -> bool {
    // A context that holds no locks cannot form part of a deadlock cycle.
    // SAFETY: Caller guarantees `ctx` points to a live acquire context.
    if unsafe { (*ctx).acquired } == 0 {
        return false;
    }

    // SAFETY: Caller guarantees `ctx` points to a live acquire context.
    let is_wait_die = unsafe { (*ctx).is_wait_die != 0 };
    if !is_wait_die {
        // Wound-wait: abort if this context has been wounded by an older transaction.
        // SAFETY: Caller guarantees `ctx` points to a live acquire context.
        return unsafe { is_context_wounded(ctx) };
    }

    // Wait-die: abort if contending with an older holder.
    // SAFETY: Caller guarantees `lock` points to a live WwMutex.
    let holder = unsafe { (*lock).ctx };
    // SAFETY: If `holder` is non-null, it points to the current lock owner context.
    if !holder.is_null() && holder != ctx && unsafe { is_stamp_younger(ctx, holder) } {
        return true;
    }

    // Wait-die: abort if any existing waiter on this lock is older than us.
    WaiterRegistry::any_waiter(lock, |waiter| {
        // SAFETY: `waiter` is a registered active context.
        waiter != ctx && unsafe { is_stamp_younger(ctx, waiter) }
    })
}

/// Checks whether the underlying mutex currently has no holder.
///
/// # Safety
/// `lock` must point to a live, initialized [`WwMutex`].
unsafe fn is_lock_unlocked(lock: *mut WwMutex) -> bool {
    // SAFETY: Caller guarantees `lock` points to a live WwMutex.
    !unsafe { crate::mutex::mutex_is_locked(core::ptr::addr_of_mut!((*lock).base)) }
}

/// Wakes any tasks waiting on wait-queue events associated with this lock.
fn notify_waiters(_lock: *mut WwMutex) {
    crate::wait::wake_all();
}

/// Updates ownership metadata following successful acquisition of the base mutex.
///
/// # Safety
/// `lock` must point to a live [`WwMutex`] whose base mutex has just been acquired by current task.
/// `ctx`, if non-null, must be a valid, live acquire context.
unsafe fn publish_acquisition(lock: *mut WwMutex, ctx: *mut WwAcquireCtx) {
    let guard = MetadataGuard::acquire();
    // SAFETY: Exclusive metadata access held under `MetadataGuard`.
    unsafe { (*lock).ctx = ctx };

    if !ctx.is_null() {
        // SAFETY: Modifying `acquired` count under metadata lock.
        unsafe {
            (*ctx).acquired = (*ctx)
                .acquired
                .checked_add(1)
                .expect("ww acquired counter overflow");
        }

        // Under wound-wait, if any queued waiter is older than us and holds locks, it wounds us.
        // SAFETY: Caller guarantees `ctx` is valid.
        let is_wait_die = unsafe { (*ctx).is_wait_die != 0 };
        if !is_wait_die {
            let wounded_by_waiter = WaiterRegistry::any_waiter(lock, |waiter| {
                // SAFETY: `waiter` points to a registered active acquire context.
                let holds_locks = unsafe { (*waiter).acquired } > 0;
                // SAFETY: Both `ctx` and `waiter` are live context pointers.
                holds_locks && unsafe { is_stamp_younger(ctx, waiter) }
            });
            if wounded_by_waiter {
                // SAFETY: `ctx` is guaranteed valid.
                unsafe { mark_context_wounded(ctx) };
            }
        }
    }

    drop(guard);
    notify_waiters(lock);
}

/// Initializes an acquire context for subsequent wait/wound lock operations.
///
/// # Safety
/// `ctx` must point to valid, writable, properly aligned storage for a [`WwAcquireCtx`]
/// that is not concurrently accessed. `class` must have static lifetime.
pub unsafe fn ww_acquire_init(ctx: *mut WwAcquireCtx, class: &'static WwClass) {
    assert!(!ctx.is_null());
    let task = crate::kernel_services::current_task_id()
        .and_then(|id| usize::try_from(id).ok())
        .filter(|id| *id != 0)
        .expect("ww_acquire_init requires an active scheduler task");

    // SAFETY: Caller guarantees `ctx` points to valid, unaliased, writable storage.
    unsafe {
        (*ctx).task = task as *mut c_void;
        (*ctx).stamp = class.stamp.fetch_add(1, Ordering::Relaxed) + 1;
        (*ctx).acquired = 0;
        (*ctx).wounded = 0;
        (*ctx).is_wait_die = class.is_wait_die;
    }
}

/// Signals the completion of the lock acquisition phase.
///
/// # Safety
/// `ctx` must point to a live, initialized [`WwAcquireCtx`].
pub unsafe fn ww_acquire_done(ctx: *mut WwAcquireCtx) {
    assert!(!ctx.is_null());
}

/// Finalizes an acquire context after all held locks have been unlocked.
///
/// # Safety
/// `ctx` must point to a live, initialized [`WwAcquireCtx`] belonging to the current task.
pub unsafe fn ww_acquire_fini(ctx: *mut WwAcquireCtx) {
    assert!(!ctx.is_null());
    // SAFETY: Reading `acquired` count from valid context pointer.
    let held_count = unsafe { (*ctx).acquired };
    assert_eq!(held_count, 0, "ww context finalized with held locks");
}

/// Acquires a [`WwMutex`], blocking until acquired or until deadlock avoidance requires backoff.
///
/// Returns `0` on success, or `-EDEADLK` if deadlock avoidance triggers.
///
/// # Safety
/// `lock` must point to a live, initialized [`WwMutex`]. If `ctx` is non-null,
/// it must designate a live [`WwAcquireCtx`] owned by the calling task.
pub unsafe fn ww_mutex_lock(lock: *mut WwMutex, ctx: *mut WwAcquireCtx) -> i32 {
    // SAFETY: Forwarding caller guarantees to `ww_mutex_lock_inner`.
    unsafe { ww_mutex_lock_inner(lock, ctx, false) }
}

/// Internal lock acquisition implementation handling both interruptible and non-interruptible modes.
///
/// # Safety
/// `lock` must point to a live initialized [`WwMutex`]. Non-null `ctx` must designate a live
/// [`WwAcquireCtx`] belonging to the calling task.
unsafe fn ww_mutex_lock_inner(
    lock: *mut WwMutex,
    ctx: *mut WwAcquireCtx,
    interruptible: bool,
) -> i32 {
    assert!(!lock.is_null());

    // Without an acquire context, fallback to regular mutex behavior.
    if ctx.is_null() {
        if interruptible {
            // SAFETY: Caller guarantees `lock` is valid.
            return unsafe {
                crate::mutex::mutex_lock_interruptible_impl(core::ptr::addr_of_mut!((*lock).base))
            };
        }
        // SAFETY: Caller guarantees `lock` is valid.
        unsafe { mutex_lock(core::ptr::addr_of_mut!((*lock).base)) };
        return 0;
    }

    // Detecting recursive acquisition on the same context.
    // SAFETY: Caller guarantees `lock` is valid.
    if unsafe { (*lock).ctx } == ctx {
        return -crate::config::EALREADY;
    }

    // A context that currently holds no locks clears any previous wounded status.
    // SAFETY: Caller guarantees `ctx` is valid.
    if unsafe { (*ctx).acquired } == 0 {
        // SAFETY: Resetting wounded field for fresh transaction.
        unsafe { (*ctx).wounded = 0 };
    }

    // Fast-path attempt: try acquiring uncontended mutex immediately.
    // SAFETY: Caller guarantees `lock` is valid.
    if unsafe { mutex_trylock(core::ptr::addr_of_mut!((*lock).base)) } {
        // SAFETY: Lock acquired successfully; publishing ownership.
        unsafe { publish_acquisition(lock, ctx) };
        return 0;
    }

    // Contended slow path: register waiter and evaluate wound-wait / wait-die rules.
    let guard = MetadataGuard::acquire();

    // In wound-wait mode, an older contending waiter wounds a younger holder.
    // SAFETY: Caller guarantees `ctx` is valid.
    let is_wait_die = unsafe { (*ctx).is_wait_die != 0 };
    if !is_wait_die {
        // SAFETY: Caller guarantees `lock` is valid.
        let holder = unsafe { (*lock).ctx };
        // SAFETY: Validating holder context and comparing timestamps.
        if !holder.is_null()
            && holder != ctx
            && unsafe { (*ctx).acquired } > 0
            && unsafe { is_stamp_younger(holder, ctx) }
        {
            // SAFETY: Holder is a live context holding this lock.
            unsafe { mark_context_wounded(holder) };
        }
    }

    WaiterRegistry::register(lock, ctx);

    // Initial deadlock abort evaluation before sleeping.
    // SAFETY: Both `lock` and `ctx` are valid; metadata lock is held.
    if unsafe { should_abort_deadlock(lock, ctx) } {
        WaiterRegistry::unregister(ctx);
        drop(guard);
        notify_waiters(lock);
        return -crate::config::EDEADLK;
    }

    drop(guard);
    notify_waiters(lock);

    // Contention loop: repeatedly try to acquire the underlying mutex, check abort conditions, and wait.
    loop {
        // SAFETY: Attempting acquisition of the base mutex.
        if unsafe { mutex_trylock(core::ptr::addr_of_mut!((*lock).base)) } {
            break;
        }

        let query_abort = || {
            let _guard = MetadataGuard::acquire();
            // SAFETY: `lock` and `ctx` are valid; metadata lock is held.
            unsafe { should_abort_deadlock(lock, ctx) }
        };

        if interruptible {
            if crate::signal::signal_pending_current() {
                let _guard = MetadataGuard::acquire();
                WaiterRegistry::unregister(ctx);
                drop(_guard);
                notify_waiters(lock);
                return -crate::config::EINTR;
            }
            let is_ready = || {
                crate::signal::signal_pending_current()
                    || query_abort()
                    // SAFETY: `lock` points to a live WwMutex.
                    || unsafe { is_lock_unlocked(lock) }
            };
            let _ = crate::wait::wait_timeout_until(Some(10_000_000), &is_ready);
        } else {
            let is_ready = || {
                query_abort()
                    // SAFETY: `lock` points to a live WwMutex.
                    || unsafe { is_lock_unlocked(lock) }
            };
            let _ = crate::wait::wait_until_ready(None, &is_ready);
        }

        // If the lock is still held and we must abort, exit with -EDEADLK.
        // SAFETY: `lock` points to a live WwMutex.
        if !unsafe { is_lock_unlocked(lock) } && query_abort() {
            let _guard = MetadataGuard::acquire();
            WaiterRegistry::unregister(ctx);
            drop(_guard);
            notify_waiters(lock);
            return -crate::config::EDEADLK;
        }
    }

    // Mutex acquired: clean up waiter registration and publish context ownership.
    {
        let _guard = MetadataGuard::acquire();
        WaiterRegistry::unregister(ctx);
    }
    // SAFETY: Base mutex was acquired; publishing context ownership.
    unsafe { publish_acquisition(lock, ctx) };
    0
}

/// Returns whether the underlying mutex is held.
///
/// # Safety
/// `lock` must point to a live, initialized [`WwMutex`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ww_mutex_is_locked(lock: *mut WwMutex) -> bool {
    assert!(!lock.is_null());
    // SAFETY: Caller guarantees `lock` points to a live initialized WwMutex.
    unsafe { crate::mutex::mutex_is_locked(core::ptr::addr_of_mut!((*lock).base)) }
}

/// Slow path re-acquisition after dropping all other held locks.
///
/// Since the calling context holds no other locks (`acquired == 0`), deadlock is impossible
/// and acquisition is guaranteed to succeed without `-EDEADLK`.
///
/// # Safety
/// `lock` must point to a live, initialized [`WwMutex`]. `ctx` must point to a live [`WwAcquireCtx`]
/// owned by the current task with an `acquired` count of zero.
pub unsafe fn ww_mutex_lock_slow(lock: *mut WwMutex, ctx: *mut WwAcquireCtx) {
    // SAFETY: Caller guarantees valid pointers and zero held locks.
    let ret = unsafe { ww_mutex_lock_inner(lock, ctx, false) };
    assert_eq!(ret, 0, "ww_mutex_lock_slow with held locks");
}

/// Attempts non-blocking acquisition of the mutex.
///
/// Returns `true` if the lock was acquired, or `false` if it is held by another task.
///
/// # Safety
/// `lock` must point to a live, initialized [`WwMutex`]. If `ctx` is non-null, it must designate
/// a live [`WwAcquireCtx`] owned by the calling task.
pub unsafe fn ww_mutex_trylock(lock: *mut WwMutex, ctx: *mut WwAcquireCtx) -> bool {
    if lock.is_null() {
        return false;
    }
    if ctx.is_null() {
        // SAFETY: Caller guarantees `lock` is valid; trylocking base mutex directly.
        return unsafe { mutex_trylock(core::ptr::addr_of_mut!((*lock).base)) };
    }
    // SAFETY: Caller guarantees `ctx` is valid.
    if unsafe { (*ctx).acquired } == 0 {
        // SAFETY: Resetting wounded field for fresh transaction.
        unsafe { (*ctx).wounded = 0 };
    }
    // SAFETY: Attempting non-blocking acquire on underlying mutex.
    if !unsafe { mutex_trylock(core::ptr::addr_of_mut!((*lock).base)) } {
        return false;
    }
    // SAFETY: Mutex was acquired; publishing context ownership.
    unsafe { publish_acquisition(lock, ctx) };
    true
}

/// Releases context ownership and unlocks the underlying mutex.
///
/// # Safety
/// `lock` must point to a live [`WwMutex`] currently held by the calling task.
pub unsafe fn ww_mutex_unlock(lock: *mut WwMutex) {
    assert!(!lock.is_null());
    {
        let _guard = MetadataGuard::acquire();
        // SAFETY: Caller guarantees `lock` is valid and held.
        let ctx = unsafe { (*lock).ctx };
        // SAFETY: Resetting lock context pointer under metadata lock.
        unsafe { (*lock).ctx = null_mut() };
        if !ctx.is_null() {
            // SAFETY: Decrementing `acquired` count on valid context pointer.
            let acquired = unsafe { &mut (*ctx).acquired };
            assert!(*acquired > 0, "ww acquired count underflow");
            *acquired -= 1;
        }
    }
    // SAFETY: Releasing the underlying mutex.
    unsafe { mutex_unlock(core::ptr::addr_of_mut!((*lock).base)) };
}

/// Acquires a wait/wound mutex in an interruptible manner from C ABI.
///
/// # Safety
/// `lock` must point to a live, initialized [`WwMutex`]. If `ctx` is non-null,
/// it must point to a live [`WwAcquireCtx`] owned by the calling task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ww_mutex_lock_interruptible(lock: *mut c_void, ctx: *mut c_void) -> i32 {
    // SAFETY: Caller guarantees valid pointers matching WwMutex and WwAcquireCtx layouts.
    unsafe { ww_mutex_lock_inner(lock.cast::<WwMutex>(), ctx.cast::<WwAcquireCtx>(), true) }
}

/// Releases a wait/wound mutex from C ABI.
///
/// # Safety
/// `lock` must point to a live [`WwMutex`] currently held by the calling task.
#[unsafe(export_name = "ww_mutex_unlock")]
pub unsafe extern "C" fn c_ww_mutex_unlock(lock: *mut c_void) {
    // SAFETY: Caller guarantees `lock` points to a live WwMutex held by current task.
    unsafe { ww_mutex_unlock(lock.cast::<WwMutex>()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_context(stamp: u64, acquired: u32, wait_die: u16) -> WwAcquireCtx {
        WwAcquireCtx {
            task: null_mut(),
            stamp,
            acquired,
            wounded: 0,
            is_wait_die: wait_die,
        }
    }

    fn create_test_lock(holder: *mut WwAcquireCtx) -> WwMutex {
        WwMutex {
            // SAFETY: An all-zero memory pattern represents an unlocked Mutex.
            base: unsafe { core::mem::zeroed() },
            ctx: holder,
        }
    }

    #[test]
    fn wait_die_younger_context_dies_against_older_holder() {
        let _guard = MetadataGuard::acquire();
        let mut old = create_test_context(1, 1, 1);
        let mut young = create_test_context(2, 1, 1);
        let mut lock = create_test_lock(&mut old);
        // SAFETY: `lock` and contexts are valid test fixtures.
        assert!(unsafe { should_abort_deadlock(&mut lock, &mut young) });
        // SAFETY: `lock` and contexts are valid test fixtures.
        assert!(!unsafe { should_abort_deadlock(&mut lock, &mut old) });
    }

    #[test]
    fn wait_die_context_holding_nothing_never_dies() {
        let _guard = MetadataGuard::acquire();
        let mut old = create_test_context(1, 1, 1);
        let mut young = create_test_context(2, 0, 1);
        let mut lock = create_test_lock(&mut old);
        // SAFETY: `lock` and contexts are valid test fixtures.
        assert!(!unsafe { should_abort_deadlock(&mut lock, &mut young) });
    }

    #[test]
    fn wait_die_younger_context_dies_against_older_waiter() {
        let _guard = MetadataGuard::acquire();
        let mut old_waiter = create_test_context(2, 1, 1);
        let mut young = create_test_context(3, 1, 1);
        let mut lock = create_test_lock(null_mut());
        let lock_ptr: *mut WwMutex = &mut lock;
        WaiterRegistry::register(lock_ptr, &mut old_waiter);
        // SAFETY: `lock_ptr` and contexts are valid test fixtures.
        assert!(unsafe { should_abort_deadlock(lock_ptr, &mut young) });
        // SAFETY: `lock_ptr` and contexts are valid test fixtures.
        assert!(!unsafe { should_abort_deadlock(lock_ptr, &mut old_waiter) });
        WaiterRegistry::unregister(&mut old_waiter);
        // SAFETY: `lock_ptr` and contexts are valid test fixtures.
        assert!(!unsafe { should_abort_deadlock(lock_ptr, &mut young) });
    }

    #[test]
    fn wound_wait_dies_only_when_wounded_and_holding() {
        let _guard = MetadataGuard::acquire();
        let mut holder = create_test_context(2, 1, 0);
        let mut lock = create_test_lock(null_mut());
        let lock_ptr: *mut WwMutex = &mut lock;
        // SAFETY: `lock_ptr` and context are valid test fixtures.
        assert!(!unsafe { should_abort_deadlock(lock_ptr, &mut holder) });
        holder.wounded = 1;
        // SAFETY: `lock_ptr` and context are valid test fixtures.
        assert!(unsafe { should_abort_deadlock(lock_ptr, &mut holder) });
        let mut empty = create_test_context(3, 0, 0);
        empty.wounded = 1;
        // SAFETY: `lock_ptr` and context are valid test fixtures.
        assert!(!unsafe { should_abort_deadlock(lock_ptr, &mut empty) });
    }

    #[test]
    fn reservation_and_crtc_classes_have_expected_modes() {
        assert_eq!(RESERVATION_WW_CLASS.is_wait_die, 1);
        assert_eq!(CRTC_WW_CLASS.is_wait_die, 0);
    }

    #[test]
    fn test_ww_mutex_waiters_overflow() {
        let _guard = MetadataGuard::acquire();
        let mut lock = create_test_lock(null_mut());
        let lock_ptr: *mut WwMutex = &mut lock;
        let mut ctxs: alloc::vec::Vec<WwAcquireCtx> = alloc::vec::Vec::new();
        for i in 0..140 {
            ctxs.push(create_test_context(i as u64 + 10, 1, 1));
        }
        for c in ctxs.iter_mut() {
            WaiterRegistry::register(lock_ptr, c);
        }
        let list = WaiterRegistry::collect_waiters(lock_ptr);
        assert_eq!(list.len(), 140);
        for c in ctxs.iter_mut() {
            WaiterRegistry::unregister(c);
        }
        let list_after = WaiterRegistry::collect_waiters(lock_ptr);
        assert_eq!(list_after.len(), 0);
    }

    #[test]
    fn test_stamp_ordering_wraparound() {
        let c1 = create_test_context(u64::MAX, 1, 0);
        let c2 = create_test_context(0, 1, 0);
        // c2 was issued after c1 across counter wraparound, so c2 is younger.
        // SAFETY: Both contexts are valid local structs.
        assert!(unsafe { is_stamp_younger(&c2, &c1) });
        // SAFETY: Both contexts are valid local structs.
        assert!(!unsafe { is_stamp_younger(&c1, &c2) });
    }
}
