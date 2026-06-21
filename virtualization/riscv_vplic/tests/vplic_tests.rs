use ax_crate_interface::impl_interface;
use ax_errno::AxError;
use ax_memory_addr::{PhysAddr, VirtAddr};
use axdevice_base::{AccessWidth, BaseDeviceOps};
use axvm_types::GuestPhysAddr;
use riscv_vplic::{
    PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET, PLIC_CONTEXT_CTRL_OFFSET, PLIC_CONTEXT_STRIDE,
    PLIC_ENABLE_OFFSET, PLIC_ENABLE_STRIDE, PLIC_NUM_SOURCES, PLIC_PENDING_OFFSET,
    PLIC_PRIORITY_OFFSET, VPlicGlobal, host::RiscvVplicHostIf,
};

const HOST_PLIC_BASE: usize = 0x0c00_0000;
const HOST_PLIC_SIZE: usize = 0x40_0000;

#[repr(align(8))]
struct AlignedHostPlic([u8; HOST_PLIC_SIZE]);

static mut HOST_PLIC: AlignedHostPlic = AlignedHostPlic([0; HOST_PLIC_SIZE]);

struct TestRiscvVplicHostIf;

#[impl_interface]
impl RiscvVplicHostIf for TestRiscvVplicHostIf {
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        let offset = paddr.as_usize() - HOST_PLIC_BASE;
        assert!(offset < HOST_PLIC_SIZE);
        let base = unsafe { core::ptr::addr_of_mut!(HOST_PLIC.0).cast::<u8>() };
        VirtAddr::from(unsafe { base.add(offset) } as usize)
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

    let vplic = VPlicGlobal::new(addr, Some(size), contexts_num);

    assert_eq!(vplic.addr, addr);
    assert_eq!(vplic.size, size);
    assert_eq!(vplic.contexts_num, contexts_num);
}

#[test]
fn test_vplic_global_with_different_contexts() {
    let addr = GuestPhysAddr::from(0x0c000000);

    // Test with 1 context
    let vplic = VPlicGlobal::new(addr, Some(0x400000), 1);
    assert_eq!(vplic.contexts_num, 1);

    // Test with 4 contexts
    let vplic = VPlicGlobal::new(addr, Some(0x400000), 4);
    assert_eq!(vplic.contexts_num, 4);

    // Test with 8 contexts
    let vplic = VPlicGlobal::new(addr, Some(0x400000), 8);
    assert_eq!(vplic.contexts_num, 8);
}

#[test]
#[should_panic(expected = "Size must be specified")]
fn test_vplic_global_size_none_panics() {
    let addr = GuestPhysAddr::from(0x0c000000);
    let _ = VPlicGlobal::new(addr, None, 2);
}

#[test]
#[should_panic(expected = "exceeds region")]
fn test_vplic_global_insufficient_size_panics() {
    let addr = GuestPhysAddr::from(0x0c000000);
    // Size too small for 2 contexts
    let _ = VPlicGlobal::new(addr, Some(0x1000), 2);
}

#[test]
fn test_vplic_global_bitmaps_initialized_empty() {
    let addr = GuestPhysAddr::from(0x0c000000);
    let vplic = VPlicGlobal::new(addr, Some(0x400000), 2);

    assert!(vplic.assigned_irqs.lock().is_empty());
    assert!(vplic.pending_irqs.lock().is_empty());
    assert!(vplic.active_irqs.lock().is_empty());
}

#[test]
fn test_typed_pending_api_is_visible_through_mmio() {
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE), 2);

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
    let vplic = VPlicGlobal::new(GuestPhysAddr::from(HOST_PLIC_BASE), Some(HOST_PLIC_SIZE), 2);

    assert_eq!(vplic.set_pending(0), Err(AxError::InvalidInput));
    assert_eq!(
        vplic.set_pending(PLIC_NUM_SOURCES),
        Err(AxError::InvalidInput)
    );

    vplic.assigned_irqs.lock().set(5, true);
    assert_eq!(vplic.set_pending(6), Err(AxError::PermissionDenied));
    assert_eq!(vplic.set_pending(5), Ok(()));
}

#[test]
fn test_claim_and_complete_move_irq_between_pending_and_active() {
    let addr = GuestPhysAddr::from(HOST_PLIC_BASE);
    let vplic = VPlicGlobal::new(addr, Some(HOST_PLIC_SIZE), 2);
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
fn test_virtual_plic_instances_and_guest_addresses_are_independent() {
    let first = VPlicGlobal::new(GuestPhysAddr::from(0x0c00_0000), Some(HOST_PLIC_SIZE), 2);
    let second = VPlicGlobal::new(GuestPhysAddr::from(0x1c00_0000), Some(HOST_PLIC_SIZE), 2);

    first.set_pending(11).unwrap();

    assert!(first.is_pending(11).unwrap());
    assert!(!second.is_pending(11).unwrap());
}
