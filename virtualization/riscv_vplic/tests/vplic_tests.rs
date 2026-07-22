use axdevice_base::{AccessWidth, BaseDeviceOps};
use axvm_types::GuestPhysAddr;
use riscv_vplic::{
    PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET, PLIC_CONTEXT_CTRL_OFFSET, PLIC_CONTEXT_STRIDE,
    PLIC_ENABLE_OFFSET, PLIC_ENABLE_STRIDE, PLIC_NUM_SOURCES, PLIC_PENDING_OFFSET,
    PLIC_PRIORITY_OFFSET, VPlicGlobal, VplicError,
};

const HOST_PLIC_BASE: usize = 0x0c00_0000;
const HOST_PLIC_SIZE: usize = 0x40_0000;

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

    assert_eq!(vplic.address(), addr);
    assert_eq!(vplic.size(), size);
    assert_eq!(vplic.context_count(), contexts_num);
}

#[test]
fn test_vplic_global_with_different_contexts() {
    let addr = GuestPhysAddr::from(0x0c000000);

    // Test with 1 context
    let vplic = VPlicGlobal::new(addr, Some(0x400000), 1).unwrap();
    assert_eq!(vplic.context_count(), 1);

    // Test with 4 contexts
    let vplic = VPlicGlobal::new(addr, Some(0x400000), 4).unwrap();
    assert_eq!(vplic.context_count(), 4);

    // Test with 8 contexts
    let vplic = VPlicGlobal::new(addr, Some(0x400000), 8).unwrap();
    assert_eq!(vplic.context_count(), 8);
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

    assert!(vplic.has_unrestricted_sources());
    assert!(!vplic.is_pending(1).unwrap());
    assert!(!vplic.is_active(1).unwrap());
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

    vplic.assign_source(5).unwrap();
    assert_eq!(
        vplic.set_pending(6),
        Err(VplicError::SourceNotAssigned { source_id: 6 })
    );
    assert_eq!(vplic.set_pending(5), Ok(()));
}

#[test]
fn explicit_empty_assignment_rejects_every_external_source() {
    let vplic =
        VPlicGlobal::new(GuestPhysAddr::from(HOST_PLIC_BASE), Some(HOST_PLIC_SIZE), 2).unwrap();

    vplic.restrict_to_assigned_sources();

    assert!(!vplic.has_unrestricted_sources());
    assert_eq!(
        vplic.set_pending(1),
        Err(VplicError::SourceNotAssigned { source_id: 1 })
    );
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
    assert!(vplic.is_active(irq_id).unwrap());

    vplic
        .handle_write(claim_addr, AccessWidth::Dword, irq_id)
        .unwrap();
    assert!(!vplic.is_active(irq_id).unwrap());
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
fn register_state_is_vm_local_and_guest_address_independent() {
    let first_addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let second_addr = GuestPhysAddr::from(0x1c00_0000);
    let first = VPlicGlobal::new(first_addr, Some(HOST_PLIC_SIZE), 2).unwrap();
    let second = VPlicGlobal::new(second_addr, Some(HOST_PLIC_SIZE), 2).unwrap();
    let source = 9;

    first
        .handle_write(
            first_addr + PLIC_PRIORITY_OFFSET + source * 4,
            AccessWidth::Dword,
            3,
        )
        .unwrap();
    second
        .handle_write(
            second_addr + PLIC_PRIORITY_OFFSET + source * 4,
            AccessWidth::Dword,
            7,
        )
        .unwrap();

    assert_eq!(
        first
            .handle_read(
                first_addr + PLIC_PRIORITY_OFFSET + source * 4,
                AccessWidth::Dword,
            )
            .unwrap(),
        3
    );
    assert_eq!(
        second
            .handle_read(
                second_addr + PLIC_PRIORITY_OFFSET + source * 4,
                AccessWidth::Dword,
            )
            .unwrap(),
        7
    );
}

#[test]
fn asserted_level_source_repends_after_guest_completion() {
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE), 2).unwrap();
    let source = 10;
    let context = 1;
    let claim = addr
        + PLIC_CONTEXT_CTRL_OFFSET
        + context * PLIC_CONTEXT_STRIDE
        + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET;
    vplic
        .handle_write(
            addr + PLIC_PRIORITY_OFFSET + source * 4,
            AccessWidth::Dword,
            1,
        )
        .unwrap();
    vplic
        .handle_write(
            addr + PLIC_ENABLE_OFFSET + context * PLIC_ENABLE_STRIDE,
            AccessWidth::Dword,
            1 << source,
        )
        .unwrap();

    vplic.set_source_level(source, true).unwrap();
    assert_eq!(
        vplic.handle_read(claim, AccessWidth::Dword).unwrap(),
        source
    );
    vplic
        .handle_write(claim, AccessWidth::Dword, source)
        .unwrap();
    assert!(vplic.is_pending(source).unwrap());

    vplic.set_source_level(source, false).unwrap();
    assert_eq!(
        vplic.handle_read(claim, AccessWidth::Dword).unwrap(),
        source
    );
    vplic
        .handle_write(claim, AccessWidth::Dword, source)
        .unwrap();
    assert!(!vplic.is_pending(source).unwrap());
}
