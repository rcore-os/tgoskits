use core::{cell::UnsafeCell, ptr, sync::atomic::AtomicBool};

use crate::{
    AutoEnable, BoxedIrqHandler, CpuId, CpuMask, IrqContext, IrqExecution, IrqRequest, IrqReturn,
    IrqScope, types::IrqHandler,
};

pub(crate) enum ActionHandler {
    Raw {
        handler: unsafe fn(IrqContext, core::ptr::NonNull<()>) -> IrqReturn,
        data: core::ptr::NonNull<()>,
    },
    Boxed(UnsafeCell<BoxedIrqHandler>),
}

unsafe impl Send for ActionHandler {}
unsafe impl Sync for ActionHandler {}

pub(crate) struct Action {
    pub(crate) id: u64,
    pub(crate) handler: ActionHandler,
    pub(crate) scope: IrqScope,
    pub(crate) execution: IrqExecution,
    pub(crate) enabled: AtomicBool,
    pub(crate) detached: AtomicBool,
    pub(crate) running: AtomicBool,
    pending_enable: UnsafeCell<CpuMask>,
    pub(crate) next: *mut Action,
}

// Raw handler context pointers and boxed callbacks are owned by the registered
// action. Boxed callbacks are only called after the NonReentrant run guard
// succeeds, so the UnsafeCell is not mutably aliased by framework dispatch.
unsafe impl Send for Action {}
unsafe impl Sync for Action {}

impl Action {
    pub(crate) fn new(id: u64, request: &mut IrqRequest) -> Self {
        let handler = match request
            .handler
            .take()
            .expect("IRQ handler was already consumed")
        {
            IrqHandler::Raw { handler, data } => ActionHandler::Raw { handler, data },
            IrqHandler::Boxed(handler) => ActionHandler::Boxed(UnsafeCell::new(handler)),
        };
        Self {
            id,
            handler,
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
