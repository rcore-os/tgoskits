#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use core::sync::atomic::{AtomicPtr, Ordering};

use ax_plat::irq::{IrqIf, dispatch_irq};
use somehal::irq_handler;

#[cfg(target_arch = "aarch64")]
const GIC_SPECIAL_IRQ_START: usize = 1020;

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
const RISCV_INTERRUPT_BIT: usize = 1usize << (usize::BITS as usize - 1);
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
const RISCV_S_EXT: usize = RISCV_INTERRUPT_BIT | 9;

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
static VIRTUAL_IRQ_INJECTOR: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub fn register_virtual_irq_injector(injector: fn(usize) -> bool) {
    VIRTUAL_IRQ_INJECTOR.store(injector as *mut (), Ordering::Release);
}

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    /// Enables or disables the given IRQ.
    fn set_enable(irq_raw: usize, enabled: bool) {
        somehal::irq::irq_set_enable(irq_raw.into(), enabled);
    }

    /// Handles the IRQ.
    fn handle(irq_num: usize) -> Option<usize> {
        #[cfg(all(target_arch = "riscv64", feature = "hv"))]
        if irq_num == RISCV_S_EXT {
            return handle_riscv_external_irq();
        }

        let irq = somehal::irq::irq_handler_with_raw(irq_num)?;
        Some(irq.raw())
    }

    fn send_ipi(id: usize, target: ax_plat::irq::IpiTarget) {
        let target = match target {
            ax_plat::irq::IpiTarget::Current { cpu_id } => {
                somehal::irq::IpiTarget::Current { cpu_id }
            }
            ax_plat::irq::IpiTarget::Other { cpu_id } => somehal::irq::IpiTarget::Other { cpu_id },
            ax_plat::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                somehal::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num }
            }
        };
        somehal::irq::send_ipi(id.into(), target);
    }
}

#[irq_handler]
fn somehal_handle_irq(irq: somehal::irq::IrqId) {
    #[cfg(target_arch = "aarch64")]
    if irq.raw() >= GIC_SPECIAL_IRQ_START {
        trace!("Ignoring special IRQ {irq:?}");
        return;
    }

    if !dispatch_irq(irq.raw()).handled {
        warn!("Unhandled IRQ {irq:?}");
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn handle_riscv_external_irq() -> Option<usize> {
    let irq = somehal::irq::claim_external_irq()?;
    let irq_num = irq.raw();
    if inject_virtual_irq(irq_num) {
        somehal::irq::complete_external_irq(irq);
        return Some(irq_num);
    }
    if !dispatch_irq(irq_num).handled {
        warn!("Unhandled IRQ {irq:?}");
    }
    somehal::irq::complete_external_irq(irq);
    Some(irq_num)
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn inject_virtual_irq(irq: usize) -> bool {
    let injector = VIRTUAL_IRQ_INJECTOR.load(Ordering::Acquire);
    if injector.is_null() {
        warn!("virtual IRQ injector is not registered");
        return false;
    }
    unsafe { core::mem::transmute::<*mut (), fn(usize) -> bool>(injector)(irq) }
}
