//! Optional PC/AT legacy-PIC transport for a firmware-less COM1 console.

use ax_sync::SpinLock;
use x86_apic_driver::legacy_pic::X86LegacyPic;

use crate::irq::{AcpiGsiController, AcpiGsiRoute, HwIrq, IrqDomainKind, IrqError, IrqId};

pub(super) const COM1_IRQ: u8 = 4;
const COM1_VECTOR: usize = 0x30 + COM1_IRQ as usize;

// SAFETY: this is the only host owner of the standard PC/AT PIC ports. The
// explicit build-time policy prevents the ordinary IOAPIC console transport
// from operating the same legacy controller.
static LEGACY_PIC: SpinLock<X86LegacyPic> = SpinLock::new(unsafe { X86LegacyPic::new() });
static LEGACY_PIC_DOMAIN_INIT: SpinLock<()> = SpinLock::new(());

pub(super) fn enabled() -> bool {
    matches!(option_env!("AX_X86_LEGACY_PIC_CONSOLE"), Some("1"))
}

pub(super) fn selected_for_vector(vector: usize) -> bool {
    enabled() && vector == COM1_VECTOR
}

pub(super) fn resolve_acpi_route(route: &AcpiGsiRoute) -> Option<Result<IrqId, IrqError>> {
    (selected_for_vector(route.vector)
        && route.gsi == u32::from(COM1_IRQ)
        && route.controller == AcpiGsiController::IoApic)
        .then(console_irq_id)
}

pub(super) fn owns_console_irq(irq: IrqId) -> bool {
    enabled()
        && irq.hwirq == HwIrq(u32::from(COM1_IRQ))
        && crate::irq::domain_is_kind(irq.domain, IrqDomainKind::X86LegacyPic)
}

pub(super) fn console_irq_id() -> Result<IrqId, IrqError> {
    if !enabled() {
        return Err(IrqError::Unsupported);
    }
    let domain = match crate::irq::domain_by_kind_fast(IrqDomainKind::X86LegacyPic) {
        Some(domain) => domain,
        None => {
            let _guard = LEGACY_PIC_DOMAIN_INIT.lock_irqsave();
            match crate::irq::domain_by_kind_fast(IrqDomainKind::X86LegacyPic) {
                Some(domain) => domain,
                None => crate::irq::alloc_irq_domain(
                    rdrive::DeviceId::new(),
                    IrqDomainKind::X86LegacyPic,
                )?,
            }
        }
    };
    Ok(IrqId::new(domain, HwIrq(u32::from(COM1_IRQ))))
}

pub(super) fn irq_for_vector(vector: usize) -> Option<IrqId> {
    if !selected_for_vector(vector) {
        return None;
    }
    let domain = crate::irq::domain_by_kind_fast(IrqDomainKind::X86LegacyPic)?;
    Some(IrqId::new(domain, HwIrq(u32::from(COM1_IRQ))))
}

pub(super) fn set_console_irq_enabled(enabled: bool) -> Result<(), IrqError> {
    if enabled {
        super::lapic::set_lint0_extint(true)?;
        LEGACY_PIC
            .lock_irqsave()
            .set_irq_enabled(COM1_IRQ, true)
            .map_err(map_pic_error)
    } else {
        LEGACY_PIC
            .lock_irqsave()
            .set_irq_enabled(COM1_IRQ, false)
            .map_err(map_pic_error)?;
        super::lapic::set_lint0_extint(false)
    }
}

pub(super) fn eoi(irq: u8) -> Result<(), IrqError> {
    LEGACY_PIC.lock_irqsave().eoi(irq).map_err(map_pic_error)
}

fn map_pic_error(error: x86_apic_driver::ApicError) -> IrqError {
    match error {
        x86_apic_driver::ApicError::InvalidLegacyPicIrq(_) => IrqError::InvalidIrq,
        _ => IrqError::Controller,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_vector_maps_only_when_legacy_transport_is_selected() {
        if enabled() {
            let irq = console_irq_id().unwrap();
            assert_eq!(irq_for_vector(COM1_VECTOR), Some(irq));
            assert!(crate::irq::domain_is_kind(
                irq.domain,
                IrqDomainKind::X86LegacyPic
            ));
            assert_eq!(irq_for_vector(COM1_VECTOR + 1), None);
        } else {
            assert_eq!(irq_for_vector(COM1_VECTOR), None);
        }
    }

    #[test]
    fn spcr_style_acpi_route_follows_the_explicit_console_transport() {
        let route = AcpiGsiRoute {
            gsi: u32::from(COM1_IRQ),
            vector: COM1_VECTOR,
            controller: AcpiGsiController::IoApic,
            controller_id: 2,
            controller_address: 0xfec0_0000,
            controller_input: COM1_IRQ,
            trigger: crate::irq::AcpiIrqTrigger::Edge,
            polarity: crate::irq::AcpiIrqPolarity::ActiveHigh,
        };

        if enabled() {
            let irq = resolve_acpi_route(&route).unwrap().unwrap();
            assert!(crate::irq::domain_is_kind(
                irq.domain,
                IrqDomainKind::X86LegacyPic
            ));
        } else {
            assert!(resolve_acpi_route(&route).is_none());
        }
    }
}
