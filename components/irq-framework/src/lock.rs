use core::sync::atomic::{AtomicBool, Ordering};

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
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;
    use crate::{CpuId, IrqError};

    struct NonCopyIrqState;

    struct TestIrqOps {
        restored: Cell<bool>,
    }

    impl IrqOps for TestIrqOps {
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

        fn set_enabled(
            &self,
            _irq: crate::IrqId,
            _cpu: Option<CpuId>,
            _enabled: bool,
        ) -> Result<(), IrqError> {
            Ok(())
        }

        fn is_enabled(&self, _irq: crate::IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
            Ok(true)
        }

        fn is_pending(&self, _irq: crate::IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
            Ok(false)
        }

        fn is_in_service(&self, _irq: crate::IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
            Ok(false)
        }

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
