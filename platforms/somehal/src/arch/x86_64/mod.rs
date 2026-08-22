use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use rdif_intc::{AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger};
use rdrive::{
    module_driver,
    probe::{
        OnProbeError,
        acpi::{AcpiId, ProbeAcpi},
    },
};
use x86_apic_driver::{IoApicIntc, VirtAddr, ioapic::IoApicInfo};

use crate::{
    common::PlatOp,
    irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqDomainId, IrqError, IrqId, IrqSource, X86_LAPIC_DOMAIN},
};

mod lapic;
mod msi;
pub mod timer;
mod vector;

#[cfg(test)]
use vector::{APIC_ERROR_VECTOR, APIC_IPI_VECTOR, APIC_TIMER_VECTOR, ioapic_gsi_irq_id};
use vector::{
    SPURIOUS_VECTOR, lapic_ipi_irq_id, lapic_timer_irq_id, local_vector_irq_id,
    validate_external_vector,
};

const IRQ_ROUTE_VALID: u64 = 1 << 63;

static IOAPIC_CPU_IF: X86IoApicCpuInterface = X86IoApicCpuInterface::new();

pub struct Plat;

module_driver!(
    name: "ACPI IOAPIC",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[ProbeKind::Acpi {
        ids: &[AcpiId {
            hid: "ACPIIOAP",
            cids: &[],
        }],
        on_probe: probe_ioapic
    }],
);

struct X86IoApicCpuInterface {
    vector_routes: [AtomicU64; 256],
}

impl X86IoApicCpuInterface {
    const fn new() -> Self {
        Self {
            vector_routes: [const { AtomicU64::new(0) }; 256],
        }
    }

    fn remember_vector_route(&self, vector: usize, irq: IrqId) -> Result<u8, IrqError> {
        let vector_u8 = validate_external_vector(vector)?;
        let encoded = encode_irq_id(irq);
        let slot = &self.vector_routes[usize::from(vector_u8)];

        match slot.compare_exchange(0, encoded, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Ok(vector_u8),
            Err(existing) if existing == encoded => Ok(vector_u8),
            Err(_) => Err(IrqError::Busy),
        }
    }

    fn irq_for_vector(&self, vector: usize) -> Option<IrqId> {
        let vector = u8::try_from(vector).ok()?;
        decode_irq_id(self.vector_routes[usize::from(vector)].load(Ordering::Acquire))
    }

    fn forget_vector_route(&self, vector: usize, irq: IrqId) -> Result<(), IrqError> {
        let vector = validate_external_vector(vector)?;
        self.vector_routes[usize::from(vector)]
            .compare_exchange(encode_irq_id(irq), 0, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| IrqError::InvalidIrq)
    }
}

fn encode_irq_id(irq: IrqId) -> u64 {
    IRQ_ROUTE_VALID | ((u64::from(irq.domain.0)) << 32) | u64::from(irq.hwirq.0)
}

fn decode_irq_id(encoded: u64) -> Option<IrqId> {
    if encoded & IRQ_ROUTE_VALID == 0 {
        return None;
    }

    let domain = IrqDomainId(((encoded >> 32) & u64::from(u16::MAX)) as u16);
    let hwirq = HwIrq((encoded & u64::from(u32::MAX)) as u32);
    Some(IrqId::new(domain, hwirq))
}

fn probe_ioapic(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let ioapics = info.root.routing().io_apics();
    if ioapics.is_empty() {
        return Err(OnProbeError::NotMatch);
    }

    let owner = dev.descriptor.device_id();
    let domain = crate::irq::alloc_irq_domain(owner, crate::irq::IrqDomainKind::X86IoApic)
        .map_err(|err| OnProbeError::other(format!("failed to register IOAPIC domain: {err:?}")))?;
    let infos: Vec<IoApicInfo> = ioapics
        .iter()
        .copied()
        .map(|ioapic| IoApicInfo {
            id: ioapic.id,
            phys_address: u64::from(ioapic.address),
            // SAFETY: the boot mapping of the ACPI-reported chip address is
            // valid for the whole kernel lifetime and the redirection tables
            // are untouched until this probe masks them.
            mmio_base: VirtAddr::new(someboot::mem::phys_to_virt(ioapic.address as usize) as usize),
            gsi_base: ioapic.gsi_base,
        })
        .collect();
    let intc = unsafe { IoApicIntc::new(&infos) };
    dev.register(rdif_intc::Intc::new(domain, intc));
    let msi = msi::X86MsiProvider::new(owner).map_err(|err| {
        OnProbeError::other(format!("failed to register x86 MSI domain: {err:?}"))
    })?;
    dev.register(rdif_msi::Msi::new(msi.provider_id(), msi));
    Ok(())
}

impl PlatOp for Plat {
    type ActiveIrq = ActiveIrq;

    fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), IrqError> {
        if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            return Ok(());
        }

        if irq.domain == X86_LAPIC_DOMAIN {
            if irq.hwirq.0 == 0 {
                if enable {
                    timer::irq_enable();
                } else {
                    timer::irq_disable();
                }
                return Ok(());
            }
            return Err(IrqError::InvalidIrq);
        }

        if crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::X86IoApic) {
            let intc = crate::irq::intc_by_domain(irq.domain)?;
            let mut intc = intc.try_lock().map_err(|_| IrqError::Busy)?;
            return match intc.set_enabled(irq.hwirq, enable) {
                // The GSI was never routed through configure_acpi; resolve
                // its firmware routes on demand.
                Err(IrqError::NotFound) => enable_unconfigured_gsi(&mut intc, irq.hwirq.0, enable),
                other => other,
            };
        }
        if crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::X86Msi) {
            // MSI-X source masking is owned by the PCI endpoint table. The
            // parent domain only validates that the allocated vector exists.
            return IOAPIC_CPU_IF
                .irq_for_vector(irq.hwirq.0 as usize)
                .filter(|registered| *registered == irq)
                .map(|_| ())
                .ok_or(IrqError::InvalidIrq);
        }

        Err(IrqError::InvalidIrq)
    }

    fn irq_set_affinity(irq: IrqId, affinity: crate::irq::IrqAffinity) -> Result<(), IrqError> {
        if irq.domain == X86_LAPIC_DOMAIN || irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            return Err(IrqError::Unsupported);
        }
        if crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::X86Msi) {
            return msi::set_irq_affinity(irq, affinity);
        }
        if !crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::X86IoApic) {
            return Err(IrqError::InvalidIrq);
        }

        let dest = match affinity {
            crate::irq::IrqAffinity::Any => 0,
            crate::irq::IrqAffinity::Fixed { cpu_id } => {
                let Some(apic_id) = someboot::smp::cpu_idx_to_id(cpu_id) else {
                    return Err(IrqError::InvalidCpu);
                };
                u8::try_from(apic_id).map_err(|_| IrqError::InvalidCpu)?
            }
        };
        if set_ioapic_gsi_destination(irq.domain, irq.hwirq.0, dest)? {
            Ok(())
        } else {
            Err(IrqError::NotFound)
        }
    }

    fn send_ipi(irq: IrqId, target: crate::irq::IpiTarget) -> Result<(), IrqError> {
        let vector = lapic::ipi_vector(irq)?;

        match target {
            crate::irq::IpiTarget::Current => lapic::send_ipi(vector),
            crate::irq::IpiTarget::Cpu(cpu) => {
                let apic_id = someboot::smp::cpu_idx_to_id(cpu.0).ok_or(IrqError::InvalidCpu)?;
                lapic::send_ipi_to_apic_id(
                    u32::try_from(apic_id).map_err(|_| IrqError::InvalidCpu)?,
                    vector,
                )
            }
        }
    }

    fn ipi_irq() -> IrqId {
        lapic_ipi_irq_id()
    }

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        if raw == SPURIOUS_VECTOR {
            return None;
        }

        if let Some(irq) = local_vector_irq_id(raw) {
            return Some(ActiveIrq::new(irq));
        }

        match ioapic_irq_for_vector(raw) {
            Some(irq) => Some(ActiveIrq::new(irq)),
            None => {
                warn!("unrouted x86 interrupt vector {raw:#x}");
                lapic::eoi();
                None
            }
        }
    }

    fn active_irq_id(active: &Self::ActiveIrq) -> IrqId {
        active.id()
    }

    fn systick_irq() -> IrqId {
        lapic_timer_irq_id()
    }

    fn resolve_irq_source(source: IrqSource) -> Result<IrqId, IrqError> {
        match source {
            IrqSource::AcpiGsi(gsi) => resolve_acpi_gsi(gsi),
            IrqSource::AcpiGsiRoute(route) => resolve_acpi_route(route),
            IrqSource::ControllerLine { domain, hwirq }
                if crate::irq::domain_is_kind(domain, crate::irq::IrqDomainKind::X86IoApic) =>
            {
                Ok(IrqId::new(domain, hwirq))
            }
            IrqSource::ControllerLine { domain, hwirq } if domain == X86_LAPIC_DOMAIN => {
                Ok(IrqId::new(domain, hwirq))
            }
            IrqSource::ControllerLine { .. } => Err(IrqError::InvalidIrq),
        }
    }

    fn secondary_init() {}

    fn init_boot_irq_cpu(_cpu_idx: usize, _role: crate::irq::CpuBootRole) {}

    fn send_ipi_to_cpu(cpu_id: usize) -> Result<(), IrqError> {
        Self::send_ipi(
            lapic_ipi_irq_id(),
            crate::irq::IpiTarget::Cpu(crate::irq::CpuId(cpu_id)),
        )
    }
}

pub struct ActiveIrq {
    irq: IrqId,
}

impl ActiveIrq {
    const fn new(irq: IrqId) -> Self {
        Self { irq }
    }

    pub fn id(&self) -> IrqId {
        self.irq
    }
}

impl Drop for ActiveIrq {
    fn drop(&mut self) {
        lapic::eoi();
    }
}

fn resolve_acpi_gsi(gsi: u32) -> Result<IrqId, IrqError> {
    let route = firmware_gsi_routes(gsi)
        .into_iter()
        .next()
        .ok_or(IrqError::InvalidIrq)?;

    resolve_acpi_route(route_to_irq_framework(route))
}

fn resolve_acpi_route(route: irq_framework::AcpiGsiRoute) -> Result<IrqId, IrqError> {
    let route = route_to_rdif(route);
    let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::X86IoApic)
        .ok_or(IrqError::Unsupported)?;
    let intc = crate::irq::intc_by_domain(domain)?;
    let mut intc = intc.lock().map_err(|_| IrqError::Controller)?;

    if !intc.supports_acpi_gsi(&route) {
        return Err(IrqError::Unsupported);
    }

    let translation = intc.translate_acpi(&route)?;
    // Pin the vector route in the dispatch cache before programming hardware
    // so a conflicting route cannot claim an entry the cache already owns.
    IOAPIC_CPU_IF.remember_vector_route(route.vector, translation.id)?;
    match intc.configure_acpi(&translation, &route) {
        Ok(()) => Ok(translation.id),
        Err(err) => {
            let _ = IOAPIC_CPU_IF.forget_vector_route(route.vector, translation.id);
            Err(err)
        }
    }
}

/// Applies an enable/disable to a GSI that was never routed through
/// `configure_acpi`, resolving its firmware routes on demand.
fn enable_unconfigured_gsi(
    intc: &mut rdif_intc::Intc,
    gsi: u32,
    enable: bool,
) -> Result<(), IrqError> {
    let routes = firmware_gsi_routes(gsi);
    if routes.is_empty() {
        return Err(IrqError::InvalidIrq);
    }

    let ioapic = intc
        .typed_mut::<IoApicIntc>()
        .ok_or(IrqError::Unsupported)?;
    let mut applied = false;
    for route in &routes {
        applied |= ioapic.set_route_enable(route, enable);
    }
    if applied {
        Ok(())
    } else {
        Err(IrqError::InvalidIrq)
    }
}

fn firmware_gsi_routes(gsi: u32) -> Vec<AcpiGsiRoute> {
    rdrive::probe::acpi::with_acpi(|system| system.routing().resolve_gsi(gsi))
        .flatten()
        .into_iter()
        .collect()
}

fn route_to_irq_framework(route: AcpiGsiRoute) -> irq_framework::AcpiGsiRoute {
    irq_framework::AcpiGsiRoute {
        gsi: route.gsi,
        vector: route.vector,
        controller: match route.controller {
            rdif_intc::AcpiGsiController::IoApic => irq_framework::AcpiGsiController::IoApic,
            rdif_intc::AcpiGsiController::PchPic => irq_framework::AcpiGsiController::PchPic,
        },
        controller_id: route.controller_id,
        controller_address: route.controller_address,
        controller_input: route.controller_input,
        trigger: match route.trigger {
            AcpiIrqTrigger::Edge => irq_framework::AcpiIrqTrigger::Edge,
            AcpiIrqTrigger::Level => irq_framework::AcpiIrqTrigger::Level,
        },
        polarity: match route.polarity {
            AcpiIrqPolarity::ActiveHigh => irq_framework::AcpiIrqPolarity::ActiveHigh,
            AcpiIrqPolarity::ActiveLow => irq_framework::AcpiIrqPolarity::ActiveLow,
        },
    }
}

fn route_to_rdif(route: irq_framework::AcpiGsiRoute) -> AcpiGsiRoute {
    AcpiGsiRoute {
        gsi: route.gsi,
        vector: route.vector,
        controller: match route.controller {
            irq_framework::AcpiGsiController::IoApic => rdif_intc::AcpiGsiController::IoApic,
            irq_framework::AcpiGsiController::PchPic => rdif_intc::AcpiGsiController::PchPic,
        },
        controller_id: route.controller_id,
        controller_address: route.controller_address,
        controller_input: route.controller_input,
        trigger: match route.trigger {
            irq_framework::AcpiIrqTrigger::Edge => AcpiIrqTrigger::Edge,
            irq_framework::AcpiIrqTrigger::Level => AcpiIrqTrigger::Level,
        },
        polarity: match route.polarity {
            irq_framework::AcpiIrqPolarity::ActiveHigh => AcpiIrqPolarity::ActiveHigh,
            irq_framework::AcpiIrqPolarity::ActiveLow => AcpiIrqPolarity::ActiveLow,
        },
    }
}

fn set_ioapic_gsi_destination(
    domain: crate::irq::IrqDomainId,
    gsi: u32,
    dest: u8,
) -> Result<bool, IrqError> {
    let intc = crate::irq::intc_by_domain(domain)?;
    let mut intc = intc.try_lock().map_err(|_| IrqError::Busy)?;
    let ioapic = intc
        .typed_mut::<IoApicIntc>()
        .ok_or(IrqError::Unsupported)?;
    if ioapic.set_gsi_destination(gsi, dest) {
        return Ok(true);
    }
    // GSIs never routed through configure_acpi fall back to firmware routes.
    let routes = firmware_gsi_routes(gsi);
    let mut applied = false;
    for route in &routes {
        applied |= ioapic.set_route_destination(route, dest);
    }
    Ok(applied)
}

fn ioapic_irq_for_vector(vector: usize) -> Option<IrqId> {
    IOAPIC_CPU_IF.irq_for_vector(vector)
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

    #[test]
    fn lapic_timer_and_ioapic_gsi_zero_are_different_irq_domains() {
        assert_eq!(lapic_timer_irq_id().domain, X86_LAPIC_DOMAIN);
        assert_ne!(lapic_timer_irq_id(), ioapic_gsi_irq_id(0));
    }

    #[test]
    fn lapic_ipi_vector_is_cpu_local_not_ioapic_gsi() {
        let irq = lapic_ipi_irq_id();
        assert_eq!(irq.domain, CPU_LOCAL_IRQ_DOMAIN);
        assert_eq!(local_vector_irq_id(APIC_IPI_VECTOR), Some(irq));
        assert_ne!(
            irq,
            ioapic_gsi_irq_id((APIC_IPI_VECTOR - rdrive::probe::acpi::PCI_INTX_VECTOR_BASE) as u32)
        );
    }

    #[test]
    fn ioapic_gsi_irq_ids_preserve_host_gsi_as_hwirq() {
        assert_eq!(ioapic_gsi_irq_id(4).hwirq, HwIrq(4));
        assert_eq!(ioapic_gsi_irq_id(18).hwirq, HwIrq(18));
    }

    #[test]
    fn ioapic_cpu_interface_resolves_vector_without_controller_device() {
        let vector = rdrive::probe::acpi::PCI_INTX_VECTOR_BASE + 5;
        let irq = ioapic_gsi_irq_id(21);
        let cpu_if = X86IoApicCpuInterface::new();

        cpu_if.remember_vector_route(vector, irq).unwrap();

        assert_eq!(cpu_if.irq_for_vector(vector), Some(irq));
        assert_eq!(cpu_if.irq_for_vector(vector + 1), None);
    }

    #[test]
    fn ioapic_cpu_interface_rejects_vector_conflicts() {
        let vector = rdrive::probe::acpi::PCI_INTX_VECTOR_BASE + 6;
        let irq = ioapic_gsi_irq_id(22);
        let conflicting = ioapic_gsi_irq_id(23);
        let cpu_if = X86IoApicCpuInterface::new();

        assert_eq!(cpu_if.remember_vector_route(vector, irq), Ok(vector as u8));
        assert_eq!(cpu_if.remember_vector_route(vector, irq), Ok(vector as u8));
        assert_eq!(
            cpu_if.remember_vector_route(vector, conflicting),
            Err(IrqError::Busy)
        );
        assert_eq!(cpu_if.irq_for_vector(vector), Some(irq));
    }

    #[test]
    fn unknown_vector_is_not_dispatched_as_cpu_local_irq() {
        let vector = 0x71;

        assert_eq!(local_vector_irq_id(vector), None);
    }

    #[test]
    fn spurious_vector_is_not_dispatched() {
        assert_eq!(local_vector_irq_id(SPURIOUS_VECTOR), None);
    }

    #[test]
    fn vector_route_rejects_reserved_out_of_range_and_collision() {
        let cpu_if = X86IoApicCpuInterface::new();
        assert_eq!(
            cpu_if.remember_vector_route(APIC_TIMER_VECTOR, ioapic_gsi_irq_id(1)),
            Err(IrqError::Busy)
        );
        assert_eq!(
            cpu_if.remember_vector_route(APIC_IPI_VECTOR, ioapic_gsi_irq_id(1)),
            Err(IrqError::Busy)
        );
        assert_eq!(
            cpu_if.remember_vector_route(APIC_ERROR_VECTOR, ioapic_gsi_irq_id(1)),
            Err(IrqError::Busy)
        );
        assert_eq!(
            cpu_if.remember_vector_route(SPURIOUS_VECTOR, ioapic_gsi_irq_id(1)),
            Err(IrqError::Busy)
        );
        assert_eq!(
            cpu_if.remember_vector_route(0x1f, ioapic_gsi_irq_id(1)),
            Err(IrqError::Busy)
        );
        assert_eq!(
            cpu_if.remember_vector_route(usize::from(u8::MAX) + 1, ioapic_gsi_irq_id(1)),
            Err(IrqError::InvalidIrq)
        );

        let vector = 0x72;
        let irq = ioapic_gsi_irq_id(7);
        cpu_if.remember_vector_route(vector, irq).unwrap();
        assert_eq!(
            cpu_if.remember_vector_route(vector, ioapic_gsi_irq_id(8)),
            Err(IrqError::Busy)
        );
        assert_eq!(cpu_if.remember_vector_route(vector, irq), Ok(vector as u8));
    }

    #[test]
    fn ipi_vector_requires_runtime_ipi_irq_identity() {
        assert_eq!(
            lapic::ipi_vector(lapic_ipi_irq_id()),
            Ok(APIC_IPI_VECTOR as u8)
        );
        assert_eq!(
            lapic::ipi_vector(lapic_timer_irq_id()),
            Err(IrqError::InvalidIrq)
        );
        assert_eq!(
            lapic::ipi_vector(IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(0x41))),
            Err(IrqError::InvalidIrq)
        );
    }

    #[test]
    fn acpi_route_conversion_preserves_trigger_and_polarity() {
        let route = AcpiGsiRoute {
            gsi: 10,
            vector: 0x3a,
            controller: rdif_intc::AcpiGsiController::IoApic,
            controller_id: 0,
            controller_address: 0xfec0_0000,
            controller_input: 10,
            trigger: AcpiIrqTrigger::Level,
            polarity: AcpiIrqPolarity::ActiveLow,
        };

        assert_eq!(route_to_rdif(route_to_irq_framework(route)), route);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[test]
    fn irq_route_constant_marks_valid_entries() {
        assert_eq!(IRQ_ROUTE_VALID, 1 << 63);
    }
}
