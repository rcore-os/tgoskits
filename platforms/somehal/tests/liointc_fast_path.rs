use core::ptr::NonNull;

use irq_framework::{HwIrq, IrqDomainId, IrqId};
use mmio_api::{MmioAddr, MmioRaw};

#[path = "../src/arch/loongarch64/liointc_fast_path.rs"]
mod liointc_fast_path;

use liointc_fast_path::{LIOINTC_VECTOR_COUNT, LioIntcFastPath, REG_ENABLE};

#[test]
fn claims_pending_irq_while_control_plane_is_busy() {
    let mut regs = [0u32; 16];
    let mut isr = [0u32; 4];
    let regs = test_mmio(&mut regs);
    let isr = test_mmio(&mut isr);
    let domain = IrqDomainId(7);
    let fast_path = LioIntcFastPath::new(domain, regs, isr.clone(), [Some(2), None, None, None]);
    fast_path.set_enabled(5, true);
    isr.write(0, 1u32 << 5);

    assert_eq!(
        fast_path.claim_irq_while_control_busy(2),
        Some(IrqId::new(domain, HwIrq(5)))
    );
    fast_path.complete_irq(IrqId::new(domain, HwIrq(5)));
}

#[test]
fn enable_state_masks_pending_inputs_and_programs_w1_register() {
    let mut regs = [0u32; 16];
    let mut isr = [0u32; 4];
    let regs = test_mmio(&mut regs);
    let isr = test_mmio(&mut isr);
    let fast_path = LioIntcFastPath::new(
        IrqDomainId(7),
        regs.clone(),
        isr.clone(),
        [Some(2), None, None, None],
    );
    isr.write(0, u32::MAX);

    fast_path.set_enabled(LIOINTC_VECTOR_COUNT - 1, true);

    assert_eq!(regs.read::<u32>(REG_ENABLE), 1u32 << 31);
    assert_eq!(fast_path.enabled_mask(), 1u32 << 31);
    assert_eq!(
        fast_path.claim_irq(2),
        Some(IrqId::new(IrqDomainId(7), HwIrq(31)))
    );
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
