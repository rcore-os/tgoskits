use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use rdif_intc::{AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger};
use rdrive::{
    DriverGeneric, module_driver,
    probe::{
        OnProbeError,
        acpi::{AcpiId, AcpiIoApic, ProbeAcpi},
    },
};
use x2apic::ioapic::{IoApic, IrqFlags, IrqMode};

use crate::{
    common::PlatOp,
    irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqDomainId, IrqError, IrqId, IrqSource, X86_LAPIC_DOMAIN},
};

mod lapic;
mod vector;

#[cfg(test)]
use vector::{APIC_IPI_VECTOR, APIC_TIMER_VECTOR, ioapic_gsi_irq_id};
use vector::{
    SPURIOUS_VECTOR, lapic_ipi_irq_id, lapic_timer_irq_id, local_vector_irq_id,
    validate_external_vector,
};

const MASKED_IOAPIC_PLACEHOLDER_VECTOR: u8 = 0x21;
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

struct X86IoApicIntc {
    ioapics: Vec<X86IoApic>,
    routes: Vec<AcpiGsiRoute>,
    vector_routes: Vec<(usize, IrqId)>,
    destinations: Vec<(usize, u8)>,
}

impl X86IoApicIntc {
    fn new(ioapics: &[AcpiIoApic]) -> Self {
        Self {
            ioapics: ioapics.iter().copied().map(X86IoApic::new).collect(),
            routes: Vec::new(),
            vector_routes: Vec::new(),
            destinations: Vec::new(),
        }
    }

    fn remember_vector_route(&mut self, vector: usize, irq: IrqId) -> Result<u8, IrqError> {
        let vector_u8 = validate_external_vector(vector)?;
        if let Some((_, existing)) = self
            .vector_routes
            .iter_mut()
            .find(|(known_vector, _)| *known_vector == vector)
        {
            if *existing == irq {
                return Ok(vector_u8);
            }
            return Err(IrqError::Busy);
        }

        IOAPIC_CPU_IF.remember_vector_route(vector, irq)?;
        self.vector_routes.push((vector, irq));
        Ok(vector_u8)
    }

    #[cfg(test)]
    fn irq_for_vector(&self, vector: usize) -> Option<IrqId> {
        self.vector_routes
            .iter()
            .find_map(|(known_vector, irq)| (*known_vector == vector).then_some(*irq))
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

    fn routes_for_gsi(&self, gsi: u32) -> Vec<AcpiGsiRoute> {
        let routes: Vec<_> = self
            .routes
            .iter()
            .copied()
            .filter(|r| r.gsi == gsi)
            .collect();
        if !routes.is_empty() {
            return routes;
        }

        rdrive::probe::acpi::with_acpi(|system| system.routing().resolve_gsi(gsi))
            .flatten()
            .into_iter()
            .collect()
    }

    fn set_gsi_enable(&mut self, gsi: u32, enable: bool) -> bool {
        let routes = self.routes_for_gsi(gsi);
        if routes.is_empty() {
            return false;
        }

        let mut applied = false;
        for route in routes {
            applied |= self.set_route_enable(&route, enable);
        }
        applied
    }

    fn set_route_enable(&mut self, route: &AcpiGsiRoute, enable: bool) -> bool {
        let dest = self.destination_for_vector(route.vector);
        for ioapic in &mut self.ioapics {
            if ioapic.contains_route(route) {
                return ioapic.set_route_enable(route, enable, dest).is_ok();
            }
        }
        false
    }

    fn remember_destination(&mut self, vector: usize, dest: u8) {
        if let Some((_, existing)) = self
            .destinations
            .iter_mut()
            .find(|(known_vector, _)| *known_vector == vector)
        {
            *existing = dest;
        } else {
            self.destinations.push((vector, dest));
        }
    }

    fn set_gsi_destination(&mut self, gsi: u32, dest: u8) -> bool {
        let routes = self.routes_for_gsi(gsi);
        if routes.is_empty() {
            return false;
        }

        let mut applied = false;
        for route in routes {
            let mut route_applied = false;
            for ioapic in &mut self.ioapics {
                if ioapic.contains_route(&route) {
                    ioapic.set_route_destination(&route, dest);
                    route_applied = true;
                    break;
                }
            }
            if route_applied {
                self.remember_destination(route.vector, dest);
                applied = true;
            }
        }
        applied
    }

    fn destination_for_vector(&self, vector: usize) -> u8 {
        self.destinations
            .iter()
            .find_map(|(known_vector, dest)| (*known_vector == vector).then_some(*dest))
            .unwrap_or(0)
    }
}

struct X86IoApic {
    info: AcpiIoApic,
    ioapic: IoApic,
}

impl X86IoApic {
    fn new(info: AcpiIoApic) -> Self {
        let ioapic_base = someboot::mem::phys_to_virt(info.address as usize) as u64;
        let mut ioapic = unsafe { IoApic::new(ioapic_base) };
        let max_entry = unsafe { ioapic.max_table_entry() };
        let redirection_entries = max_entry.saturating_add(1);

        unsafe {
            ioapic.init(MASKED_IOAPIC_PLACEHOLDER_VECTOR);
            for input in 0..=max_entry {
                let mut entry = ioapic.table_entry(input);
                entry.set_flags(entry.flags() | IrqFlags::MASKED);
                ioapic.set_table_entry(input, entry);
            }
        }

        info!(
            "ACPI IOAPIC initialized: id={} base={:#x} gsi_base={} entries={}",
            info.id, info.address, info.gsi_base, redirection_entries
        );

        Self {
            info: AcpiIoApic {
                redirection_entries,
                ..info
            },
            ioapic,
        }
    }

    fn contains(&self, gsi: u32) -> bool {
        let start = self.info.gsi_base;
        let end = start.saturating_add(u32::from(self.info.redirection_entries));
        (start..end).contains(&gsi)
    }

    fn contains_route(&self, route: &AcpiGsiRoute) -> bool {
        u16::from(self.info.id) == route.controller_id
            && u64::from(self.info.address) == route.controller_address
            && self.contains(route.gsi)
    }

    fn set_route_enable(
        &mut self,
        route: &AcpiGsiRoute,
        enable: bool,
        dest: u8,
    ) -> Result<(), IrqError> {
        if !self.contains_route(route) {
            return Err(IrqError::InvalidIrq);
        }
        let vector = u8::try_from(route.vector).map_err(|_| IrqError::InvalidIrq)?;

        unsafe {
            let input = route.controller_input;
            let mut entry = self.ioapic.table_entry(input);
            entry.set_vector(vector);
            entry.set_mode(IrqMode::Fixed);
            entry.set_flags(intx_flags(route.trigger, route.polarity) | IrqFlags::MASKED);
            entry.set_dest(dest);
            self.ioapic.set_table_entry(input, entry);

            if enable {
                self.ioapic.enable_irq(input);
            }
        }
        Ok(())
    }

    fn set_route_destination(&mut self, route: &AcpiGsiRoute, dest: u8) {
        if !self.contains_route(route) {
            return;
        }

        unsafe {
            let input = route.controller_input;
            let mut entry = self.ioapic.table_entry(input);
            entry.set_dest(dest);
            self.ioapic.set_table_entry(input, entry);
        }
    }
}

impl DriverGeneric for X86IoApicIntc {
    fn name(&self) -> &str {
        "x86 ACPI IOAPIC"
    }
}

impl rdif_intc::Interface for X86IoApicIntc {
    fn supports_acpi_gsi(&self, route: &AcpiGsiRoute) -> bool {
        route.controller == rdif_intc::AcpiGsiController::IoApic
            && self
                .ioapics
                .iter()
                .any(|ioapic| ioapic.contains_route(route))
    }

    fn translate_acpi(
        &self,
        route: &AcpiGsiRoute,
    ) -> Result<rdif_intc::ControllerIrqTranslation, IrqError> {
        if !self.supports_acpi_gsi(route) {
            return Err(IrqError::Unsupported);
        }
        Ok(rdif_intc::ControllerIrqTranslation::new(HwIrq(route.gsi)))
    }

    fn configure_acpi(
        &mut self,
        translation: &rdif_intc::IrqTranslation,
        route: &AcpiGsiRoute,
    ) -> Result<(), IrqError> {
        if translation.id.hwirq != HwIrq(route.gsi) {
            return Err(IrqError::InvalidIrq);
        }
        self.remember_vector_route(route.vector, translation.id)?;
        self.remember_route(*route);
        if self.set_route_enable(route, false) {
            Ok(())
        } else {
            Err(IrqError::Unsupported)
        }
    }

    fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
        if self.set_gsi_enable(hwirq.0, enabled) {
            Ok(())
        } else {
            Err(IrqError::InvalidIrq)
        }
    }
}

fn probe_ioapic(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let ioapics = info.root.routing().io_apics();
    if ioapics.is_empty() {
        return Err(OnProbeError::NotMatch);
    }

    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::X86IoApic,
    )
    .map_err(|err| OnProbeError::other(format!("failed to register IOAPIC domain: {err:?}")))?;
    dev.register(rdif_intc::Intc::new(domain, X86IoApicIntc::new(ioapics)));
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
                someboot::irq::irq_set_enable(someboot::irq::systimer_irq(), enable);
                return Ok(());
            }
            return Err(IrqError::InvalidIrq);
        }

        if crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::X86IoApic) {
            let intc = crate::irq::intc_by_domain(irq.domain)?;
            let mut intc = intc.try_lock().map_err(|_| IrqError::Busy)?;
            return intc.set_enabled(irq.hwirq, enable);
        }

        Err(IrqError::InvalidIrq)
    }

    fn irq_set_affinity(irq: IrqId, affinity: crate::irq::IrqAffinity) -> Result<(), IrqError> {
        if irq.domain == X86_LAPIC_DOMAIN || irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            return Err(IrqError::Unsupported);
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

    fn send_ipi(irq: IrqId, target: crate::irq::IpiTarget) {
        let Ok(vector) = lapic::ipi_vector(irq) else {
            warn!("refuse to send non-runtime IPI IRQ {irq:?}");
            return;
        };

        let result = match target {
            crate::irq::IpiTarget::Current { .. } => lapic::send_ipi(
                0,
                lapic::ICR_FIXED_BASE | lapic::ICR_DEST_SELF | u32::from(vector),
            ),
            crate::irq::IpiTarget::Other { cpu_id } => {
                let Some(apic_id) = someboot::smp::cpu_idx_to_id(cpu_id) else {
                    warn!("failed to resolve CPU index {cpu_id} to APIC ID");
                    return;
                };
                lapic::send_ipi_to_apic_id(
                    apic_id as u32,
                    lapic::ICR_FIXED_BASE | u32::from(vector),
                )
            }
            crate::irq::IpiTarget::AllExceptCurrent { .. } => lapic::send_ipi(
                0,
                lapic::ICR_FIXED_BASE | lapic::ICR_DEST_ALL_EXCLUDING_SELF | u32::from(vector),
            ),
        };

        if let Err(err) = result {
            warn!("failed to send runtime IPI vector {vector:#x}: {err:?}");
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

    fn send_ipi_to_cpu(cpu_id: usize) {
        Self::send_ipi(lapic_ipi_irq_id(), crate::irq::IpiTarget::Other { cpu_id });
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
    let route = rdrive::probe::acpi::with_acpi(|system| system.routing().resolve_gsi(gsi))
        .flatten()
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
    intc.configure_acpi(&translation, &route)?;
    Ok(translation.id)
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
        .typed_mut::<X86IoApicIntc>()
        .ok_or(IrqError::Unsupported)?;
    Ok(ioapic.set_gsi_destination(gsi, dest))
}

fn ioapic_irq_for_vector(vector: usize) -> Option<IrqId> {
    IOAPIC_CPU_IF.irq_for_vector(vector)
}

fn intx_flags(trigger: AcpiIrqTrigger, polarity: AcpiIrqPolarity) -> IrqFlags {
    let mut flags = IrqFlags::empty();
    if trigger == AcpiIrqTrigger::Level {
        flags |= IrqFlags::LEVEL_TRIGGERED;
    }
    if polarity == AcpiIrqPolarity::ActiveLow {
        flags |= IrqFlags::LOW_ACTIVE;
    }
    flags
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

    fn empty_ioapic_intc() -> X86IoApicIntc {
        X86IoApicIntc {
            ioapics: Vec::new(),
            routes: Vec::new(),
            vector_routes: Vec::new(),
            destinations: Vec::new(),
        }
    }

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
    fn ioapic_vector_reverse_route_does_not_assume_base_plus_gsi() {
        let vector = rdrive::probe::acpi::PCI_INTX_VECTOR_BASE + 3;
        let irq = ioapic_gsi_irq_id(18);
        let mut intc = empty_ioapic_intc();
        intc.remember_vector_route(vector, irq).unwrap();

        assert_eq!(intc.irq_for_vector(vector), Some(irq));
        assert_ne!(intc.irq_for_vector(vector), Some(ioapic_gsi_irq_id(3)));
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
        let mut intc = empty_ioapic_intc();
        assert_eq!(
            intc.remember_vector_route(APIC_TIMER_VECTOR, ioapic_gsi_irq_id(1)),
            Err(IrqError::Busy)
        );
        assert_eq!(
            intc.remember_vector_route(APIC_IPI_VECTOR, ioapic_gsi_irq_id(1)),
            Err(IrqError::Busy)
        );
        assert_eq!(
            intc.remember_vector_route(SPURIOUS_VECTOR, ioapic_gsi_irq_id(1)),
            Err(IrqError::Busy)
        );
        assert_eq!(
            intc.remember_vector_route(0x1f, ioapic_gsi_irq_id(1)),
            Err(IrqError::Busy)
        );
        assert_eq!(
            intc.remember_vector_route(usize::from(u8::MAX) + 1, ioapic_gsi_irq_id(1)),
            Err(IrqError::InvalidIrq)
        );

        let vector = 0x72;
        let irq = ioapic_gsi_irq_id(7);
        intc.remember_vector_route(vector, irq).unwrap();
        assert_eq!(
            intc.remember_vector_route(vector, ioapic_gsi_irq_id(8)),
            Err(IrqError::Busy)
        );
        assert_eq!(intc.remember_vector_route(vector, irq), Ok(vector as u8));
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
    fn xapic_destination_rejects_high_apic_ids_without_truncation() {
        assert_eq!(lapic::xapic_destination(0xfe), Ok(0xfe00_0000));
        assert_eq!(lapic::xapic_destination(0x100), Err(IrqError::InvalidCpu));
    }

    #[test]
    fn x2apic_icr_encodes_full_destination_id() {
        let icr = lapic::x2apic_icr(0x1234_5678, lapic::ICR_FIXED_BASE | APIC_IPI_VECTOR as u32);

        assert_eq!(icr >> 32, 0x1234_5678);
        assert_eq!(icr as u32, lapic::ICR_FIXED_BASE | APIC_IPI_VECTOR as u32);
    }

    #[test]
    fn acpi_intx_flags_preserve_trigger_and_polarity() {
        let level_low = intx_flags(AcpiIrqTrigger::Level, AcpiIrqPolarity::ActiveLow);
        assert!(level_low.contains(IrqFlags::LEVEL_TRIGGERED));
        assert!(level_low.contains(IrqFlags::LOW_ACTIVE));

        let edge_high = intx_flags(AcpiIrqTrigger::Edge, AcpiIrqPolarity::ActiveHigh);
        assert!(!edge_high.contains(IrqFlags::LEVEL_TRIGGERED));
        assert!(!edge_high.contains(IrqFlags::LOW_ACTIVE));
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

#[cfg(axtest)]
pub(crate) fn somehal_x86_64_constants_hold_for_test() -> bool {
    // IRQ route constant
    assert!(IRQ_ROUTE_VALID == 1 << 63);
    
    // IOAPIC placeholder vector
    assert!(MASKED_IOAPIC_PLACEHOLDER_VECTOR == 0x21);
    
    true
}
