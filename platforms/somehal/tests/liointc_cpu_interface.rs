use core::ptr::NonNull;

use irq_framework::{HwIrq, IrqDomainId, IrqId};
use mmio_api::{MmioAddr, MmioRaw};

#[path = "../src/arch/loongarch64/liointc_cpu_interface.rs"]
mod liointc_cpu_interface;

use liointc_cpu_interface::{LIOINTC_VECTOR_COUNT, LioIntcCpuInterface};

#[test]
fn claims_pending_irq_through_dedicated_cpu_interface() {
    let mut isr = [0u32; 4];
    let isr = test_mmio(&mut isr);
    let domain = IrqDomainId(7);
    let cpu_if = LioIntcCpuInterface::new(domain, isr.clone(), [Some(2), None, None, None]);
    cpu_if.publish_enabled(5);
    isr.write(0, 1u32 << 5);

    assert_eq!(cpu_if.claim_irq(3), None);
    assert_eq!(cpu_if.claim_irq(2), Some(IrqId::new(domain, HwIrq(5))));
    cpu_if.complete_irq(IrqId::new(domain, HwIrq(5)));
}

#[test]
fn enabled_snapshot_masks_pending_inputs() {
    let mut isr = [0u32; 4];
    let isr = test_mmio(&mut isr);
    let cpu_if = LioIntcCpuInterface::new(IrqDomainId(7), isr.clone(), [Some(2), None, None, None]);
    isr.write(0, u32::MAX);

    cpu_if.publish_enabled(LIOINTC_VECTOR_COUNT - 1);

    assert_eq!(cpu_if.enabled_mask(), 1u32 << 31);
    assert_eq!(
        cpu_if.claim_irq(2),
        Some(IrqId::new(IrqDomainId(7), HwIrq(31)))
    );

    cpu_if.hide_disabled(LIOINTC_VECTOR_COUNT - 1);

    assert_eq!(cpu_if.enabled_mask(), 0);
    assert_eq!(cpu_if.claim_irq(2), None);
}

fn test_mmio<const N: usize>(backing: &mut [u32; N]) -> MmioRaw {
    let virt = NonNull::new(backing.as_mut_ptr().cast::<u8>()).unwrap();
    // SAFETY: the returned mapping is used only while `backing` remains alive
    // in this test, and its size exactly matches the backing array.
    unsafe {
        MmioRaw::new(
            MmioAddr::from(0usize),
            virt,
            core::mem::size_of_val(backing),
        )
    }
}
