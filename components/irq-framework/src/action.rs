use core::{
    cell::UnsafeCell,
    ptr::{self, NonNull},
    sync::atomic::AtomicBool,
};

use crate::{AutoEnable, CpuId, CpuMask, IrqExecution, IrqRequest, IrqScope, RawIrqHandler};

pub(crate) struct Action {
    pub(crate) id: u64,
    pub(crate) handler: RawIrqHandler,
    pub(crate) data: NonNull<()>,
    pub(crate) scope: IrqScope,
    pub(crate) execution: IrqExecution,
    pub(crate) enabled: AtomicBool,
    pub(crate) detached: AtomicBool,
    pub(crate) running: AtomicBool,
    pending_enable: UnsafeCell<CpuMask>,
    pub(crate) next: *mut Action,
}

// Raw handler context pointers are owned by the OS adapter. The framework only
// stores and passes them back to the registered handler.
unsafe impl Send for Action {}
unsafe impl Sync for Action {}

impl Action {
    pub(crate) fn new(id: u64, request: &IrqRequest) -> Self {
        Self {
            id,
            handler: request.handler,
            data: request.data,
            scope: request.scope,
            execution: request.execution,
            enabled: AtomicBool::new(request.auto_enable == AutoEnable::Yes),
            detached: AtomicBool::new(false),
            running: AtomicBool::new(false),
            pending_enable: UnsafeCell::new(CpuMask::empty()),
            next: ptr::null_mut(),
        }
    }

    pub(crate) fn pending_enable_contains(&self, cpu: CpuId) -> bool {
        unsafe { (&*self.pending_enable.get()).contains(cpu) }
    }

    pub(crate) fn insert_pending_enable(&self, cpu: CpuId) {
        unsafe { (&mut *self.pending_enable.get()).insert(cpu) };
    }

    pub(crate) fn remove_pending_enable(&self, cpu: CpuId) {
        unsafe { (&mut *self.pending_enable.get()).remove(cpu) };
    }

    pub(crate) fn clear_pending_enable_all(&self) {
        unsafe { *self.pending_enable.get() = CpuMask::empty() };
    }
}
