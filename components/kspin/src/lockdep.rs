use ax_kernel_guard::BaseGuard;

use crate::base::BaseSpinLock;

pub(crate) struct Lockdep {
    inner: ax_lockdep::Lockdep,
}

impl Lockdep {
    #[inline(always)]
    pub(crate) fn prepare<G: BaseGuard, T: ?Sized>(
        lock: &BaseSpinLock<G, T>,
        is_try: bool,
    ) -> Self {
        let addr = lock as *const _ as *const () as usize;
        Self {
            inner: ax_lockdep::Lockdep::prepare(
                "spin",
                addr,
                is_try,
                Some(core::any::type_name::<G>()),
            ),
        }
    }

    #[inline(always)]
    pub(crate) fn finish(&self, acquired: bool) {
        self.inner.finish(acquired);
    }

    #[inline(always)]
    pub(crate) fn lock_addr(&self) -> usize {
        self.inner.lock_addr()
    }
}

#[inline(always)]
pub(crate) fn release<G: BaseGuard>(addr: usize) {
    ax_lockdep::Lockdep::release("spin", addr, Some(core::any::type_name::<G>()));
}
