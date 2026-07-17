//! Allocation audit for the complete hard-IRQ dispatch path.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use irq_framework::{
    CpuId, HwIrq, IrqDomainId, IrqError, IrqId, IrqOps, IrqRequest, IrqReturn, Registry,
};

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);

struct AuditAllocator;

// SAFETY: every operation is forwarded to `System` with the original layout
// and pointer. The extra atomics only record operations during the bounded
// audit window and do not affect allocation ownership.
unsafe impl GlobalAlloc for AuditAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: AuditAllocator = AuditAllocator;

struct AuditOps {
    line_enabled: AtomicBool,
}

// SAFETY: CPU thunks run synchronously before return, the adapter never keeps
// their raw argument, and every operation is lock-free and allocation-free.
unsafe impl IrqOps for AuditOps {
    type LocalIrqState = ();

    fn current_cpu(&self) -> CpuId {
        CpuId(0)
    }

    fn cpu_online(&self, cpu: CpuId) -> bool {
        cpu == CpuId(0)
    }

    fn in_irq_context(&self) -> bool {
        false
    }

    fn local_irq_save(&self) -> Self::LocalIrqState {}

    fn local_irq_restore(&self, _state: Self::LocalIrqState) {}

    fn run_on_cpu_sync(
        &self,
        _cpu: CpuId,
        f: unsafe fn(*mut ()),
        arg: *mut (),
    ) -> Result<(), IrqError> {
        unsafe { f(arg) };
        Ok(())
    }

    fn set_enabled(&self, _irq: IrqId, _cpu: Option<CpuId>, enabled: bool) -> Result<(), IrqError> {
        self.line_enabled.store(enabled, Ordering::Release);
        Ok(())
    }

    fn is_enabled(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Ok(self.line_enabled.load(Ordering::Acquire))
    }

    fn is_pending(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Ok(false)
    }

    fn is_in_service(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Ok(false)
    }

    fn relax(&self) {
        core::hint::spin_loop();
    }
}

#[test]
fn dispatch_and_fail_closed_quench_allocate_and_free_nothing() {
    let registry = Registry::new(AuditOps {
        line_enabled: AtomicBool::new(false),
    });
    let irq = IrqId::new(IrqDomainId(1), HwIrq(1));
    let quench_irq = IrqId::new(IrqDomainId(1), HwIrq(2));
    let action = registry
        .request(
            irq,
            IrqRequest::new(|_| {
                HANDLER_CALLS.fetch_add(1, Ordering::Relaxed);
                IrqReturn::Handled
            }),
        )
        .unwrap();
    registry.enable(action).unwrap();
    let quench_action = registry
        .request(
            quench_irq,
            IrqRequest::new(|_| {
                HANDLER_CALLS.fetch_add(1, Ordering::Relaxed);
                IrqReturn::QuenchAndWake
            }),
        )
        .unwrap();
    registry.enable(quench_action).unwrap();

    ALLOCATIONS.store(0, Ordering::Relaxed);
    DEALLOCATIONS.store(0, Ordering::Relaxed);
    TRACK_ALLOCATIONS.store(true, Ordering::Release);
    let outcome = registry.dispatch(irq, CpuId(0));
    let quench_outcome = registry.dispatch(quench_irq, CpuId(0));
    TRACK_ALLOCATIONS.store(false, Ordering::Release);

    assert!(outcome.handled);
    assert_eq!(outcome.called, 1);
    assert!(quench_outcome.handled);
    assert!(quench_outcome.wake);
    assert_eq!(quench_outcome.called, 1);
    assert_eq!(HANDLER_CALLS.load(Ordering::Relaxed), 2);
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(DEALLOCATIONS.load(Ordering::Relaxed), 0);

    registry.release_quench(quench_action).unwrap();
}
