use alloc::sync::Arc;
use core::cell::UnsafeCell;

use spin::{
    RwLock,
    mutex::{SpinMutex, SpinMutexGuard},
};

use super::reg::{DisableIrqGuard, XhciRegisters};

pub(crate) struct IrqLock<T> {
    inner: SpinMutex<()>,
    reg: Arc<RwLock<XhciRegisters>>,
    data: UnsafeCell<T>,
}

unsafe impl<T> Sync for IrqLock<T> where T: Send {}
unsafe impl<T> Send for IrqLock<T> where T: Send {}

impl<T> IrqLock<T> {
    pub fn new(data: T, reg: Arc<RwLock<XhciRegisters>>) -> Self {
        Self {
            inner: SpinMutex::new(()),
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

    /// # Safety
    ///
    /// The caller must run from the xHCI interrupt/event path while no task
    /// side `lock()` guard is alive. Task-side mutation uses `lock()`, which
    /// disables the controller interrupter before taking the mutex. This
    /// method deliberately avoids taking the mutex so the interrupt hot path
    /// can only touch state that was pre-registered under that protocol.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn force_use(&self) -> &mut T {
        unsafe { &mut *self.data.get() }
    }
}

pub(crate) struct IrqLockGuard<'a, T> {
    _guard: SpinMutexGuard<'a, ()>,
    data: &'a mut T,
    _disable_guard: DisableIrqGuard,
}

impl<T> core::ops::Deref for IrqLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl<T> core::ops::DerefMut for IrqLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data
    }
}
