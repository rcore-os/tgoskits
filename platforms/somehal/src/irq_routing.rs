#![cfg(any(test, target_arch = "loongarch64", target_arch = "riscv64"))]

#[cfg(any(test, target_arch = "loongarch64"))]
use alloc::vec::Vec;
#[cfg(any(test, target_arch = "loongarch64"))]
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(any(test, target_arch = "loongarch64"))]
use rdif_intc::{AcpiGsiController, AcpiGsiRoute};

#[cfg(any(test, target_arch = "loongarch64"))]
use crate::irq::IrqDomainId;
#[cfg(any(test, target_arch = "riscv64"))]
use crate::irq::{CPU_LOCAL_IRQ_DOMAIN, IrqSource};
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

#[cfg(any(test, target_arch = "loongarch64"))]
const IRQ_ROUTE_VALID: u64 = 1 << 63;
#[cfg(any(test, target_arch = "loongarch64"))]
const PCH_PIC_CPU_ROUTE_SLOTS: usize = 256;

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

#[cfg(any(test, target_arch = "loongarch64"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RouteEntry {
    input: usize,
    irq: IrqId,
}

#[cfg(any(test, target_arch = "loongarch64"))]
pub(super) struct PchPicCpuInterface {
    domain: IrqDomainId,
    controller: AcpiGsiController,
    controller_address: u64,
    base_vector: usize,
    vector_count: usize,
    routes: [AtomicU64; PCH_PIC_CPU_ROUTE_SLOTS],
}

#[cfg(any(test, target_arch = "loongarch64"))]
impl PchPicCpuInterface {
    pub(super) const fn new(
        domain: IrqDomainId,
        controller: AcpiGsiController,
        controller_address: u64,
        base_vector: usize,
        vector_count: usize,
    ) -> Self {
        Self {
            domain,
            controller,
            controller_address,
            base_vector,
            vector_count,
            routes: [const { AtomicU64::new(0) }; PCH_PIC_CPU_ROUTE_SLOTS],
        }
    }

    pub(super) fn remember_route(&self, route: &AcpiGsiRoute, irq: IrqId) -> Result<(), IrqError> {
        if !self.supports_acpi_gsi(route) {
            return Err(IrqError::Unsupported);
        }
        if irq.hwirq != HwIrq(u32::from(route.controller_input)) {
            return Err(IrqError::InvalidIrq);
        }

        let input = usize::from(route.controller_input);
        let encoded = encode_irq_id(irq);
        let slot = &self.routes[input];
        match slot.compare_exchange(0, encoded, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Ok(()),
            Err(existing) if existing == encoded => Ok(()),
            Err(_) => Err(IrqError::Busy),
        }
    }

    pub(super) fn irq_for_external_vector(&self, vector: usize) -> Option<IrqId> {
        let input = self.input_for_vector(vector)?;
        decode_irq_id(self.routes[input].load(Ordering::Acquire))
            .or_else(|| Some(IrqId::new(self.domain, HwIrq(input as u32))))
    }

    fn supports_acpi_gsi(&self, route: &AcpiGsiRoute) -> bool {
        route.controller == self.controller
            && route.controller_address == self.controller_address
            && self.valid_input(usize::from(route.controller_input))
    }

    fn input_for_vector(&self, vector: usize) -> Option<usize> {
        let input = vector.checked_sub(self.base_vector)?;
        self.valid_input(input).then_some(input)
    }

    fn valid_input(&self, input: usize) -> bool {
        input < self.vector_count && input < self.routes.len()
    }
}

#[cfg(any(test, target_arch = "loongarch64"))]
fn encode_irq_id(irq: IrqId) -> u64 {
    IRQ_ROUTE_VALID | ((u64::from(irq.domain.0)) << 32) | u64::from(irq.hwirq.0)
}

#[cfg(any(test, target_arch = "loongarch64"))]
fn decode_irq_id(encoded: u64) -> Option<IrqId> {
    if encoded & IRQ_ROUTE_VALID == 0 {
        return None;
    }

    let domain = IrqDomainId(((encoded >> 32) & u64::from(u16::MAX)) as u16);
    let hwirq = HwIrq((encoded & u64::from(u32::MAX)) as u32);
    Some(IrqId::new(domain, hwirq))
}

#[cfg(any(test, target_arch = "loongarch64"))]
pub(super) struct AcpiControllerRoutes {
    controller: AcpiGsiController,
    controller_address: u64,
    base_vector: usize,
    vector_count: usize,
    routes: Vec<RouteEntry>,
}

#[cfg(any(test, target_arch = "loongarch64"))]
impl AcpiControllerRoutes {
    pub(super) const fn new(
        controller: AcpiGsiController,
        controller_address: u64,
        base_vector: usize,
        vector_count: usize,
    ) -> Self {
        Self {
            controller,
            controller_address,
            base_vector,
            vector_count,
            routes: Vec::new(),
        }
    }

    pub(super) const fn vector_count(&self) -> usize {
        self.vector_count
    }

    pub(super) fn vector_for_input(&self, input: usize) -> Option<usize> {
        (input < self.vector_count).then_some(self.base_vector + input)
    }

    pub(super) fn input_for_vector(&self, vector: usize) -> Option<usize> {
        let input = vector.checked_sub(self.base_vector)?;
        (input < self.vector_count).then_some(input)
    }

    pub(super) fn supports_acpi_gsi(&self, route: &AcpiGsiRoute) -> bool {
        route.controller == self.controller
            && route.controller_address == self.controller_address
            && usize::from(route.controller_input) < self.vector_count
    }

    pub(super) fn remember_route(
        &mut self,
        route: &AcpiGsiRoute,
        irq: IrqId,
    ) -> Result<(), IrqError> {
        if !self.supports_acpi_gsi(route) {
            return Err(IrqError::Unsupported);
        }
        if irq.hwirq != HwIrq(u32::from(route.controller_input)) {
            return Err(IrqError::InvalidIrq);
        }
        if let Some(entry) = self
            .routes
            .iter()
            .find(|entry| entry.input == usize::from(route.controller_input))
        {
            return if entry.irq == irq {
                Ok(())
            } else {
                Err(IrqError::Busy)
            };
        }
        self.routes.push(RouteEntry {
            input: usize::from(route.controller_input),
            irq,
        });
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn irq_for_external_vector(&self, vector: usize) -> Option<IrqId> {
        let input = self.input_for_vector(vector)?;
        self.routes
            .iter()
            .find_map(|entry| (entry.input == input).then_some(entry.irq))
    }
}

#[cfg(test)]
mod tests {
    use rdif_intc::{AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger};

    use super::*;
    use crate::irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqDomainId, IrqError, IrqId, IrqSource};

    fn acpi_route(gsi: u32, input: u8) -> AcpiGsiRoute {
        AcpiGsiRoute {
            gsi,
            vector: rdrive::probe::acpi::PCI_INTX_VECTOR_BASE + gsi as usize,
            controller: AcpiGsiController::PchPic,
            controller_id: 1,
            controller_address: 0x1000_0000,
            controller_input: input,
            trigger: AcpiIrqTrigger::Level,
            polarity: AcpiIrqPolarity::ActiveLow,
        }
    }

    #[test]
    fn acpi_controller_reverse_route_uses_controller_input_not_acpi_vector() {
        let mut routes = AcpiControllerRoutes::new(AcpiGsiController::PchPic, 0x1000_0000, 0, 64);
        let route = acpi_route(82, 18);
        let irq = IrqId::new(IrqDomainId(42), HwIrq(18));

        routes.remember_route(&route, irq).unwrap();

        assert_eq!(routes.irq_for_external_vector(18), Some(irq));
        assert_eq!(routes.irq_for_external_vector(route.vector), None);
        assert_ne!(
            routes.irq_for_external_vector(18),
            Some(IrqId::new(IrqDomainId(42), HwIrq(82)))
        );
    }

    #[test]
    fn pch_pic_cpu_interface_resolves_external_vector_without_controller_device() {
        let cpu_if = PchPicCpuInterface::new(
            IrqId::new(IrqDomainId(42), HwIrq(0)).domain,
            AcpiGsiController::PchPic,
            0x1000_0000,
            0,
            64,
        );
        let route = acpi_route(82, 18);
        let irq = IrqId::new(IrqDomainId(42), HwIrq(18));

        cpu_if.remember_route(&route, irq).unwrap();

        assert_eq!(cpu_if.irq_for_external_vector(18), Some(irq));
        assert_eq!(
            cpu_if.irq_for_external_vector(19),
            Some(IrqId::new(IrqDomainId(42), HwIrq(19)))
        );
        assert_eq!(cpu_if.irq_for_external_vector(route.vector), None);
    }

    #[test]
    fn pch_pic_cpu_interface_rejects_out_of_range_input() {
        let cpu_if = PchPicCpuInterface::new(
            IrqId::new(IrqDomainId(42), HwIrq(0)).domain,
            AcpiGsiController::PchPic,
            0x1000_0000,
            0,
            64,
        );
        let route = AcpiGsiRoute {
            controller_input: 64,
            ..acpi_route(82, 18)
        };
        let irq = IrqId::new(IrqDomainId(42), HwIrq(64));

        assert_eq!(
            cpu_if.remember_route(&route, irq),
            Err(IrqError::Unsupported)
        );
        assert_eq!(cpu_if.irq_for_external_vector(64), None);
    }

    #[test]
    fn pch_pic_cpu_interface_does_not_store_acpi_vector_as_external_vector() {
        let domain = IrqId::new(IrqDomainId(42), HwIrq(0)).domain;
        let cpu_if =
            PchPicCpuInterface::new(domain, AcpiGsiController::PchPic, 0x1000_0000, 16, 64);
        let route = acpi_route(8, 18);
        let irq = IrqId::new(domain, HwIrq(18));

        cpu_if.remember_route(&route, irq).unwrap();

        assert_eq!(cpu_if.irq_for_external_vector(16 + 18), Some(irq));
        assert_eq!(
            cpu_if.irq_for_external_vector(route.vector),
            Some(IrqId::new(domain, HwIrq((route.vector - 16) as u32)))
        );
        assert_ne!(cpu_if.irq_for_external_vector(route.vector), Some(irq));
    }

    #[test]
    fn acpi_controller_acpi_route_keeps_hardware_vector_as_base_plus_input() {
        let mut routes = AcpiControllerRoutes::new(AcpiGsiController::PchPic, 0x1000_0000, 0, 64);
        let route = acpi_route(82, 18);
        let irq = IrqId::new(IrqDomainId(42), HwIrq(18));

        routes.remember_route(&route, irq).unwrap();

        assert_eq!(routes.vector_count(), 64);
        assert_eq!(routes.vector_for_input(18), Some(18));
        assert_eq!(routes.input_for_vector(18), Some(18));
        assert_ne!(routes.vector_for_input(18), Some(route.vector));
    }

    #[test]
    fn acpi_controller_route_rejects_unsupported_controller_and_collision() {
        let mut routes = AcpiControllerRoutes::new(AcpiGsiController::PchPic, 0x1000_0000, 0, 64);
        let route = acpi_route(82, 18);
        let irq = IrqId::new(IrqDomainId(42), HwIrq(18));

        routes.remember_route(&route, irq).unwrap();

        assert_eq!(
            routes.remember_route(&route, IrqId::new(IrqDomainId(43), HwIrq(18))),
            Err(IrqError::Busy)
        );
        assert_eq!(
            routes.remember_route(&route, IrqId::new(IrqDomainId(42), HwIrq(19))),
            Err(IrqError::InvalidIrq)
        );

        let unsupported = AcpiGsiRoute {
            controller: AcpiGsiController::IoApic,
            ..route
        };
        assert_eq!(
            routes.remember_route(&unsupported, irq),
            Err(IrqError::Unsupported)
        );

        let out_of_input_range = AcpiGsiRoute {
            controller_input: 64,
            ..route
        };
        assert_eq!(
            routes.remember_route(&out_of_input_range, irq),
            Err(IrqError::Unsupported)
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
