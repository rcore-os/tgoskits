use alloc::sync::Arc;
use core::cell::UnsafeCell;

use spin::{Mutex, RwLock};

use super::reg::{DisableIrqGuard, XhciRegisters};

pub(crate) struct IrqLock<T> {
    inner: Mutex<()>,
    reg: Arc<RwLock<XhciRegisters>>,
    data: UnsafeCell<T>,
}

unsafe impl<T> Sync for IrqLock<T> where T: Send {}
unsafe impl<T> Send for IrqLock<T> where T: Send {}

impl<T> IrqLock<T> {
    pub fn new(data: T, reg: Arc<RwLock<XhciRegisters>>) -> Self {
        Self {
            inner: Mutex::new(()),
            reg,
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> IrqLockGuard<'_, T> {
        let _disable_guard = self.reg.write().disable_irq_guard();
        let guard = self.inner.lock();
        IrqLockGuard {
            _guard: guard,
            data: unsafe { &mut *self.data.get() },
            _disable_guard,
        }
    }

    #[allow(clippy::mut_from_ref)]
    pub unsafe fn force_use(&self) -> &mut T {
        unsafe { &mut *self.data.get() }
    }
}

pub(crate) struct IrqLockGuard<'a, T> {
    _guard: spin::MutexGuard<'a, ()>,
    data: &'a mut T,
    _disable_guard: DisableIrqGuard,
}

impl<'a, T> core::ops::Deref for IrqLockGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl<'a, T> core::ops::DerefMut for IrqLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data
    }
}
