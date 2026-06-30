#![cfg(any(test, target_arch = "loongarch64"))]

use alloc::vec::Vec;

use rdif_intc::{AcpiGsiController, AcpiGsiRoute};

use crate::irq::{HwIrq, IrqError, IrqId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RawIrq {
    Timer,
    Ipi,
    External,
    Unknown,
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExternalVectorResolveFailure {
    KeepPending,
    Complete,
}

pub(super) const fn external_vector_failure_policy(err: IrqError) -> ExternalVectorResolveFailure {
    if matches!(err, IrqError::Busy) {
        ExternalVectorResolveFailure::KeepPending
    } else {
        ExternalVectorResolveFailure::Complete
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RouteEntry {
    input: usize,
    irq: IrqId,
}

pub(super) struct AcpiControllerRoutes {
    controller: AcpiGsiController,
    controller_address: u64,
    base_vector: usize,
    vector_count: usize,
    routes: Vec<RouteEntry>,
}

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
    use crate::irq::{HwIrq, IrqDomainId, IrqError, IrqId};

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
}
