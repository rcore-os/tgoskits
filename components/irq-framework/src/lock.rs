use core::{
    marker::PhantomData,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::IrqOps;

pub(crate) struct MetadataLock {
    locked: AtomicBool,
}

impl MetadataLock {
    pub(crate) const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    pub(crate) fn lock<O: IrqOps>(&self, ops: &O) -> O::LocalIrqState {
        let state = ops.local_irq_save();
        while self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            ops.relax();
        }
        state
    }

    pub(crate) fn unlock<O: IrqOps>(&self, ops: &O, state: O::LocalIrqState) {
        self.locked.store(false, Ordering::Release);
        ops.local_irq_restore(state);
    }

    pub(crate) fn guard<'a, O: IrqOps>(&'a self, ops: &'a O) -> MetadataGuard<'a, O> {
        MetadataGuard {
            lock: self,
            ops,
            state: Some(self.lock(ops)),
            _not_send: PhantomData,
        }
    }
}

/// RAII ownership of an IRQ-disabled metadata or controller transition lock.
///
/// The marker prevents a saved local IRQ state from being restored on another
/// CPU. Drop is intentionally the only unlock path so an early `?` cannot
/// strand the lock or leave local interrupts disabled.
pub(crate) struct MetadataGuard<'a, O: IrqOps> {
    lock: &'a MetadataLock,
    ops: &'a O,
    state: Option<O::LocalIrqState>,
    _not_send: PhantomData<*mut ()>,
}

impl<O: IrqOps> Drop for MetadataGuard<'_, O> {
    fn drop(&mut self) {
        let state = self
            .state
            .take()
            .expect("metadata lock guard released twice");
        self.lock.unlock(self.ops, state);
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;
    use crate::{
        CpuId, IrqAffinity, IrqError, IrqLineBinding, IrqLineControl, IrqScope, PreparedIrqLine,
    };

    struct NonCopyIrqState;

    struct TestIrqOps {
        restored: Cell<bool>,
    }

    // SAFETY: This unit-test adapter executes no deferred CPU thunk, is used on
    // one thread, and restores every local IRQ token synchronously.
    unsafe impl IrqOps for TestIrqOps {
        type LocalIrqState = NonCopyIrqState;

        fn current_cpu(&self) -> CpuId {
            CpuId(0)
        }

        fn cpu_online(&self, _cpu: CpuId) -> bool {
            true
        }

        fn in_irq_context(&self) -> bool {
            false
        }

        fn local_irq_save(&self) -> Self::LocalIrqState {
            NonCopyIrqState
        }

        fn local_irq_restore(&self, _state: Self::LocalIrqState) {
            self.restored.set(true);
        }

        fn run_on_cpu_sync(
            &self,
            _cpu: CpuId,
            _f: unsafe fn(*mut ()),
            _arg: *mut (),
        ) -> Result<(), IrqError> {
            Ok(())
        }

        fn prepare_line(
            &self,
            irq: crate::IrqId,
            _scope: IrqScope,
            _affinity: IrqAffinity,
        ) -> Result<PreparedIrqLine, IrqError> {
            Ok(PreparedIrqLine::new(
                IrqLineBinding::new(irq.hwirq.0, 1).unwrap(),
                IrqLineControl::Maskable,
            ))
        }

        fn set_line_enabled(&self, _binding: IrqLineBinding, _cpu: Option<CpuId>, _enabled: bool) {}

        fn relax(&self) {
            core::hint::spin_loop();
        }
    }

    #[test]
    fn metadata_lock_accepts_a_non_copy_irq_guard() {
        let lock = MetadataLock::new();
        let ops = TestIrqOps {
            restored: Cell::new(false),
        };

        let state = lock.lock(&ops);
        lock.unlock(&ops, state);

        assert!(ops.restored.get());
    }
}
