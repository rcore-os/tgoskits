//! `rdif-intc` capability implementation for the x86 ACPI GSI domain.
//!
//! [`IoApicIntc`] aggregates every I/O APIC chip discovered in the MADT and
//! implements the [`rdif_intc::Interface`] contract, so the OS glue can
//! register it through `rdrive` exactly like `arm-gic-driver`:
//!
//! ```text
//! glue: probe MADT -> ioremap -> IoApicIntc::new(&infos)
//!     -> alloc_irq_domain -> dev.register(rdif_intc::Intc::new(domain, intc))
//! ```
//!
//! Firmware route resolution (ACPI GSI -> controller/input/trigger lookup)
//! and the vector-to-IRQ dispatch cache stay in the OS glue; this driver only
//! remembers the routes explicitly configured through it. [`IrqError::NotFound`]
//! from [`Interface::set_enabled`] therefore means "GSI not configured
//! through this controller yet" and is the glue's signal to fall back to
//! firmware-resolved routes.

use alloc::vec::Vec;

use rdif_intc::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, ControllerIrqTranslation,
    DriverGeneric, HwIrq, Interface, IrqError, IrqTranslation,
};

use crate::ioapic::{IoApicInfo, PinPolarity, TriggerMode, X86IoApic};

/// Multi-chip I/O APIC interrupt controller for one ACPI GSI domain.
pub struct IoApicIntc {
    ioapics: Vec<X86IoApic>,
    routes: Vec<AcpiGsiRoute>,
    destinations: Vec<(usize, u8)>,
}

impl IoApicIntc {
    /// Probes and masks every listed I/O APIC chip.
    ///
    /// # Safety
    ///
    /// Every `info.mmio_base` must be the mapped register page of the chip it
    /// describes, and no other code may race the redirection tables during
    /// the probe.
    pub unsafe fn new(ioapics: &[IoApicInfo]) -> Self {
        Self {
            ioapics: ioapics
                .iter()
                .copied()
                .map(|info| unsafe { X86IoApic::new(info) })
                .collect(),
            routes: Vec::new(),
            destinations: Vec::new(),
        }
    }

    /// Programs one firmware GSI route, masked (`enable == false`) or armed.
    ///
    /// Returns `false` when no chip in this controller owns the route or the
    /// route's vector cannot be programmed.
    pub fn set_route_enable(&mut self, route: &AcpiGsiRoute, enable: bool) -> bool {
        let Ok(vector) = u8::try_from(route.vector) else {
            return false;
        };
        let destination = self.destination_for_vector(route.vector);
        let Some(ioapic) = self.ioapic_for_route_mut(route) else {
            return false;
        };

        let programmed = ioapic.program_input_masked(
            route.controller_input,
            vector,
            trigger_mode(route.trigger),
            pin_polarity(route.polarity),
            destination,
        );
        match programmed {
            Ok(()) if enable => ioapic.unmask_input(route.controller_input).is_ok(),
            Ok(()) => true,
            Err(_) => false,
        }
    }

    /// Retargets every remembered route of `gsi` to `destination` (APIC id).
    ///
    /// Returns `false` when the GSI has no remembered routes or no owning
    /// chip accepted the change.
    pub fn set_gsi_destination(&mut self, gsi: u32, destination: u8) -> bool {
        let routes: Vec<_> = self
            .routes
            .iter()
            .copied()
            .filter(|r| r.gsi == gsi)
            .collect();
        let mut applied = false;
        for route in routes {
            applied |= self.set_route_destination(&route, destination);
        }
        applied
    }

    /// Retargets one firmware GSI route to `destination`, remembering it so
    /// later reprogramming keeps the target.
    pub fn set_route_destination(&mut self, route: &AcpiGsiRoute, destination: u8) -> bool {
        let Some(ioapic) = self.ioapic_for_route_mut(route) else {
            return false;
        };
        if ioapic
            .set_input_destination(route.controller_input, destination)
            .is_err()
        {
            return false;
        }
        self.remember_destination(route.vector, destination);
        true
    }

    fn ioapic_for_route_mut(&mut self, route: &AcpiGsiRoute) -> Option<&mut X86IoApic> {
        self.ioapics
            .iter_mut()
            .find(|ioapic| chip_owns_route(ioapic, route))
    }

    fn remember_route(&mut self, route: AcpiGsiRoute) {
        if let Some(existing) = self.routes.iter_mut().find(|r| {
            r.controller_id == route.controller_id
                && r.controller_address == route.controller_address
                && r.gsi == route.gsi
        }) {
            *existing = route;
        } else {
            self.routes.push(route);
        }
    }

    fn remember_destination(&mut self, vector: usize, destination: u8) {
        if let Some((_, existing)) = self
            .destinations
            .iter_mut()
            .find(|(known_vector, _)| *known_vector == vector)
        {
            *existing = destination;
        } else {
            self.destinations.push((vector, destination));
        }
    }

    fn destination_for_vector(&self, vector: usize) -> u8 {
        self.destinations
            .iter()
            .find_map(|(known_vector, dest)| (*known_vector == vector).then_some(*dest))
            .unwrap_or(0)
    }
}

/// Returns whether `route` addresses this chip by ACPI controller identity
/// and GSI range.
fn chip_owns_route(ioapic: &X86IoApic, route: &AcpiGsiRoute) -> bool {
    let info = ioapic.info();
    u16::from(info.id) == route.controller_id
        && info.phys_address == route.controller_address
        && ioapic.contains_gsi(route.gsi)
}

fn trigger_mode(trigger: AcpiIrqTrigger) -> TriggerMode {
    match trigger {
        AcpiIrqTrigger::Edge => TriggerMode::Edge,
        AcpiIrqTrigger::Level => TriggerMode::Level,
    }
}

fn pin_polarity(polarity: AcpiIrqPolarity) -> PinPolarity {
    match polarity {
        AcpiIrqPolarity::ActiveHigh => PinPolarity::ActiveHigh,
        AcpiIrqPolarity::ActiveLow => PinPolarity::ActiveLow,
    }
}

impl DriverGeneric for IoApicIntc {
    fn name(&self) -> &str {
        "x86 ACPI IOAPIC"
    }
}

impl Interface for IoApicIntc {
    fn supports_acpi_gsi(&self, route: &AcpiGsiRoute) -> bool {
        route.controller == AcpiGsiController::IoApic
            && self
                .ioapics
                .iter()
                .any(|ioapic| chip_owns_route(ioapic, route))
    }

    fn translate_acpi(&self, route: &AcpiGsiRoute) -> Result<ControllerIrqTranslation, IrqError> {
        if !self.supports_acpi_gsi(route) {
            return Err(IrqError::Unsupported);
        }
        Ok(ControllerIrqTranslation::new(HwIrq(route.gsi)))
    }

    fn configure_acpi(
        &mut self,
        translation: &IrqTranslation,
        route: &AcpiGsiRoute,
    ) -> Result<(), IrqError> {
        if translation.id.hwirq != HwIrq(route.gsi) {
            return Err(IrqError::InvalidIrq);
        }
        self.remember_route(*route);
        if self.set_route_enable(route, false) {
            Ok(())
        } else {
            Err(IrqError::Unsupported)
        }
    }

    fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
        let routes: Vec<_> = self
            .routes
            .iter()
            .copied()
            .filter(|r| r.gsi == hwirq.0)
            .collect();
        if routes.is_empty() {
            // The GSI was never configured through this controller; the glue
            // decides whether to fall back to firmware-resolved routes.
            return Err(IrqError::NotFound);
        }

        let mut applied = false;
        for route in routes {
            applied |= self.set_route_enable(&route, enabled);
        }
        if applied {
            Ok(())
        } else {
            Err(IrqError::InvalidIrq)
        }
    }
}

#[cfg(test)]
mod tests {
    use rdif_intc::{HwIrq, IrqDomainId, IrqId, IrqTranslation};

    use super::*;

    fn empty_intc() -> IoApicIntc {
        // An empty chip list performs no hardware access, so the aggregate's
        // bookkeeping can be verified on the host.
        unsafe { IoApicIntc::new(&[]) }
    }

    fn sample_route(gsi: u32) -> AcpiGsiRoute {
        AcpiGsiRoute {
            gsi,
            vector: 0x3a,
            controller: AcpiGsiController::IoApic,
            controller_id: 0,
            controller_address: 0xfec0_0000,
            controller_input: gsi as u8,
            trigger: AcpiIrqTrigger::Level,
            polarity: AcpiIrqPolarity::ActiveLow,
        }
    }

    #[test]
    fn unknown_routes_are_unsupported_without_chips() {
        let intc = empty_intc();

        assert!(!intc.supports_acpi_gsi(&sample_route(4)));
        assert_eq!(
            intc.translate_acpi(&sample_route(4)),
            Err(IrqError::Unsupported)
        );
    }

    #[test]
    fn configure_acpi_rejects_translation_hwirq_mismatch() {
        let mut intc = empty_intc();
        let route = sample_route(9);
        let mismatched = IrqTranslation::new(IrqId::new(IrqDomainId(2), HwIrq(8)));

        assert_eq!(
            intc.configure_acpi(&mismatched, &route),
            Err(IrqError::InvalidIrq)
        );
    }

    #[test]
    fn configure_acpi_without_an_owning_chip_is_unsupported() {
        let mut intc = empty_intc();
        let route = sample_route(9);
        let translation = IrqTranslation::new(IrqId::new(IrqDomainId(2), HwIrq(9)));

        assert_eq!(
            intc.configure_acpi(&translation, &route),
            Err(IrqError::Unsupported)
        );
    }

    #[test]
    fn set_enabled_reports_unconfigured_gsis_as_not_found() {
        let mut intc = empty_intc();

        assert_eq!(intc.set_enabled(HwIrq(11), true), Err(IrqError::NotFound));
    }

    #[test]
    fn set_gsi_destination_without_routes_changes_nothing() {
        let mut intc = empty_intc();

        assert!(!intc.set_gsi_destination(11, 3));
    }
}
