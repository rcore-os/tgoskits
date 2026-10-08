//! Minimal spin mutex used by the OS-neutral x86 APIC device models.

use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) struct RawSpinLockStorage<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

pub(crate) struct RawSpinLockStorageGuard<'a, T> {
    lock: &'a RawSpinLockStorage<T>,
}

unsafe impl<T: Send> Send for RawSpinLockStorage<T> {}
unsafe impl<T: Send> Sync for RawSpinLockStorage<T> {}

impl<T> RawSpinLockStorage<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub(crate) fn lock(&self) -> RawSpinLockStorageGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        RawSpinLockStorageGuard { lock: self }
    }
}

impl<T> Deref for RawSpinLockStorageGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for RawSpinLockStorageGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for RawSpinLockStorageGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}
