#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use core::sync::atomic::{AtomicPtr, Ordering};

use ax_plat::irq::{IrqAffinity, IrqError, IrqId, IrqIf, IrqSource, TrapVector, dispatch_irq};

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
    fn set_enable(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
        somehal::irq::irq_set_enable(irq, enabled)
    }

    fn set_affinity(irq: IrqId, affinity: IrqAffinity) -> Result<(), IrqError> {
        let affinity = match affinity {
            IrqAffinity::Any => somehal::irq::IrqAffinity::Any,
            IrqAffinity::Fixed(cpu) => somehal::irq::IrqAffinity::Fixed { cpu_id: cpu.0 },
        };
        somehal::irq::irq_set_affinity(irq, affinity)
    }

    /// Handles the IRQ.
    fn handle(vector: TrapVector) -> Option<IrqId> {
        let irq = {
            let active = somehal::irq::begin_irq(vector.0)?;
            let irq = active.id();

            #[cfg(all(target_arch = "riscv64", feature = "hv"))]
            if is_guest_forwardable(irq) && inject_virtual_irq(irq.hwirq.0 as usize) {
                return Some(irq);
            }

            let outcome = dispatch_irq(irq);
            if !outcome.handled {
                if outcome.called == 0 {
                    warn!("Unhandled IRQ {irq:?}");
                } else {
                    debug!("Spurious IRQ {irq:?}");
                }
            }
            irq
        };
        Some(irq)
    }

    fn send_ipi(id: IrqId, target: ax_plat::irq::IpiTarget) {
        let target = match target {
            ax_plat::irq::IpiTarget::Current { cpu_id } => {
                somehal::irq::IpiTarget::Current { cpu_id }
            }
            ax_plat::irq::IpiTarget::Other { cpu_id } => somehal::irq::IpiTarget::Other { cpu_id },
            ax_plat::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                somehal::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num }
            }
        };
        somehal::irq::send_ipi(id, target);
    }

    fn resolve_source(source: IrqSource) -> Result<IrqId, IrqError> {
        somehal::irq::resolve_irq_source(source)
    }

    fn resolve_percpu(hwirq: ax_plat::irq::HwIrq) -> Result<IrqId, IrqError> {
        #[cfg(target_arch = "aarch64")]
        {
            somehal::irq::aarch64_gic_irq_id_checked(hwirq)
        }
        #[cfg(any(target_arch = "loongarch64", target_arch = "riscv64", target_arch = "x86_64"))]
        {
            Ok(IrqId::new(somehal::irq::CPU_LOCAL_IRQ_DOMAIN, hwirq))
        }
    }
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
fn is_guest_forwardable(irq: IrqId) -> bool {
    somehal::irq::domain_is_kind(irq.domain, somehal::irq::IrqDomainKind::RiscvPlic)
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

#[cfg(test)]
mod tests {
    use ax_plat::irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqId};

    #[test]
    fn cpu_local_irq_is_never_forwarded_to_guest() {
        let irq = IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(5));

        assert!(!super::is_guest_forwardable(irq));
    }

    #[test]
    fn plic_irq_can_be_forwarded_to_guest() {
        let domain = somehal::irq::alloc_irq_domain(
            rdrive::DeviceId::new(),
            somehal::irq::IrqDomainKind::RiscvPlic,
        )
        .unwrap();
        let irq = IrqId::new(domain, HwIrq(10));

        assert!(super::is_guest_forwardable(irq));
    }
}
