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
