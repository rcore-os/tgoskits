#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
use ax_plat::irq::IrqOutcome;
use ax_plat::irq::{
    CpuId, IrqAffinity, IrqError, IrqId, IrqIf, IrqSource, TrapVector, dispatch_irq_on,
};

#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
mod loongarch64_hv;

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
static VIRTUAL_IRQ_INJECTOR: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
static VIRTUAL_IRQ_TARGET_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
static VIRTUAL_IRQ_AFFINITY_CONFIGURED: [AtomicBool; RISCV_PLIC_SOURCE_COUNT] =
    [const { AtomicBool::new(false) }; RISCV_PLIC_SOURCE_COUNT];
#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
const RISCV_PLIC_SOURCE_COUNT: usize = 1024;

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub fn register_virtual_irq_injector(injector: fn(usize) -> bool) {
    VIRTUAL_IRQ_INJECTOR.store(injector as *mut (), Ordering::Release);
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub fn set_virtual_irq_targets(cpu_id: usize, irq_sources: &[u32]) {
    VIRTUAL_IRQ_TARGET_CPU.store(cpu_id, Ordering::Release);
    for configured in &VIRTUAL_IRQ_AFFINITY_CONFIGURED {
        configured.store(false, Ordering::Release);
    }
    for &irq in irq_sources {
        route_virtual_irq_to_target_cpu(irq as usize);
    }
}

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    fn prepare(_vector: TrapVector) {}

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
            if should_forward_riscv_guest_irq(irq, IrqOutcome::default())
                && inject_virtual_irq(irq.hwirq.0 as usize)
            {
                return Some(irq);
            }

            let cpu = current_irq_cpu();
            let outcome = dispatch_irq_on(irq, cpu);
            if !outcome.handled {
                #[cfg(all(target_arch = "loongarch64", feature = "hv"))]
                if is_loongarch_guest_forwardable(irq)
                    && loongarch64_hv::inject_virtual_irq(irq.hwirq.0 as usize)
                {
                    return Some(irq);
                }

                if outcome.called == 0 {
                    warn!("Unhandled IRQ {irq:?} on CPU {}", cpu.0);
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

    fn ipi_irq() -> IrqId {
        somehal::irq::ipi_irq()
    }

    fn resolve_source(source: IrqSource) -> Result<IrqId, IrqError> {
        somehal::irq::resolve_irq_source(source)
    }

    fn resolve_percpu(hwirq: ax_plat::irq::HwIrq) -> Result<IrqId, IrqError> {
        #[cfg(target_arch = "aarch64")]
        {
            somehal::irq::aarch64_gic_irq_id_checked(hwirq)
        }
        #[cfg(any(target_arch = "loongarch64", target_arch = "x86_64"))]
        {
            Ok(IrqId::new(somehal::irq::CPU_LOCAL_IRQ_DOMAIN, hwirq))
        }
        #[cfg(target_arch = "riscv64")]
        {
            Ok(IrqId::new(somehal::irq::CPU_LOCAL_IRQ_DOMAIN, hwirq))
        }
    }
}

fn current_irq_cpu() -> CpuId {
    CpuId(ax_plat::percpu::this_cpu_id())
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
fn is_guest_forwardable(irq: IrqId) -> bool {
    somehal::irq::domain_is_kind(irq.domain, somehal::irq::IrqDomainKind::RiscvPlic)
}

#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
fn should_forward_riscv_guest_irq(irq: IrqId, _host_outcome: IrqOutcome) -> bool {
    is_guest_forwardable(irq)
}

#[cfg(test)]
fn riscv_plic_source_index(irq: IrqId) -> Option<usize> {
    if !is_guest_forwardable(irq) {
        return None;
    }
    let source = irq.hwirq.0 as usize;
    (1..RISCV_PLIC_SOURCE_COUNT)
        .contains(&source)
        .then_some(source)
}

#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
fn is_loongarch_guest_forwardable(irq: IrqId) -> bool {
    somehal::irq::domain_is_kind(irq.domain, somehal::irq::IrqDomainKind::LoongArchEioIntc)
        || somehal::irq::domain_is_kind(irq.domain, somehal::irq::IrqDomainKind::LoongArchPchPic)
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn inject_virtual_irq(irq: usize) -> bool {
    route_virtual_irq_to_target_cpu(irq);

    let injector = VIRTUAL_IRQ_INJECTOR.load(Ordering::Acquire);
    if injector.is_null() {
        trace!("skip RISC-V virtual IRQ {irq}: injector is not registered");
        return false;
    }
    unsafe { core::mem::transmute::<*mut (), fn(usize) -> bool>(injector)(irq) }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn route_virtual_irq_to_target_cpu(irq: usize) {
    if irq == 0 || irq >= RISCV_PLIC_SOURCE_COUNT {
        return;
    }
    let target_cpu = VIRTUAL_IRQ_TARGET_CPU.load(Ordering::Acquire);
    if target_cpu == usize::MAX {
        return;
    }
    let configured = &VIRTUAL_IRQ_AFFINITY_CONFIGURED[irq];
    if configured.swap(true, Ordering::AcqRel) {
        return;
    }

    let Some(domain) = somehal::irq::domain_by_kind_fast(somehal::irq::IrqDomainKind::RiscvPlic)
    else {
        configured.store(false, Ordering::Release);
        trace!("skip RISC-V virtual IRQ {irq} affinity: PLIC domain is not registered");
        return;
    };
    let irq_id = IrqId::new(domain, ax_plat::irq::HwIrq(irq as u32));
    let affinity = somehal::irq::IrqAffinity::Fixed { cpu_id: target_cpu };
    if let Err(err) = somehal::irq::irq_set_affinity(irq_id, affinity) {
        configured.store(false, Ordering::Release);
        trace!("skip RISC-V virtual IRQ {irq} affinity to CPU {target_cpu}: {err:?}");
    }
}

#[cfg(test)]
mod tests {
    use ax_plat::irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqId};
    use spin::Once;

    fn plic_irq(hwirq: u32) -> IrqId {
        static PLIC_DOMAIN: Once<somehal::irq::IrqDomainId> = Once::new();

        let domain = *PLIC_DOMAIN.call_once(|| {
            somehal::irq::domain_by_kind(somehal::irq::IrqDomainKind::RiscvPlic)
                .map(|domain| domain.id)
                .unwrap_or_else(|| {
                    somehal::irq::alloc_irq_domain(
                        rdrive::DeviceId::new(),
                        somehal::irq::IrqDomainKind::RiscvPlic,
                    )
                    .unwrap()
                })
        });
        IrqId::new(domain, HwIrq(hwirq))
    }

    #[test]
    fn cpu_local_irq_is_never_forwarded_to_guest() {
        let irq = IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(5));

        assert!(!super::is_guest_forwardable(irq));
    }

    #[test]
    fn plic_irq_can_be_forwarded_to_guest() {
        let irq = plic_irq(10);

        assert!(super::is_guest_forwardable(irq));
    }

    #[test]
    fn handled_plic_irq_remains_forwardable_to_passthrough_guest() {
        let irq = plic_irq(1);
        let host_outcome = ax_plat::irq::IrqOutcome {
            handled: true,
            wake: false,
            called: 1,
        };

        assert!(super::should_forward_riscv_guest_irq(irq, host_outcome));
    }

    #[test]
    fn unhandled_plic_irq_can_be_forwarded_to_guest() {
        let irq = plic_irq(2);

        assert!(super::should_forward_riscv_guest_irq(
            irq,
            ax_plat::irq::IrqOutcome::default()
        ));
    }

    #[test]
    fn only_real_plic_sources_have_virtual_irq_source_index() {
        let irq = plic_irq(2);
        assert_eq!(super::riscv_plic_source_index(irq), Some(2));

        let reserved = IrqId::new(irq.domain, HwIrq(0));
        assert_eq!(super::riscv_plic_source_index(reserved), None);

        let out_of_range = IrqId::new(irq.domain, HwIrq(super::RISCV_PLIC_SOURCE_COUNT as u32));
        assert_eq!(super::riscv_plic_source_index(out_of_range), None);
    }
}
