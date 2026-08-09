use ax_plat::irq::{
    CpuId, IrqAffinity, IrqError, IrqId, IrqIf, IrqSource, IrqTrigger, TrapVector, dispatch_irq_on,
};

#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
mod loongarch64_hv;
#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
mod riscv64_hv;
#[cfg(any(all(target_arch = "riscv64", feature = "hv"), test))]
const RISCV_PLIC_SOURCE_COUNT: usize = 1024;

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    fn prepare(_vector: TrapVector) {}

    fn init_boot_irqs(cpu_id: usize) -> Result<(), IrqError> {
        somehal::irq::init_boot_irqs(cpu_id)
    }

    #[cfg(feature = "smp")]
    fn init_secondary_boot_irqs(cpu_id: usize) -> Result<(), IrqError> {
        somehal::irq::init_secondary_boot_irqs(cpu_id);
        Ok(())
    }

    /// Enables or disables the given IRQ.
    fn set_enable(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
        somehal::irq::irq_set_enable(irq, enabled)
    }

    fn set_trigger(irq: IrqId, trigger: IrqTrigger) -> Result<(), IrqError> {
        #[cfg(target_arch = "aarch64")]
        {
            let trigger = match trigger {
                IrqTrigger::Edge => somehal::irq::IrqTrigger::Edge,
                IrqTrigger::Level => somehal::irq::IrqTrigger::Level,
            };
            somehal::arch::gic::irq_set_trigger(irq, trigger)
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            let _ = (irq, trigger);
            Err(IrqError::Unsupported)
        }
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
            let mut active = active;

            #[cfg(all(target_arch = "riscv64", feature = "hv"))]
            let mut guest_claim = is_guest_forwardable(irq)
                .then(|| riscv64_hv::GuestPlicClaim::detach(&mut active, irq))
                .flatten();

            #[cfg(all(target_arch = "riscv64", feature = "hv"))]
            if guest_claim
                .as_mut()
                .is_some_and(riscv64_hv::GuestPlicClaim::publish_to_guest)
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

    fn send_ipi(id: IrqId, target: ax_plat::irq::IpiTarget) -> Result<(), IrqError> {
        let target = match target {
            ax_plat::irq::IpiTarget::Current => somehal::irq::IpiTarget::Current,
            ax_plat::irq::IpiTarget::Cpu(cpu) => {
                if cpu.0 >= ax_plat::power::cpu_num() {
                    return Err(IrqError::InvalidCpu);
                }
                if !ax_plat::irq::is_cpu_online(cpu.0) {
                    return Err(IrqError::CpuOffline);
                }
                somehal::irq::IpiTarget::Cpu(somehal::irq::CpuId(cpu.0))
            }
        };
        somehal::irq::send_ipi(id, target)
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
            let parent = somehal::irq::aarch64_gic_irq_id_checked(hwirq)?;
            Ok(somehal::irq::resolve_irq_route(parent))
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
    fn only_real_plic_sources_have_virtual_irq_source_index() {
        let irq = plic_irq(2);
        assert_eq!(super::riscv_plic_source_index(irq), Some(2));

        let reserved = IrqId::new(irq.domain, HwIrq(0));
        assert_eq!(super::riscv_plic_source_index(reserved), None);

        let out_of_range = IrqId::new(irq.domain, HwIrq(super::RISCV_PLIC_SOURCE_COUNT as u32));
        assert_eq!(super::riscv_plic_source_index(out_of_range), None);
    }
}
