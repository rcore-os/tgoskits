//! Allocation audit for the complete hard-IRQ dispatch path.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use irq_framework::{
    AutoEnable, CpuId, HwIrq, IrqAffinity, IrqDomainId, IrqError, IrqId, IrqLineBinding,
    IrqLineControl, IrqOps, IrqRequest, IrqReturn, IrqScope, PreparedIrqLine, Registry,
};

static HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);
static EOI_CALLS: AtomicUsize = AtomicUsize::new(0);

std::thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static DEALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn begin_allocation_audit() {
    ALLOCATIONS.set(0);
    DEALLOCATIONS.set(0);
    TRACK_ALLOCATIONS.set(true);
}

fn finish_allocation_audit() -> (usize, usize) {
    TRACK_ALLOCATIONS.set(false);
    (ALLOCATIONS.get(), DEALLOCATIONS.get())
}

struct AuditAllocator;

// SAFETY: every operation is forwarded to `System` with the original layout
// and pointer. The extra atomics only record operations during the bounded
// audit window and do not affect allocation ownership.
unsafe impl GlobalAlloc for AuditAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACK_ALLOCATIONS.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|allocations| allocations.set(allocations.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if TRACK_ALLOCATIONS.try_with(Cell::get).unwrap_or(false) {
            let _ =
                DEALLOCATIONS.try_with(|deallocations| deallocations.set(deallocations.get() + 1));
        }
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: AuditAllocator = AuditAllocator;

struct AuditOps {
    line_enabled: AtomicBool,
    audit_after_prepare: bool,
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

    fn prepare_line(
        &self,
        irq: IrqId,
        _scope: IrqScope,
        _affinity: IrqAffinity,
    ) -> Result<PreparedIrqLine, IrqError> {
        if self.audit_after_prepare {
            begin_allocation_audit();
        }
        Ok(PreparedIrqLine::new(
            IrqLineBinding::new(irq.hwirq.0, 1).unwrap(),
            IrqLineControl::Maskable,
        ))
    }

    fn set_line_enabled(&self, _binding: IrqLineBinding, _cpu: Option<CpuId>, enabled: bool) {
        self.line_enabled.store(enabled, Ordering::Release);
    }

    fn relax(&self) {
        core::hint::spin_loop();
    }
}

#[test]
fn dispatch_action_disable_line_mask_and_eoi_allocate_and_free_nothing() {
    HANDLER_CALLS.store(0, Ordering::Relaxed);
    EOI_CALLS.store(0, Ordering::Relaxed);
    let registry = Registry::new(AuditOps {
        line_enabled: AtomicBool::new(false),
        audit_after_prepare: false,
    });
    let irq = IrqId::new(IrqDomainId(1), HwIrq(1));
    let action_disable_irq = IrqId::new(IrqDomainId(1), HwIrq(2));
    let line_mask_irq = IrqId::new(IrqDomainId(1), HwIrq(3));
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
    let disabled_action = registry
        .request(
            action_disable_irq,
            IrqRequest::new(|_| {
                HANDLER_CALLS.fetch_add(1, Ordering::Relaxed);
                IrqReturn::DisableActionAndWake
            }),
        )
        .unwrap();
    registry.enable(disabled_action).unwrap();
    let masked_action = registry
        .request(
            line_mask_irq,
            IrqRequest::new(|_| {
                HANDLER_CALLS.fetch_add(1, Ordering::Relaxed);
                IrqReturn::MaskLineAndWake
            }),
        )
        .unwrap();
    registry.enable(masked_action).unwrap();

    begin_allocation_audit();
    let outcome = registry.dispatch(irq, CpuId(0), || {
        EOI_CALLS.fetch_add(1, Ordering::Relaxed);
    });
    let disable_outcome = registry.dispatch(action_disable_irq, CpuId(0), || {
        EOI_CALLS.fetch_add(1, Ordering::Relaxed);
    });
    let mask_outcome = registry.dispatch(line_mask_irq, CpuId(0), || {
        EOI_CALLS.fetch_add(1, Ordering::Relaxed);
    });
    let (allocations, deallocations) = finish_allocation_audit();

    assert!(outcome.handled);
    assert_eq!(outcome.called, 1);
    assert!(disable_outcome.handled);
    assert!(disable_outcome.wake);
    assert_eq!(disable_outcome.called, 1);
    assert!(mask_outcome.handled);
    assert!(mask_outcome.wake);
    assert_eq!(mask_outcome.called, 1);
    assert_eq!(HANDLER_CALLS.load(Ordering::Relaxed), 3);
    assert_eq!(EOI_CALLS.load(Ordering::Relaxed), 3);
    assert_eq!(allocations, 0);
    assert_eq!(deallocations, 0);

    registry.release_quench(masked_action).unwrap();
}

#[test]
fn registration_allocates_everything_before_preparing_the_irqchip_line() {
    TRACK_ALLOCATIONS.set(false);
    let registry = Registry::new(AuditOps {
        line_enabled: AtomicBool::new(false),
        audit_after_prepare: true,
    });
    let irq = IrqId::new(IrqDomainId(1), HwIrq(4));

    let action = registry
        .request(
            irq,
            IrqRequest::new(|_| IrqReturn::Handled).auto_enable(AutoEnable::Yes),
        )
        .unwrap();
    let (allocations, deallocations) = finish_allocation_audit();

    assert_eq!(allocations, 0);
    assert_eq!(deallocations, 0);
    assert!(registry.status(action).unwrap().action_enabled);
    registry.free(action).unwrap();
}
