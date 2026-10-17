// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
//! Architecture-neutral atomic storage for Linux spin-lock operations.
//!
//! The C-layout lock word remains inside shared structures, while task
//! preemption and local interrupt state are supplied by a narrow runtime
//! provider. The algorithm itself only uses Rust atomics and contains no
//! architecture-specific instructions.
//!
//! This module deliberately does not bind Linux `struct mutex`: the current
//! translation models it as an opaque 24-byte C-layout object, while the
//! kernel's sleepable mutex runtime (`tk-axsync`) is a larger task-aware Rust
//! object with a wait-queue lifecycle. Reinterpreting those bytes as a spin
//! lock or allocating a sidecar would not preserve Linux mutex semantics.

use core::{
    ffi::c_ulong,
    mem::{align_of, size_of},
    sync::atomic::{AtomicU32, Ordering},
};

use crate::{
    kernel_services::ProviderSlot,
    types::{PinCookie, Spinlock},
};

/// Task/preemption and local-IRQ operations required by Linux raw spinlocks.
///
/// The ArceOS runtime installs these callbacks once during initialization.
/// Provider callbacks must preserve nesting and restore the saved IRQ state
/// exactly; LinuxKPI does not emulate scheduler or interrupt state.
pub struct LockContextOps {
    pub preempt_disable: fn(),
    pub preempt_enable: fn(),
    pub irq_save_disable: fn() -> c_ulong,
    /// # Safety
    /// `flags` must be returned by the matching save callback on this CPU.
    pub irq_restore: unsafe fn(flags: c_ulong),
    pub irq_disable: fn(),
    pub irq_enable: fn(),
    pub irqs_disabled: fn() -> bool,
}

static LOCK_CONTEXT: ProviderSlot<LockContextOps> = ProviderSlot::new();

/// Install the native lock-context provider.
pub fn install_lock_context_ops(ops: &'static LockContextOps) -> Result<(), &'static str> {
    LOCK_CONTEXT.install(ops)
}

fn lock_context() -> &'static LockContextOps {
    #[cfg(test)]
    {
        static TEST_OPS: LockContextOps = LockContextOps {
            preempt_disable: || {},
            preempt_enable: || {},
            irq_save_disable: || 0,
            irq_restore: |_| {},
            irq_disable: || {},
            irq_enable: || {},
            irqs_disabled: || false,
        };
        let _ = LOCK_CONTEXT.install(&TEST_OPS);
    }
    LOCK_CONTEXT.require("lock context")
}

/// RAII preemption guard used by timers, wait/wound locks and deferred work.
pub struct NoPreempt;

impl NoPreempt {
    pub fn new() -> Self {
        preempt_disable();
        Self
    }
}

impl Default for NoPreempt {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NoPreempt {
    fn drop(&mut self) {
        preempt_enable();
    }
}

/// RAII guard that disables preemption and saves/disables local IRQs.
pub struct NoPreemptIrqSave(c_ulong);

impl NoPreemptIrqSave {
    pub fn new() -> Self {
        let flags = irq_save_flags();
        preempt_disable();
        Self(flags)
    }
}

impl Default for NoPreemptIrqSave {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NoPreemptIrqSave {
    fn drop(&mut self) {
        irq_restore_flags(self.0);
        preempt_enable();
    }
}

/// `lockdep_pin_lock()` is a no-op in the target's CONFIG_LOCKDEP=n build.
#[inline]
pub fn lockdep_pin_lock<T>(_lock: &mut T) -> PinCookie {
    PinCookie
}

/// `lockdep_unpin_lock()` is a no-op in the target's CONFIG_LOCKDEP=n build.
#[inline]
pub fn lockdep_unpin_lock<T>(_lock: &mut T, _cookie: PinCookie) {}

/// `lockdep_set_class_and_name()` compiles away with the source CONFIG_LOCKDEP=n.
#[inline]
pub fn lockdep_set_class_and_name<L, K, N>(_lock: *mut L, _key: *const K, _name: *const N) {}

// The Linux internal lock storage is one aligned 32-bit atomic word. This is
// not a user ABI; it is architecture-neutral across supported 64-bit targets.
const _: [(); size_of::<AtomicU32>()] = [(); size_of::<Spinlock>()];
const _: [(); align_of::<AtomicU32>()] = [(); align_of::<Spinlock>()];

#[inline]
fn irq_save_flags() -> c_ulong {
    (lock_context().irq_save_disable)()
}

#[inline]
fn irq_restore_flags(flags: c_ulong) {
    // SAFETY: `flags` was returned by a matching prior `irq_save_disable()` call on the
    // current CPU, and `irq_restore` is a valid function pointer registered in `lock_context`.
    unsafe { (lock_context().irq_restore)(flags) };
}

#[inline]
pub fn preempt_disable() {
    (lock_context().preempt_disable)();
}

#[inline]
pub fn preempt_enable() {
    (lock_context().preempt_enable)();
}

/// # Safety
///
/// `lock` must point to live, properly aligned and initialized `Spinlock` storage.
#[inline]
unsafe fn raw_lock(lock: *mut Spinlock) {
    // SAFETY: forwarded from raw_lock's caller. The lock's exact size and
    // alignment are asserted above; initialization writes a valid unlocked
    // word, and only this atomic view accesses it after initialization.
    let word = unsafe { AtomicU32::from_ptr(lock.cast::<u32>()) };
    loop {
        if word
            .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        // Test-and-test-and-set: the Acquire CAS is the synchronization point;
        // this poll only waits for the holder's Release store before retrying.
        // sync-lint: ignore suspicious_relaxed_wait_condition
        // sync-lint: ignore suspicious_relaxed_mixed_ordering
        while word.load(Ordering::Relaxed) != 0 {
            core::hint::spin_loop();
        }
    }
}

/// # Safety
///
/// `lock` must point to live, properly aligned and initialized `Spinlock` storage.
#[inline]
unsafe fn raw_try_lock(lock: *mut Spinlock) -> bool {
    // SAFETY: forwarded from raw_try_lock's caller; see raw_lock.
    unsafe { AtomicU32::from_ptr(lock.cast::<u32>()) }
        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// # Safety
///
/// `lock` must point to live, properly aligned and initialized `Spinlock` storage held by the caller.
#[inline]
unsafe fn raw_unlock(lock: *mut Spinlock) {
    // SAFETY: forwarded from raw_unlock's caller; see raw_lock.
    unsafe { AtomicU32::from_ptr(lock.cast::<u32>()) }.store(0, Ordering::Release);
}

#[inline]
fn lock_ptr(lock: &mut Spinlock) -> *mut Spinlock {
    lock as *mut Spinlock
}

/// Initialize a Linux x86 raw spin lock before sharing it with other CPUs.
///
/// This corresponds to Linux `spin_lock_init()`: the four-byte raw lock word
/// is reset to its unlocked state. The caller must ensure no CPU currently
/// holds or is attempting to acquire this lock.
#[inline]
pub fn spin_lock_init(lock: &mut Spinlock) {
    // SAFETY: a mutable reference proves this storage is live and exclusively
    // borrowed during initialization; the compile-time assertions prove the
    // zeroing covers exactly the lock's ABI-sized storage.
    unsafe {
        core::ptr::write_bytes(lock_ptr(lock).cast::<u8>(), 0, size_of::<Spinlock>());
    }
}

/// Acquire a Linux-style spin lock, disabling preemption for the hold interval.
#[inline]
pub fn spin_lock(lock: &mut Spinlock) {
    preempt_disable();
    // SAFETY: the reference is live and correctly aligned; callers initialize
    // each lock before use and do not access its storage non-atomically.
    unsafe { raw_lock(lock_ptr(lock)) };
}

/// Release a Linux-style spin lock and re-enable preemption.
#[inline]
pub fn spin_unlock(lock: &mut Spinlock) {
    // SAFETY: the reference is live and correctly aligned; caller must own the
    // lock, matching Linux spin_unlock's contract.
    unsafe { raw_unlock(lock_ptr(lock)) };
    preempt_enable();
}

/// Acquire a spin lock with local IRQs disabled, preserving their prior state.
#[inline]
pub fn spin_lock_irqsave(lock: &mut Spinlock, flags: &mut c_ulong) {
    // Linux v7.2's x86 generic raw-spin path orders these as irq-save then
    // preempt-disable, followed by the atomic lock acquisition.
    let saved = irq_save_flags();
    preempt_disable();
    // SAFETY: the reference is live and correctly aligned; see spin_lock.
    unsafe { raw_lock(lock_ptr(lock)) };
    *flags = saved;
}

/// Release a spin lock and restore the exact incoming local IRQ state.
#[inline]
pub fn spin_unlock_irqrestore(lock: &mut Spinlock, flags: c_ulong) {
    // Linux v7.2 restores IRQ state after unlocking and before preempt-enable.
    // SAFETY: the reference is live and correctly aligned; caller must own the
    // lock and pass the flags returned by the matching irqsave call.
    unsafe { raw_unlock(lock_ptr(lock)) };
    irq_restore_flags(flags);
    preempt_enable();
}

/// Acquire a spin lock after unconditionally disabling local IRQs.
#[inline]
pub fn spin_lock_irq(lock: &mut Spinlock) {
    local_irq_disable();
    preempt_disable();
    // SAFETY: the reference is live and correctly aligned; see spin_lock.
    unsafe { raw_lock(lock_ptr(lock)) };
}

/// Release a spin lock and unconditionally enable local IRQs.
///
/// Like Linux `spin_unlock_irq`, this does not restore a saved IRQ state; the
/// matching caller contract requires IRQs to be enabled at the end.
#[inline]
pub fn spin_unlock_irq(lock: &mut Spinlock) {
    // SAFETY: the reference is live and correctly aligned; caller must own the
    // lock, matching Linux spin_unlock_irq's contract.
    unsafe { raw_unlock(lock_ptr(lock)) };
    local_irq_enable();
    preempt_enable();
}

/// Try to acquire a spin lock without disabling local IRQs.
#[inline]
pub fn spin_trylock(lock: &mut Spinlock) -> bool {
    preempt_disable();
    // SAFETY: the reference is live and correctly aligned; see spin_lock.
    if unsafe { raw_try_lock(lock_ptr(lock)) } {
        true
    } else {
        preempt_enable();
        false
    }
}

/// Try to acquire a spin lock with IRQ-save semantics.
///
/// On failure, the IRQ state and preemption nesting are restored before
/// returning `false`, matching Linux `spin_trylock_irqsave`.
#[inline]
pub fn spin_trylock_irqsave(lock: &mut Spinlock, flags: &mut c_ulong) -> bool {
    let saved = irq_save_flags();
    preempt_disable();
    // SAFETY: the reference is live and correctly aligned; see spin_lock.
    if unsafe { raw_try_lock(lock_ptr(lock)) } {
        *flags = saved;
        true
    } else {
        preempt_enable();
        irq_restore_flags(saved);
        false
    }
}

/// Disable local interrupts without changing their saved state.
#[inline]
pub fn local_irq_disable() {
    (lock_context().irq_disable)();
}

/// Enable local interrupts unconditionally, as Linux local_irq_enable does.
#[inline]
pub fn local_irq_enable() {
    (lock_context().irq_enable)();
}

/// Query x86's IF bit without changing interrupt state.
#[inline]
pub fn irqs_disabled() -> bool {
    (lock_context().irqs_disabled)()
}

/// Acquire a spin lock through a Linux-style raw pointer.
///
/// # Safety
/// `lock` must be a non-null, aligned pointer to a live lock initialized by
/// `spin_lock_init`; it must remain valid for the duration of the call.
#[inline]
pub unsafe fn spin_lock_raw(lock: *mut Spinlock) {
    preempt_disable();
    // SAFETY: required by this function's contract.
    unsafe { raw_lock(lock) };
}

/// Release a spin lock through a Linux-style raw pointer.
///
/// # Safety
/// `lock` must meet `spin_lock_raw`'s pointer requirements and be held by the
/// current execution context.
#[inline]
pub unsafe fn spin_unlock_raw(lock: *mut Spinlock) {
    // SAFETY: required by this function's contract.
    unsafe { raw_unlock(lock) };
    preempt_enable();
}

/// Acquire a spin lock with IRQ-save semantics through a raw pointer.
///
/// # Safety
/// `lock` must meet `spin_lock_raw`'s pointer requirements.
#[inline]
pub unsafe fn spin_lock_irqsave_raw(lock: *mut Spinlock, flags: &mut c_ulong) {
    let saved = irq_save_flags();
    preempt_disable();
    // SAFETY: required by this function's contract.
    unsafe { raw_lock(lock) };
    *flags = saved;
}

/// Release a spin lock with IRQ restoration through a raw pointer.
///
/// # Safety
/// `lock` must meet `spin_lock_raw`'s requirements, be held by the current
/// context, and `flags` must come from its matching irqsave operation.
#[inline]
pub unsafe fn spin_unlock_irqrestore_raw(lock: *mut Spinlock, flags: c_ulong) {
    // SAFETY: required by this function's contract.
    unsafe { raw_unlock(lock) };
    irq_restore_flags(flags);
    preempt_enable();
}

/// Acquire a spin lock with IRQ-disable semantics through a raw pointer.
///
/// # Safety
/// `lock` must meet `spin_lock_raw`'s pointer requirements.
#[inline]
pub unsafe fn spin_lock_irq_raw(lock: *mut Spinlock) {
    local_irq_disable();
    preempt_disable();
    // SAFETY: required by this function's contract.
    unsafe { raw_lock(lock) };
}

/// Release a spin lock and unconditionally enable IRQs through a raw pointer.
///
/// # Safety
/// `lock` must meet `spin_lock_raw`'s requirements and be held by the current
/// context. Callers must require IRQs to be enabled after unlock.
#[inline]
pub unsafe fn spin_unlock_irq_raw(lock: *mut Spinlock) {
    // SAFETY: required by this function's contract.
    unsafe { raw_unlock(lock) };
    local_irq_enable();
    preempt_enable();
}

/// Try to acquire a spin lock with IRQ-save semantics through a raw pointer.
///
/// # Safety
/// `lock` must meet `spin_lock_raw`'s pointer requirements.
#[inline]
pub unsafe fn spin_trylock_irqsave_raw(lock: *mut Spinlock, flags: &mut c_ulong) -> bool {
    let saved = irq_save_flags();
    preempt_disable();
    // SAFETY: required by this function's contract.
    if unsafe { raw_try_lock(lock) } {
        *flags = saved;
        true
    } else {
        preempt_enable();
        irq_restore_flags(saved);
        false
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::{
        cell::UnsafeCell,
        mem::MaybeUninit,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::sync::Arc;

    use super::*;

    struct SharedSpinlock(UnsafeCell<Spinlock>);

    // SAFETY: all accesses after initialization go through this module's raw
    // atomic lock functions; the storage itself is never read non-atomically.
    unsafe impl Sync for SharedSpinlock {}

    impl SharedSpinlock {
        fn new() -> Self {
            let mut storage = MaybeUninit::<Spinlock>::zeroed();
            // SAFETY: zero is a valid initialized byte pattern for the opaque
            // four-byte lock storage; initialization is exclusive here.
            unsafe { spin_lock_init(storage.assume_init_mut()) };
            // SAFETY: spin_lock_init initialized every byte of the storage.
            Self(UnsafeCell::new(unsafe { storage.assume_init() }))
        }

        fn ptr(&self) -> *mut Spinlock {
            self.0.get()
        }
    }

    #[test]
    fn spin_lock_is_mutually_exclusive_across_threads() {
        const THREADS: usize = 6;
        const ITERATIONS: usize = 4_000;

        let lock = Arc::new(SharedSpinlock::new());
        let inside = Arc::new(AtomicBool::new(false));
        let overlap = Arc::new(AtomicUsize::new(0));
        let counter = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();

        for _ in 0..THREADS {
            let lock = Arc::clone(&lock);
            let inside = Arc::clone(&inside);
            let overlap = Arc::clone(&overlap);
            let counter = Arc::clone(&counter);
            workers.push(std::thread::spawn(move || {
                for _ in 0..ITERATIONS {
                    // SAFETY: SharedSpinlock initializes once and only exposes
                    // this valid pointer to the actual lock operations.
                    unsafe { spin_lock_raw(lock.ptr()) };
                    if inside.swap(true, Ordering::SeqCst) {
                        overlap.fetch_add(1, Ordering::Relaxed);
                    }
                    counter.fetch_add(1, Ordering::Relaxed);
                    inside.store(false, Ordering::SeqCst);
                    // SAFETY: this thread holds the lock acquired above.
                    unsafe { spin_unlock_raw(lock.ptr()) };
                }
            }));
        }

        for worker in workers {
            worker.join().expect("spinlock worker panicked");
        }
        assert_eq!(overlap.load(Ordering::Relaxed), 0);
        assert_eq!(counter.load(Ordering::Relaxed), THREADS * ITERATIONS);
    }

    #[test]
    fn irqsave_trylock_failure_restores_host_context() {
        let mut storage = MaybeUninit::<Spinlock>::zeroed();
        // SAFETY: zeroed bytes are valid storage and no concurrent users exist.
        unsafe { spin_lock_init(storage.assume_init_mut()) };
        // SAFETY: initialized above and local to this test.
        let mut lock = unsafe { storage.assume_init() };
        let mut flags: c_ulong = 0;

        assert!(spin_trylock_irqsave(&mut lock, &mut flags));
        assert!(!spin_trylock_irqsave(&mut lock, &mut flags));
        spin_unlock_irqrestore(&mut lock, flags);
    }
}

/// Native reader/writer lock in the four-byte, zero-initialized rwlock storage.
/// All LinuxKPI users of that storage use this implementation, never mix it
/// with Linux qrwlock instructions. Reader guards retain native preemption
/// exclusion until read_unlock, as required for DRM lookup atomic sections.
///
/// # Safety
/// `lock` must point to a live, properly aligned rwlock word initialized to
/// the unlocked state and used exclusively with this native implementation.
pub unsafe fn read_lock(lock: *mut core::sync::atomic::AtomicU32) {
    preempt_disable();
    loop {
        // SAFETY: `lock` points to a live, properly aligned AtomicU32 word guaranteed by caller.
        let old = unsafe { (*lock).load(Ordering::Relaxed) };
        if old & (1 << 31) == 0
            && old < (1 << 31) - 1
            && unsafe {
                // SAFETY: `lock` points to a live, properly aligned AtomicU32 word guaranteed by caller.
                (*lock).compare_exchange_weak(old, old + 1, Ordering::Acquire, Ordering::Relaxed)
            }
            .is_ok()
        {
            break;
        }
        core::hint::spin_loop();
    }
}
/// # Safety
/// `lock` must be a live rwlock word with a read lock held by this execution
/// context; each successful `read_lock` must be paired with one unlock.
pub unsafe fn read_unlock(lock: *mut core::sync::atomic::AtomicU32) {
    // SAFETY: `lock` points to a live, properly aligned AtomicU32 word currently read-locked by caller.
    let old = unsafe { (*lock).fetch_sub(1, Ordering::Release) };
    assert!(old != 0 && old & (1 << 31) == 0);
    preempt_enable();
}
/// # Safety
/// `lock` must point to a live, properly aligned rwlock word initialized to
/// the unlocked state and used exclusively with this native implementation.
pub unsafe fn write_lock(lock: *mut core::sync::atomic::AtomicU32) {
    preempt_disable();
    // SAFETY: `lock` points to a live, properly aligned AtomicU32 word guaranteed by caller.
    while unsafe { (*lock).compare_exchange_weak(0, 1 << 31, Ordering::Acquire, Ordering::Relaxed) }
        .is_err()
    {
        core::hint::spin_loop();
    }
}
/// # Safety
/// `lock` must be a live rwlock word exclusively held for writing by this
/// execution context from a matching `write_lock` call.
pub unsafe fn write_unlock(lock: *mut core::sync::atomic::AtomicU32) {
    // SAFETY: `lock` points to a live, properly aligned AtomicU32 word exclusively write-locked by caller.
    assert_eq!(unsafe { (*lock).swap(0, Ordering::Release) }, 1 << 31);
    preempt_enable();
}

// ---------------------------------------------------------------------------
// C ABI entry points used by the source-order DRM translations.
//
// Lock-debugging subclasses are ignored because CONFIG_LOCKDEP is disabled, so
// the `_nested` variants are the plain operations. Rust helpers that already
// carry the same name keep their identifiers; the C symbol is attached with
// `export_name`.
// ---------------------------------------------------------------------------

/// Linux `spin_unlock()`.
///
/// # Safety
/// `lock` must point to an initialized spinlock currently held by this
/// execution context.
#[unsafe(export_name = "spin_unlock")]
pub unsafe extern "C" fn c_spin_unlock(lock: *mut Spinlock) {
    // SAFETY: caller guarantees `lock` points to an initialized spinlock held by this context.
    unsafe { spin_unlock_raw(lock) };
}

/// Linux `spin_lock_nested()`; the subclass only feeds lockdep.
///
/// # Safety
/// `lock` must point to live, initialized spinlock storage not held by this
/// execution context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spin_lock_nested(lock: *mut Spinlock, _subclass: u32) {
    // SAFETY: caller guarantees `lock` points to live, initialized spinlock storage not held by this context.
    unsafe { spin_lock_raw(lock) };
}

/// Linux `spin_lock_irq()`.
///
/// # Safety
/// `lock` must point to live, initialized spinlock storage not held by this
/// execution context.
#[unsafe(export_name = "spin_lock_irq")]
pub unsafe extern "C" fn c_spin_lock_irq(lock: *mut Spinlock) {
    // SAFETY: caller guarantees `lock` points to live, initialized spinlock storage not held by this context.
    unsafe { spin_lock_irq_raw(lock) };
}

/// Linux `spin_unlock_irq()`.
///
/// # Safety
/// `lock` must point to an initialized spinlock held with IRQ state matching
/// the paired `spin_lock_irq()` call.
#[unsafe(export_name = "spin_unlock_irq")]
pub unsafe extern "C" fn c_spin_unlock_irq(lock: *mut Spinlock) {
    // SAFETY: caller guarantees `lock` points to an initialized spinlock held with matching IRQ state.
    unsafe { spin_unlock_irq_raw(lock) };
}

/// Linux `spin_lock_irqsave()`.
///
/// # Safety
/// `lock` must point to live initialized spinlock storage and `flags` to
/// writable storage for the saved local interrupt state.
#[unsafe(export_name = "spin_lock_irqsave")]
pub unsafe extern "C" fn c_spin_lock_irqsave(lock: *mut Spinlock, flags: *mut c_ulong) {
    // SAFETY: caller guarantees `lock` is valid and `flags` points to writable storage.
    unsafe { spin_lock_irqsave_raw(lock, &mut *flags) };
}

/// Linux `spin_lock_irqsave_nested()`; the subclass only feeds lockdep.
///
/// # Safety
/// `lock` must point to live initialized spinlock storage and `flags` to
/// writable storage for the saved local interrupt state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spin_lock_irqsave_nested(
    lock: *mut Spinlock,
    flags: *mut c_ulong,
    _subclass: u32,
) {
    // SAFETY: caller guarantees `lock` is valid and `flags` points to writable storage.
    unsafe { spin_lock_irqsave_raw(lock, &mut *flags) };
}

/// Linux `spin_unlock_irqrestore()`.
///
/// # Safety
/// `lock` must point to an initialized spinlock held with the local IRQ state
/// represented by `flags` saved from the matching lock operation.
#[unsafe(export_name = "spin_unlock_irqrestore")]
pub unsafe extern "C" fn c_spin_unlock_irqrestore(lock: *mut Spinlock, flags: c_ulong) {
    // SAFETY: caller guarantees `lock` is held with matching `flags` saved from locking.
    unsafe { spin_unlock_irqrestore_raw(lock, flags) };
}

/// Linux `cpu_relax()`: a spin-wait hint with no architectural side effects.
#[unsafe(no_mangle)]
pub extern "C" fn cpu_relax() {
    core::hint::spin_loop();
}

/// Linux `__cond_resched_lock()`: when a reschedule is pending, drop the
/// spinlock, yield, retake the lock, and return 1; otherwise return 0.
///
/// # Safety
/// `lock` must point to a live initialized spinlock held by this execution
/// context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __cond_resched_lock(lock: *mut Spinlock) -> i32 {
    if !crate::wait::need_resched() {
        return 0;
    }
    // SAFETY: caller guarantees `lock` points to an initialized spinlock held by this context.
    unsafe { spin_unlock_raw(lock) };
    crate::wait::cond_resched();
    // SAFETY: retaking the spinlock whose pointer remains valid as guaranteed by caller contract.
    unsafe { spin_lock_raw(lock) };
    1
}
