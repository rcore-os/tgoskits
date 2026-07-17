use ax_kspin::{LockRuntime, LockdepEvent, impl_trait};
use axdevice_base::{AccessWidth, BaseDeviceOps};
use axvm_types::GuestPhysAddr;
use riscv_vplic::{
    ForwardedBatchError, PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET, PLIC_CONTEXT_CTRL_OFFSET,
    PLIC_CONTEXT_STRIDE, PLIC_ENABLE_OFFSET, PLIC_ENABLE_STRIDE, PLIC_NUM_SOURCES,
    PLIC_PENDING_OFFSET, PLIC_PRIORITY_OFFSET, VPlicGlobal, VplicError,
};

const HOST_PLIC_BASE: usize = 0x0c00_0000;
const HOST_PLIC_SIZE: usize = 0x40_0000;

struct TestLockRuntime;

impl_trait! {
    impl LockRuntime for TestLockRuntime {
        fn irq_enter() {}
        fn irq_exit() {}
        fn preempt_enter() {}
        fn preempt_exit() {}
        unsafe fn preempt_exit_irq_return() {}
        fn current_thread_id() -> u64 { 1 }
        fn lockdep_acquire(_event: LockdepEvent) {}
        fn lockdep_release(_event: LockdepEvent) {}
        fn lockdep_set_trace_enabled(_enabled: bool) {}
        fn lockdep_dump_trace() {}
    }
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

    let vplic = VPlicGlobal::new(addr, Some(size), contexts_num).unwrap();

    assert_eq!(vplic.addr, addr);
    assert_eq!(vplic.size, size);
    assert_eq!(vplic.contexts_num, contexts_num);
}

#[test]
fn test_vplic_global_with_different_contexts() {
    let addr = GuestPhysAddr::from(0x0c000000);

    // Test with 1 context
    let vplic = VPlicGlobal::new(addr, Some(0x400000), 1).unwrap();
    assert_eq!(vplic.contexts_num, 1);

    // Test with 4 contexts
    let vplic = VPlicGlobal::new(addr, Some(0x400000), 4).unwrap();
    assert_eq!(vplic.contexts_num, 4);

    // Test with 8 contexts
    let vplic = VPlicGlobal::new(addr, Some(0x400000), 8).unwrap();
    assert_eq!(vplic.contexts_num, 8);
}

#[test]
fn test_vplic_global_missing_size_returns_typed_error() {
    let addr = GuestPhysAddr::from(0x0c000000);
    assert!(matches!(
        VPlicGlobal::new(addr, None, 2),
        Err(VplicError::MissingRegionSize)
    ));
}

#[test]
fn test_vplic_global_insufficient_size_returns_typed_error() {
    let addr = GuestPhysAddr::from(0x0c000000);
    assert!(matches!(
        VPlicGlobal::new(addr, Some(0x1000), 2),
        Err(VplicError::InsufficientRegion { .. })
    ));
}

#[test]
fn test_vplic_global_bitmaps_initialized_empty() {
    let addr = GuestPhysAddr::from(0x0c000000);
    let vplic = VPlicGlobal::new(addr, Some(0x400000), 2).unwrap();

    assert!(vplic.assigned_irqs.lock().is_empty());
    assert!(vplic.pending_irqs.lock().is_empty());
    assert!(vplic.active_irqs.lock().is_empty());
}

#[test]
fn test_typed_pending_api_is_visible_through_mmio() {
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE), 2).unwrap();

    vplic.set_pending(33).unwrap();

    assert!(vplic.is_pending(33).unwrap());
    assert_eq!(
        vplic
            .handle_read(addr + PLIC_PENDING_OFFSET + 4, AccessWidth::Dword)
            .unwrap(),
        1 << 1
    );

    vplic.clear_pending(33).unwrap();
    assert!(!vplic.is_pending(33).unwrap());
}

#[test]
fn guest_write_to_read_only_pending_register_has_no_effect() {
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE), 2).unwrap();

    vplic
        .handle_write(addr + PLIC_PENDING_OFFSET, AccessWidth::Dword, 1 << 10)
        .unwrap();

    assert!(!vplic.is_pending(10).unwrap());
}

#[test]
fn misaligned_context_register_access_returns_an_error_without_underflow() {
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE), 2).unwrap();
    let misaligned = addr + PLIC_CONTEXT_CTRL_OFFSET + 1;

    assert!(vplic.handle_read(misaligned, AccessWidth::Dword).is_err());
    assert!(
        vplic
            .handle_write(misaligned, AccessWidth::Dword, 0)
            .is_err()
    );
}

#[test]
fn test_pending_api_rejects_reserved_unassigned_and_out_of_range_sources() {
    let vplic =
        VPlicGlobal::new(GuestPhysAddr::from(HOST_PLIC_BASE), Some(HOST_PLIC_SIZE), 2).unwrap();

    assert_eq!(
        vplic.set_pending(0),
        Err(VplicError::InvalidSource {
            source_id: 0,
            max: PLIC_NUM_SOURCES,
        })
    );
    assert_eq!(
        vplic.set_pending(PLIC_NUM_SOURCES),
        Err(VplicError::InvalidSource {
            source_id: PLIC_NUM_SOURCES,
            max: PLIC_NUM_SOURCES,
        })
    );

    vplic.assigned_irqs.lock().set(5, true);
    assert_eq!(
        vplic.set_pending(6),
        Err(VplicError::SourceNotAssigned { source_id: 6 })
    );
    assert_eq!(vplic.set_pending(5), Ok(()));
}

#[test]
fn test_claim_and_complete_move_irq_between_pending_and_active() {
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE), 2).unwrap();
    let irq_id = 7;
    let context_id = 1;

    vplic
        .handle_write(
            addr + PLIC_PRIORITY_OFFSET + irq_id * 4,
            AccessWidth::Dword,
            1,
        )
        .unwrap();
    vplic
        .handle_write(
            addr + PLIC_ENABLE_OFFSET + context_id * PLIC_ENABLE_STRIDE,
            AccessWidth::Dword,
            1 << irq_id,
        )
        .unwrap();
    vplic.set_pending(irq_id).unwrap();

    let claim_addr = addr
        + PLIC_CONTEXT_CTRL_OFFSET
        + context_id * PLIC_CONTEXT_STRIDE
        + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET;
    assert_eq!(
        vplic.handle_read(claim_addr, AccessWidth::Dword).unwrap(),
        irq_id
    );
    assert!(!vplic.is_pending(irq_id).unwrap());
    assert!(vplic.active_irqs.lock().get(irq_id));

    vplic
        .handle_write(claim_addr, AccessWidth::Dword, irq_id)
        .unwrap();
    assert!(!vplic.active_irqs.lock().get(irq_id));
}

#[test]
fn uart_source_10_round_trips_through_guest_context_1_exactly_once() {
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE), 2).unwrap();
    let irq_id = 10;
    let context_id = 1;
    let threshold_addr = addr + PLIC_CONTEXT_CTRL_OFFSET + context_id * PLIC_CONTEXT_STRIDE;
    let claim_addr = addr
        + PLIC_CONTEXT_CTRL_OFFSET
        + context_id * PLIC_CONTEXT_STRIDE
        + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET;

    vplic
        .handle_write(
            addr + PLIC_PRIORITY_OFFSET + irq_id * 4,
            AccessWidth::Dword,
            1,
        )
        .unwrap();
    vplic
        .handle_write(
            addr + PLIC_ENABLE_OFFSET + context_id * PLIC_ENABLE_STRIDE,
            AccessWidth::Dword,
            1 << irq_id,
        )
        .unwrap();
    vplic
        .handle_write(threshold_addr, AccessWidth::Dword, 0)
        .unwrap();
    assert_eq!(
        vplic
            .handle_read(addr + PLIC_PRIORITY_OFFSET + irq_id * 4, AccessWidth::Dword,)
            .unwrap(),
        1
    );
    assert_eq!(
        vplic
            .handle_read(
                addr + PLIC_ENABLE_OFFSET + context_id * PLIC_ENABLE_STRIDE,
                AccessWidth::Dword,
            )
            .unwrap(),
        1 << irq_id
    );
    assert_eq!(
        vplic
            .handle_read(threshold_addr, AccessWidth::Dword)
            .unwrap(),
        0
    );
    vplic.set_forwarded_pending(irq_id).unwrap();
    assert_eq!(
        vplic.take_context_notification(context_id).unwrap(),
        Some(true)
    );

    assert_eq!(
        vplic.handle_read(claim_addr, AccessWidth::Dword).unwrap(),
        irq_id
    );
    assert_eq!(
        vplic.take_context_notification(context_id).unwrap(),
        Some(false)
    );
    assert_eq!(vplic.take_completed_forwarded_irq(), None);

    vplic
        .handle_write(claim_addr, AccessWidth::Dword, irq_id)
        .unwrap();
    assert_eq!(vplic.take_completed_forwarded_irq(), Some(irq_id));
    assert_eq!(vplic.take_completed_forwarded_irq(), None);
}

#[test]
fn test_virtual_plic_instances_and_guest_addresses_are_independent() {
    let first =
        VPlicGlobal::new(GuestPhysAddr::from(0x0c00_0000), Some(HOST_PLIC_SIZE), 2).unwrap();
    let second =
        VPlicGlobal::new(GuestPhysAddr::from(0x1c00_0000), Some(HOST_PLIC_SIZE), 2).unwrap();

    first.set_pending(11).unwrap();

    assert!(first.is_pending(11).unwrap());
    assert!(!second.is_pending(11).unwrap());
}

#[test]
fn forwarded_batch_is_atomic_and_rejects_pending_or_active_collisions() {
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE), 2).unwrap();
    let context_id = 1;
    let first = 10;
    let second = 11;

    for irq_id in [first, second] {
        vplic
            .handle_write(
                addr + PLIC_PRIORITY_OFFSET + irq_id * 4,
                AccessWidth::Dword,
                1,
            )
            .unwrap();
    }
    vplic
        .handle_write(
            addr + PLIC_ENABLE_OFFSET + context_id * PLIC_ENABLE_STRIDE,
            AccessWidth::Dword,
            (1 << first) | (1 << second),
        )
        .unwrap();

    vplic.set_pending(first).unwrap();
    assert_eq!(
        vplic.set_forwarded_pending_batch(&[first, second]),
        Err(ForwardedBatchError::Rejected(
            VplicError::ForwardedSourceCollision { source_id: first }
        ))
    );
    assert!(vplic.is_pending(first).unwrap());
    assert!(!vplic.is_pending(second).unwrap());

    vplic.clear_pending(first).unwrap();
    vplic.set_forwarded_pending_batch(&[first, second]).unwrap();
    assert!(vplic.is_pending(first).unwrap());
    assert!(vplic.is_pending(second).unwrap());
    assert_eq!(
        vplic.take_context_notification(context_id).unwrap(),
        Some(true)
    );
    assert_eq!(
        vplic.set_forwarded_pending_batch(&[first]),
        Err(ForwardedBatchError::Rejected(
            VplicError::ForwardedSourceBusy { source_id: first }
        ))
    );
}

#[test]
fn forwarded_route_revocation_is_generation_checked_and_reusable() {
    let vplic =
        VPlicGlobal::new(GuestPhysAddr::from(HOST_PLIC_BASE), Some(HOST_PLIC_SIZE), 2).unwrap();
    let source = 37;

    vplic
        .set_forwarded_pending_batch_for_generation(&[source], 7)
        .unwrap();
    assert!(matches!(
        vplic.revoke_forwarded_route_batch(8, &[source]),
        Err(VplicError::ForwardedGenerationMismatch {
            source_id: 37,
            expected: 8,
            actual: 7,
        })
    ));
    assert!(vplic.is_pending(source).unwrap());

    assert_eq!(vplic.revoke_forwarded_route_batch(7, &[source]).unwrap(), 1);
    assert!(!vplic.is_pending(source).unwrap());
    vplic
        .set_forwarded_pending_batch_for_generation(&[source], 9)
        .unwrap();
}

#[test]
fn test_context_lines_remain_independent_after_another_context_claims() {
    const ISOLATED_OFFSET: usize = 0x10_0000;
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE + ISOLATED_OFFSET);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE - ISOLATED_OFFSET), 4).unwrap();
    let first_irq = 41;
    let second_irq = 42;

    for irq_id in [first_irq, second_irq] {
        vplic
            .handle_write(
                addr + PLIC_PRIORITY_OFFSET + irq_id * 4,
                AccessWidth::Dword,
                2,
            )
            .unwrap();
    }
    for (context_id, irq_id) in [(1, first_irq), (3, second_irq)] {
        vplic
            .handle_write(
                addr + PLIC_ENABLE_OFFSET + context_id * PLIC_ENABLE_STRIDE + irq_id / 32 * 4,
                AccessWidth::Dword,
                1 << (irq_id % 32),
            )
            .unwrap();
    }

    vplic.set_pending(first_irq).unwrap();
    vplic.set_pending(second_irq).unwrap();
    assert_eq!(vplic.take_context_notification(1).unwrap(), Some(true));
    assert_eq!(vplic.take_context_notification(3).unwrap(), Some(true));

    let first_claim =
        addr + PLIC_CONTEXT_CTRL_OFFSET + PLIC_CONTEXT_STRIDE + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET;
    assert_eq!(
        vplic.handle_read(first_claim, AccessWidth::Dword).unwrap(),
        first_irq
    );

    assert!(!vplic.context_line_asserted(1).unwrap());
    assert!(vplic.context_line_asserted(3).unwrap());
    assert_eq!(vplic.take_context_notification(1).unwrap(), Some(false));
    assert_eq!(vplic.take_context_notification(3).unwrap(), None);
}
