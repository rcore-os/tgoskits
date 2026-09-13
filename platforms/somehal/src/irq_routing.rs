#![cfg(any(test, target_arch = "loongarch64", target_arch = "riscv64"))]

#[cfg(any(test, target_arch = "riscv64"))]
use crate::irq::{CPU_LOCAL_IRQ_DOMAIN, IrqSource};
#[cfg(any(test, target_arch = "riscv64"))]
use crate::irq::{HwIrq, IrqError, IrqId};

#[cfg(any(test, target_arch = "loongarch64"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RawIrq {
    Timer,
    Ipi,
    External,
    Unknown,
}

#[cfg(any(test, target_arch = "loongarch64"))]
pub(super) const fn classify_cpu_irq(
    raw: usize,
    timer_irq: usize,
    ipi_irq: usize,
    external_irq: usize,
) -> RawIrq {
    if raw == timer_irq {
        RawIrq::Timer
    } else if raw == ipi_irq {
        RawIrq::Ipi
    } else if raw == external_irq {
        RawIrq::External
    } else {
        RawIrq::Unknown
    }
}

#[cfg(any(test, target_arch = "loongarch64"))]
pub(super) const fn cpu_local_hwirq_is_runtime_irq(
    raw: usize,
    timer_irq: usize,
    ipi_irq: usize,
    eiointc_irq: usize,
) -> bool {
    matches!(
        classify_cpu_irq(raw, timer_irq, ipi_irq, eiointc_irq),
        RawIrq::Timer | RawIrq::Ipi
    )
}

#[cfg(any(test, target_arch = "loongarch64"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PchPicFirmwareCount {
    ExplicitInputCount(usize),
    AcpiGsiRoutingSpan(usize),
}

#[cfg(any(test, target_arch = "loongarch64"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PchPicInputCountSource {
    HardwareId,
    Explicit(usize),
}

#[cfg(any(test, target_arch = "loongarch64"))]
pub(super) const fn pch_pic_input_count_source(
    firmware_count: PchPicFirmwareCount,
) -> PchPicInputCountSource {
    match firmware_count {
        PchPicFirmwareCount::ExplicitInputCount(count) => PchPicInputCountSource::Explicit(count),
        // BIO_PIC describes the GSI routing span, not the number of inputs
        // implemented by the controller. The hardware ID is authoritative.
        PchPicFirmwareCount::AcpiGsiRoutingSpan(_) => PchPicInputCountSource::HardwareId,
    }
}

#[cfg(any(test, target_arch = "loongarch64"))]
#[derive(Debug, Eq, PartialEq)]
pub(super) enum CascadeTransitionError<E> {
    Parent(E),
    Local(E),
    Rollback { local: E, rollback: E },
}

/// Applies one parent-first cascade transition without retaining either
/// controller borrow across the other operation.
#[cfg(any(test, target_arch = "loongarch64"))]
pub(super) fn apply_parent_first_transition<E>(
    enabled: bool,
    mut set_parent: impl FnMut(bool) -> Result<(), E>,
    set_local: impl FnOnce(bool) -> Result<(), E>,
) -> Result<(), CascadeTransitionError<E>> {
    set_parent(enabled).map_err(CascadeTransitionError::Parent)?;
    let local = match set_local(enabled) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    match set_parent(!enabled) {
        Ok(()) => Err(CascadeTransitionError::Local(local)),
        Err(rollback) => Err(CascadeTransitionError::Rollback { local, rollback }),
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExternalVectorResolveFailure {
    KeepPending,
    Complete,
}

#[cfg(test)]
pub(super) const fn external_vector_failure_policy(err: IrqError) -> ExternalVectorResolveFailure {
    if matches!(err, IrqError::Busy) {
        ExternalVectorResolveFailure::KeepPending
    } else {
        ExternalVectorResolveFailure::Complete
    }
}

#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) const RISCV_INTERRUPT_BIT: usize = 1usize << (usize::BITS as usize - 1);
#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) const RISCV_S_SOFT_CAUSE: usize = 1;
#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) const RISCV_S_TIMER_CAUSE: usize = 5;
#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) const RISCV_S_EXT_CAUSE: usize = 9;
#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) const RISCV_S_SOFT_IRQ: usize = RISCV_INTERRUPT_BIT | RISCV_S_SOFT_CAUSE;
#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) const RISCV_S_TIMER_IRQ: usize = RISCV_INTERRUPT_BIT | RISCV_S_TIMER_CAUSE;
#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) const RISCV_S_EXT_IRQ: usize = RISCV_INTERRUPT_BIT | RISCV_S_EXT_CAUSE;

#[cfg(any(test, target_arch = "riscv64"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RiscvTrapIrq {
    Timer,
    Ipi,
    External,
    UnknownInterrupt { cause: usize },
    BareSource(usize),
}

#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) fn classify_riscv_trap(raw: usize) -> RiscvTrapIrq {
    if raw & RISCV_INTERRUPT_BIT == 0 {
        return RiscvTrapIrq::BareSource(raw);
    }

    match raw & !RISCV_INTERRUPT_BIT {
        RISCV_S_TIMER_CAUSE => RiscvTrapIrq::Timer,
        RISCV_S_SOFT_CAUSE => RiscvTrapIrq::Ipi,
        RISCV_S_EXT_CAUSE => RiscvTrapIrq::External,
        cause => RiscvTrapIrq::UnknownInterrupt { cause },
    }
}

#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) fn riscv_cpu_local_hwirq_is_runtime_irq(hwirq: HwIrq) -> bool {
    matches!(
        hwirq.0 as usize,
        RISCV_S_TIMER_CAUSE | RISCV_S_SOFT_CAUSE | RISCV_S_EXT_CAUSE
    )
}

#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) fn riscv_cpu_local_irq_from_raw(raw: usize) -> Option<IrqId> {
    let cause = raw & !RISCV_INTERRUPT_BIT;
    let hwirq = HwIrq(u32::try_from(cause).ok()?);
    riscv_cpu_local_hwirq_is_runtime_irq(hwirq).then_some(IrqId::new(CPU_LOCAL_IRQ_DOMAIN, hwirq))
}

#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) fn riscv_local_irq_raw(irq: IrqId) -> Result<usize, IrqError> {
    if irq.domain != CPU_LOCAL_IRQ_DOMAIN || !riscv_cpu_local_hwirq_is_runtime_irq(irq.hwirq) {
        return Err(IrqError::InvalidIrq);
    }
    Ok(RISCV_INTERRUPT_BIT | irq.hwirq.0 as usize)
}

#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) fn riscv_plic_hwirq_from_source(
    source: usize,
    source_count: usize,
) -> Result<HwIrq, IrqError> {
    if source == 0 || source > source_count {
        return Err(IrqError::InvalidIrq);
    }
    let source = u32::try_from(source).map_err(|_| IrqError::InvalidIrq)?;
    Ok(HwIrq(source))
}

#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) fn riscv_source_from_plic_hwirq(
    hwirq: HwIrq,
    source_count: usize,
) -> Result<usize, IrqError> {
    let source = hwirq.0 as usize;
    if source == 0 || source > source_count {
        return Err(IrqError::InvalidIrq);
    }
    Ok(source)
}

#[cfg(any(test, target_arch = "riscv64"))]
pub(crate) fn riscv_resolve_controller_line(
    source: IrqSource,
    is_plic_domain: impl FnOnce() -> bool,
) -> Result<(), IrqError> {
    match source {
        IrqSource::ControllerLine { domain, hwirq } if domain == CPU_LOCAL_IRQ_DOMAIN => {
            if riscv_cpu_local_hwirq_is_runtime_irq(hwirq) {
                Ok(())
            } else {
                Err(IrqError::InvalidIrq)
            }
        }
        IrqSource::ControllerLine { .. } if is_plic_domain() => Ok(()),
        IrqSource::ControllerLine { .. } => Err(IrqError::InvalidIrq),
        IrqSource::AcpiGsi(_) | IrqSource::AcpiGsiRoute(_) => Err(IrqError::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqDomainId, IrqError, IrqId, IrqSource};

    #[test]
    fn acpi_routing_span_does_not_override_pch_hardware_input_count() {
        assert_eq!(
            pch_pic_input_count_source(PchPicFirmwareCount::AcpiGsiRoutingSpan(256)),
            PchPicInputCountSource::HardwareId
        );
        assert_eq!(
            pch_pic_input_count_source(PchPicFirmwareCount::ExplicitInputCount(32)),
            PchPicInputCountSource::Explicit(32)
        );
    }

    #[test]
    fn parent_first_cascade_transition_rolls_parent_back_after_local_failure() {
        let calls = core::cell::RefCell::new(alloc::vec::Vec::new());

        let result = apply_parent_first_transition(
            true,
            |enabled| {
                calls.borrow_mut().push(("parent", enabled));
                Ok::<_, u8>(())
            },
            |enabled| {
                calls.borrow_mut().push(("local", enabled));
                Err(7)
            },
        );

        assert_eq!(result, Err(CascadeTransitionError::Local(7)));
        assert_eq!(
            *calls.borrow(),
            alloc::vec![("parent", true), ("local", true), ("parent", false)]
        );
    }

    #[test]
    fn parent_first_cascade_transition_does_not_touch_local_after_parent_failure() {
        let local_called = core::cell::Cell::new(false);

        let result = apply_parent_first_transition(
            false,
            |_| Err::<(), _>(3u8),
            |_| {
                local_called.set(true);
                Ok(())
            },
        );

        assert_eq!(result, Err(CascadeTransitionError::Parent(3)));
        assert!(!local_called.get());
    }

    #[test]
    fn parent_first_cascade_transition_reports_failed_rollback() {
        let parent_calls = core::cell::Cell::new(0);

        let result = apply_parent_first_transition(
            true,
            |_| {
                let call = parent_calls.get();
                parent_calls.set(call + 1);
                if call == 0 { Ok(()) } else { Err(9u8) }
            },
            |_| Err(7u8),
        );

        assert_eq!(
            result,
            Err(CascadeTransitionError::Rollback {
                local: 7,
                rollback: 9,
            })
        );
    }

    #[test]
    fn cpu_irq_classifier_keeps_unknown_lines_local_only() {
        assert_eq!(classify_cpu_irq(11, 11, 12, 3), RawIrq::Timer);
        assert_eq!(classify_cpu_irq(12, 11, 12, 3), RawIrq::Ipi);
        assert_eq!(classify_cpu_irq(3, 11, 12, 3), RawIrq::External);
        assert_eq!(classify_cpu_irq(7, 11, 12, 3), RawIrq::Unknown);

        assert!(cpu_local_hwirq_is_runtime_irq(11, 11, 12, 3));
        assert!(cpu_local_hwirq_is_runtime_irq(12, 11, 12, 3));
        assert!(!cpu_local_hwirq_is_runtime_irq(3, 11, 12, 3));
        assert!(!cpu_local_hwirq_is_runtime_irq(7, 11, 12, 3));
    }

    #[test]
    fn busy_external_vector_resolution_keeps_interrupt_pending() {
        assert_eq!(
            external_vector_failure_policy(IrqError::Busy),
            ExternalVectorResolveFailure::KeepPending
        );
        assert_eq!(
            external_vector_failure_policy(IrqError::Unsupported),
            ExternalVectorResolveFailure::Complete
        );
        assert_eq!(
            external_vector_failure_policy(IrqError::Controller),
            ExternalVectorResolveFailure::Complete
        );
    }

    #[test]
    fn riscv_classifies_only_real_trap_causes_as_runtime_irqs() {
        assert_eq!(classify_riscv_trap(RISCV_S_TIMER_IRQ), RiscvTrapIrq::Timer);
        assert_eq!(classify_riscv_trap(RISCV_S_SOFT_IRQ), RiscvTrapIrq::Ipi);
        assert_eq!(classify_riscv_trap(RISCV_S_EXT_IRQ), RiscvTrapIrq::External);
        assert_eq!(
            classify_riscv_trap(RISCV_INTERRUPT_BIT | 3),
            RiscvTrapIrq::UnknownInterrupt { cause: 3 }
        );
        assert_eq!(classify_riscv_trap(10), RiscvTrapIrq::BareSource(10));
    }

    #[test]
    fn riscv_cpu_local_hwirq_accepts_only_timer_ipi_and_external_cascade() {
        assert!(riscv_cpu_local_hwirq_is_runtime_irq(HwIrq(
            RISCV_S_TIMER_CAUSE as u32
        )));
        assert!(riscv_cpu_local_hwirq_is_runtime_irq(HwIrq(
            RISCV_S_SOFT_CAUSE as u32
        )));
        assert!(riscv_cpu_local_hwirq_is_runtime_irq(HwIrq(
            RISCV_S_EXT_CAUSE as u32
        )));
        assert!(!riscv_cpu_local_hwirq_is_runtime_irq(HwIrq(0)));
        assert!(!riscv_cpu_local_hwirq_is_runtime_irq(HwIrq(10)));
    }

    #[test]
    fn riscv_plic_sources_are_nonzero_and_bounded() {
        assert_eq!(riscv_plic_hwirq_from_source(1, 8), Ok(HwIrq(1)));
        assert_eq!(riscv_plic_hwirq_from_source(8, 8), Ok(HwIrq(8)));
        assert_eq!(
            riscv_plic_hwirq_from_source(0, 8),
            Err(IrqError::InvalidIrq)
        );
        assert_eq!(
            riscv_plic_hwirq_from_source(9, 8),
            Err(IrqError::InvalidIrq)
        );
        assert_eq!(
            riscv_source_from_plic_hwirq(HwIrq(0), 8),
            Err(IrqError::InvalidIrq)
        );
        assert_eq!(
            riscv_source_from_plic_hwirq(HwIrq(9), 8),
            Err(IrqError::InvalidIrq)
        );
    }

    #[test]
    fn riscv_local_irq_raw_encodes_only_runtime_cpu_local_irqs() {
        let ipi = IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(RISCV_S_SOFT_CAUSE as u32));
        let invalid_local = IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(10));
        let external = IrqId::new(IrqDomainId(7), HwIrq(1));

        assert_eq!(riscv_local_irq_raw(ipi), Ok(RISCV_S_SOFT_IRQ));
        assert_eq!(
            riscv_local_irq_raw(invalid_local),
            Err(IrqError::InvalidIrq)
        );
        assert_eq!(riscv_local_irq_raw(external), Err(IrqError::InvalidIrq));
    }

    #[test]
    fn riscv_bare_plic_source_is_not_a_cpu_trap_cause() {
        let irq = riscv_cpu_local_irq_from_raw(10);

        assert_eq!(irq, None);
    }

    #[test]
    fn riscv_resolve_controller_line_keeps_cpu_local_and_plic_domains_separate() {
        let cpu_local = IrqSource::ControllerLine {
            domain: CPU_LOCAL_IRQ_DOMAIN,
            hwirq: HwIrq(RISCV_S_TIMER_CAUSE as u32),
        };
        let invalid_cpu_local = IrqSource::ControllerLine {
            domain: CPU_LOCAL_IRQ_DOMAIN,
            hwirq: HwIrq(10),
        };
        let plic = IrqSource::ControllerLine {
            domain: IrqDomainId(7),
            hwirq: HwIrq(10),
        };
        let other = IrqSource::ControllerLine {
            domain: IrqDomainId(8),
            hwirq: HwIrq(10),
        };

        assert_eq!(riscv_resolve_controller_line(cpu_local, || false), Ok(()));
        assert_eq!(
            riscv_resolve_controller_line(invalid_cpu_local, || false),
            Err(IrqError::InvalidIrq)
        );
        assert_eq!(riscv_resolve_controller_line(plic, || true), Ok(()));
        assert_eq!(
            riscv_resolve_controller_line(other, || false),
            Err(IrqError::InvalidIrq)
        );
    }
}
