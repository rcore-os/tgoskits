extern crate alloc;

use alloc::{sync::Arc, vec, vec::Vec};

use ax_errno::AxResult;
use axaddrspace::GuestPhysAddr;
use riscv_vplic::{
    PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET, PLIC_CONTEXT_CTRL_OFFSET, PLIC_CONTEXT_STRIDE, VPlicGlobal,
};
use vm_interrupt::{InterruptControllerRoute, VmInterruptRouter};

struct TestInterruptRouter;

impl VmInterruptRouter for TestInterruptRouter {
    fn route_interrupt(&self, _route: InterruptControllerRoute) -> AxResult {
        Ok(())
    }
}

fn test_interrupt_router() -> Arc<dyn VmInterruptRouter> {
    Arc::new(TestInterruptRouter)
}

fn test_context_routes(contexts_num: usize) -> Vec<Option<usize>> {
    let mut routes = vec![None; contexts_num];
    for vcpu_id in 0..contexts_num / 2 {
        routes[vcpu_id * 2 + 1] = Some(vcpu_id);
    }
    routes
}

/// Calculate minimum required size for VPlicGlobal with given contexts
fn calculate_min_size(contexts_num: usize) -> usize {
    contexts_num * PLIC_CONTEXT_STRIDE
        + PLIC_CONTEXT_CTRL_OFFSET
        + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET
        + 0x1000
}

#[test]
fn test_vplic_global_creation() {
    let addr = GuestPhysAddr::from(0x0c000000);
    let contexts_num = 2;
    let size = calculate_min_size(contexts_num);

    let vplic = VPlicGlobal::new(
        addr,
        Some(size),
        contexts_num,
        test_context_routes(contexts_num),
        test_interrupt_router(),
    );

    assert_eq!(vplic.addr, addr);
    assert_eq!(vplic.size, size);
    assert_eq!(vplic.contexts_num, contexts_num);
}

#[test]
fn test_vplic_global_with_different_contexts() {
    let addr = GuestPhysAddr::from(0x0c000000);

    // Test with 1 context
    let vplic = VPlicGlobal::new(
        addr,
        Some(0x400000),
        1,
        test_context_routes(1),
        test_interrupt_router(),
    );
    assert_eq!(vplic.contexts_num, 1);

    // Test with 4 contexts
    let vplic = VPlicGlobal::new(
        addr,
        Some(0x400000),
        4,
        test_context_routes(4),
        test_interrupt_router(),
    );
    assert_eq!(vplic.contexts_num, 4);

    // Test with 8 contexts
    let vplic = VPlicGlobal::new(
        addr,
        Some(0x400000),
        8,
        test_context_routes(8),
        test_interrupt_router(),
    );
    assert_eq!(vplic.contexts_num, 8);
}

#[test]
#[should_panic(expected = "Size must be specified")]
fn test_vplic_global_size_none_panics() {
    let addr = GuestPhysAddr::from(0x0c000000);
    let _ = VPlicGlobal::new(
        addr,
        None,
        2,
        test_context_routes(2),
        test_interrupt_router(),
    );
}

#[test]
#[should_panic(expected = "exceeds region")]
fn test_vplic_global_insufficient_size_panics() {
    let addr = GuestPhysAddr::from(0x0c000000);
    // Size too small for 2 contexts
    let _ = VPlicGlobal::new(
        addr,
        Some(0x1000),
        2,
        test_context_routes(2),
        test_interrupt_router(),
    );
}

#[test]
fn test_vplic_global_bitmaps_initialized_empty() {
    let addr = GuestPhysAddr::from(0x0c000000);
    let vplic = VPlicGlobal::new(
        addr,
        Some(0x400000),
        2,
        test_context_routes(2),
        test_interrupt_router(),
    );

    assert!(vplic.assigned_irqs.lock().is_empty());
    assert!(vplic.pending_irqs.lock().is_empty());
    assert!(vplic.active_irqs.lock().is_empty());
}
