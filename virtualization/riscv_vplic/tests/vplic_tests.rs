use axdevice_base::AccessWidth;
use axvm_types::GuestPhysAddr;
use riscv_vplic::{
    PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET, PLIC_CONTEXT_CTRL_OFFSET, PLIC_CONTEXT_STRIDE,
    PLIC_ENABLE_OFFSET, PLIC_ENABLE_STRIDE, PLIC_NUM_SOURCES, PLIC_PENDING_OFFSET,
    PLIC_PRIORITY_OFFSET, VPlicGlobal, VplicError,
};

const HOST_PLIC_BASE: usize = 0x0c00_0000;
const HOST_PLIC_SIZE: usize = 0x40_0000;

#[test]
fn test_vplic_global_rejects_missing_or_insufficient_mmio_regions() {
    let addr = GuestPhysAddr::from(0x0c000000);
    assert!(matches!(
        VPlicGlobal::new(addr, None, 2),
        Err(VplicError::MissingRegionSize)
    ));
    assert!(matches!(
        VPlicGlobal::new(addr, Some(0x1000), 2),
        Err(VplicError::InsufficientRegion { .. })
    ));
}

#[test]
fn test_typed_pending_api_is_visible_through_mmio() {
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE), 2).unwrap();

    vplic.set_pending(33).unwrap();

    assert!(vplic.is_pending(33).unwrap());
    assert_eq!(
        vplic
            .read_register(addr + PLIC_PENDING_OFFSET + 4, AccessWidth::Dword)
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
        .write_register(
            addr + PLIC_PRIORITY_OFFSET + irq_id * 4,
            AccessWidth::Dword,
            1,
        )
        .unwrap();
    vplic
        .write_register(
            addr + PLIC_ENABLE_OFFSET + context_id * PLIC_ENABLE_STRIDE,
            AccessWidth::Dword,
            1 << irq_id,
        )
        .unwrap();
    assert!(vplic.set_irq_line_level(irq_id, true).unwrap());
    assert!(!vplic.set_irq_line_level(irq_id, true).unwrap());

    let claim_addr = addr
        + PLIC_CONTEXT_CTRL_OFFSET
        + context_id * PLIC_CONTEXT_STRIDE
        + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET;
    assert_eq!(
        vplic.read_register(claim_addr, AccessWidth::Dword).unwrap(),
        irq_id
    );
    assert!(!vplic.is_pending(irq_id).unwrap());
    assert!(vplic.active_irqs.lock().get(irq_id));

    // The UART still holds its level line high, so completion must repend it
    // without requiring another device poll.
    vplic
        .write_register(claim_addr, AccessWidth::Dword, irq_id)
        .unwrap();
    assert!(!vplic.active_irqs.lock().get(irq_id));
    assert!(vplic.is_pending(irq_id).unwrap());
    assert_eq!(
        vplic.read_register(claim_addr, AccessWidth::Dword).unwrap(),
        irq_id
    );
    assert!(vplic.active_irqs.lock().get(irq_id));

    assert!(!vplic.set_irq_line_level(irq_id, false).unwrap());
    vplic
        .write_register(claim_addr, AccessWidth::Dword, irq_id)
        .unwrap();
    assert_eq!(
        vplic.read_register(claim_addr, AccessWidth::Dword).unwrap(),
        0
    );
}

#[test]
fn test_completion_event_is_reported_only_after_an_active_guest_claim() {
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE), 2).unwrap();
    let irq_id = 7;
    let context_id = 1;
    let claim_addr = addr
        + PLIC_CONTEXT_CTRL_OFFSET
        + context_id * PLIC_CONTEXT_STRIDE
        + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET;

    assert_eq!(
        vplic
            .write_register_with_completion(claim_addr, AccessWidth::Dword, irq_id)
            .unwrap(),
        None
    );

    vplic
        .write_register(
            addr + PLIC_PRIORITY_OFFSET + irq_id * 4,
            AccessWidth::Dword,
            1,
        )
        .unwrap();
    vplic
        .write_register(
            addr + PLIC_ENABLE_OFFSET + context_id * PLIC_ENABLE_STRIDE,
            AccessWidth::Dword,
            1 << irq_id,
        )
        .unwrap();
    vplic.set_pending(irq_id).unwrap();
    assert_eq!(
        vplic.read_register(claim_addr, AccessWidth::Dword).unwrap(),
        irq_id
    );

    let completion = vplic
        .write_register_with_completion(claim_addr, AccessWidth::Dword, irq_id)
        .unwrap();
    assert_eq!(completion.map(|event| event.source()), Some(irq_id));
    assert!(!vplic.active_irqs.lock().get(irq_id));
}

#[test]
fn test_deliverable_state_tracks_guest_enable_claim_and_level_completion() {
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE), 2).unwrap();
    let irq_id = 7;
    let context_id = 1;

    assert!(!vplic.context_has_deliverable_irq(context_id).unwrap());
    vplic.set_irq_line_level(irq_id, true).unwrap();
    assert!(!vplic.context_has_deliverable_irq(context_id).unwrap());

    vplic
        .write_register(
            addr + PLIC_PRIORITY_OFFSET + irq_id * 4,
            AccessWidth::Dword,
            1,
        )
        .unwrap();
    vplic
        .write_register(
            addr + PLIC_ENABLE_OFFSET + context_id * PLIC_ENABLE_STRIDE,
            AccessWidth::Dword,
            1 << irq_id,
        )
        .unwrap();
    assert!(vplic.context_has_deliverable_irq(context_id).unwrap());

    let claim_addr = addr
        + PLIC_CONTEXT_CTRL_OFFSET
        + context_id * PLIC_CONTEXT_STRIDE
        + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET;
    assert_eq!(
        vplic.read_register(claim_addr, AccessWidth::Dword).unwrap(),
        irq_id
    );
    assert!(!vplic.context_has_deliverable_irq(context_id).unwrap());

    vplic
        .write_register(claim_addr, AccessWidth::Dword, irq_id)
        .unwrap();
    assert!(vplic.context_has_deliverable_irq(context_id).unwrap());

    vplic.set_irq_line_level(irq_id, false).unwrap();
    assert!(!vplic.context_has_deliverable_irq(context_id).unwrap());
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
