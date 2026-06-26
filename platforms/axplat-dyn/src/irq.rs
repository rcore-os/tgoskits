#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use core::sync::atomic::{AtomicPtr, Ordering};

use ax_plat::irq::{IrqAffinity, IrqError, IrqIf, dispatch_irq};

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
const RISCV_INTERRUPT_BIT: usize = 1usize << (usize::BITS as usize - 1);

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

    fn set_affinity(irq_raw: usize, affinity: IrqAffinity) -> Result<(), IrqError> {
        let affinity = match affinity {
            IrqAffinity::Any => somehal::irq::IrqAffinity::Any,
            IrqAffinity::Fixed(cpu) => somehal::irq::IrqAffinity::Fixed { cpu_id: cpu.0 },
        };
        somehal::irq::irq_set_affinity(irq_raw.into(), affinity).map_err(|_| IrqError::Unsupported)
    }

    /// Handles the IRQ.
    fn handle(irq_num: usize) -> Option<usize> {
        let irq_num = {
            let active = somehal::irq::begin_irq(irq_num)?;
            let irq = active.id();
            let irq_num = irq.raw();

            #[cfg(all(target_arch = "riscv64", feature = "hv"))]
            if (irq_num & RISCV_INTERRUPT_BIT == 0) && inject_virtual_irq(irq_num) {
                return Some(irq_num);
            }

            let outcome = dispatch_irq(irq_num);
            if !outcome.handled {
                if outcome.called == 0 {
                    warn!("Unhandled IRQ {irq:?}");
                } else {
                    debug!("Spurious IRQ {irq:?}");
                }
            }
            irq_num
        };
        Some(irq_num)
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

    fn ipi_irq() -> usize {
        somehal::irq::ipi_irq().raw()
    }
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
