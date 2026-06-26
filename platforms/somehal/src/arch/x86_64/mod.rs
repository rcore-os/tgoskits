use alloc::vec::Vec;

use rdif_intc::{AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger};
use rdrive::{
    DriverGeneric, module_driver,
    probe::{
        OnProbeError,
        acpi::{AcpiId, AcpiIoApic, ProbeAcpi},
    },
};
use spin::Mutex;
use x2apic::ioapic::{IoApic, IrqFlags, IrqMode};

use crate::{
    common::PlatOp,
    irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqError, IrqId, IrqSource, X86_LAPIC_DOMAIN},
};

pub struct Plat;

const APIC_TIMER_VECTOR: usize = 0x20;
const APIC_IPI_VECTOR: usize = 0xf3;
const LAPIC_REG_EOI: u32 = 0x0b0;
const LAPIC_REG_ICR_LOW: u32 = 0x300;
const LAPIC_REG_ICR_HIGH: u32 = 0x310;
const ICR_DELIVERY_PENDING: u32 = 1 << 12;
const ICR_FIXED_BASE: u32 = 0x0000_4000;
const ICR_DEST_SELF: u32 = 0x0004_0000;
const ICR_DEST_ALL_EXCLUDING_SELF: u32 = 0x000c_0000;

#[derive(Clone, Copy, Debug)]
struct VectorRoute {
    irq: IrqId,
}

static VECTOR_ROUTES: Mutex<[Option<VectorRoute>; 256]> = Mutex::new([const { None }; 256]);

fn lapic_timer_irq_id() -> IrqId {
    IrqId::new(X86_LAPIC_DOMAIN, HwIrq(0))
}

fn lapic_ipi_irq_id() -> IrqId {
    IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(APIC_IPI_VECTOR as u32))
}

fn cpu_local_vector_irq_id(raw: usize) -> IrqId {
    IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(raw as u32))
}

#[cfg(test)]
fn ioapic_gsi_irq_id(gsi: u32) -> IrqId {
    IrqId::new(crate::irq::IrqDomainId(7), HwIrq(gsi))
}

fn trap_vector_irq_id(raw: usize) -> Option<IrqId> {
    if raw == APIC_TIMER_VECTOR {
        return Some(lapic_timer_irq_id());
    }

    if raw == APIC_IPI_VECTOR {
        return Some(lapic_ipi_irq_id());
    }

    Some(
        VECTOR_ROUTES
            .lock()
            .get(raw)
            .and_then(|route| route.map(|route| route.irq))
            .unwrap_or_else(|| cpu_local_vector_irq_id(raw)),
    )
}

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

struct X86IoApicIntc {
    ioapics: Vec<X86IoApic>,
    routes: Vec<AcpiGsiRoute>,
    destinations: Vec<(usize, u8)>,
}

impl X86IoApicIntc {
    fn new(ioapics: &[AcpiIoApic]) -> Self {
        Self {
            ioapics: ioapics.iter().copied().map(X86IoApic::new).collect(),
            routes: Vec::new(),
            destinations: Vec::new(),
        }
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
            ioapic.init(irq_vector_base(info.gsi_base) as u8);
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
        install_vector_route(route.vector, translation.id)?;
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
            return crate::irq::set_controller_irq_enabled(irq, enable);
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
                apic_id as u8
            }
        };
        if set_ioapic_gsi_destination(irq.hwirq.0, dest) {
            Ok(())
        } else {
            Err(IrqError::NotFound)
        }
    }

    fn send_ipi(irq: IrqId, target: crate::irq::IpiTarget) {
        let vector = irq.hwirq.0 as u8;

        unsafe {
            match target {
                crate::irq::IpiTarget::Current { .. } => {
                    send_lapic_ipi(0, ICR_FIXED_BASE | ICR_DEST_SELF | u32::from(vector))
                }
                crate::irq::IpiTarget::Other { cpu_id } => {
                    let Some(apic_id) = someboot::smp::cpu_idx_to_id(cpu_id) else {
                        warn!("failed to resolve CPU index {cpu_id} to APIC ID");
                        return;
                    };
                    send_lapic_ipi(raw_apic_id(apic_id), ICR_FIXED_BASE | u32::from(vector));
                }
                crate::irq::IpiTarget::AllExceptCurrent { .. } => {
                    send_lapic_ipi(
                        0,
                        ICR_FIXED_BASE | ICR_DEST_ALL_EXCLUDING_SELF | u32::from(vector),
                    );
                }
            }
        }
    }

    fn ipi_irq() -> IrqId {
        lapic_ipi_irq_id()
    }

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        trap_vector_irq_id(raw).map(ActiveIrq::new)
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

    fn secondary_init_intc(_cpu_idx: usize) {}

    fn secondary_init_systick() {}

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
        lapic_eoi();
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
    let domain = crate::irq::domain_by_kind(crate::irq::IrqDomainKind::X86IoApic)
        .ok_or(IrqError::Unsupported)?;
    let intc = crate::irq::intc_by_domain(domain.id)?;
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

fn install_vector_route(vector: usize, irq: IrqId) -> Result<u8, IrqError> {
    let vector_u8 = u8::try_from(vector).map_err(|_| IrqError::InvalidIrq)?;
    if matches!(vector, APIC_TIMER_VECTOR | APIC_IPI_VECTOR) {
        return Err(IrqError::Busy);
    }

    let mut routes = VECTOR_ROUTES.lock();
    let Some(slot) = routes.get_mut(vector) else {
        return Err(IrqError::InvalidIrq);
    };
    match *slot {
        None => *slot = Some(VectorRoute { irq }),
        Some(old) if old.irq == irq => {}
        Some(_) => return Err(IrqError::Busy),
    }
    Ok(vector_u8)
}

fn set_ioapic_gsi_destination(gsi: u32, dest: u8) -> bool {
    for intc in rdrive::get_list::<rdif_intc::Intc>() {
        if intc.descriptor().name.starts_with("ACPI IOAPIC")
            && let Ok(ioapic) = intc.downcast::<X86IoApicIntc>()
            && let Ok(mut ioapic) = ioapic.try_lock()
            && ioapic.set_gsi_destination(gsi, dest)
        {
            return true;
        }
    }
    false
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

fn irq_vector_base(gsi_base: u32) -> usize {
    rdrive::probe::acpi::PCI_INTX_VECTOR_BASE + gsi_base as usize
}

fn lapic_eoi() {
    unsafe {
        lapic_write(LAPIC_REG_EOI, 0);
    }
}

fn raw_apic_id(id: usize) -> u32 {
    (id as u32) << 24
}

unsafe fn send_lapic_ipi(destination: u32, icr_low: u32) {
    unsafe {
        lapic_write(LAPIC_REG_ICR_HIGH, destination);
        lapic_write(LAPIC_REG_ICR_LOW, icr_low);
        while lapic_read(LAPIC_REG_ICR_LOW) & ICR_DELIVERY_PENDING != 0 {
            core::hint::spin_loop();
        }
    }
}

unsafe fn lapic_read(offset: u32) -> u32 {
    let ptr = lapic_ptr(offset) as *const u32;
    unsafe { ptr.read_volatile() }
}

unsafe fn lapic_write(offset: u32, value: u32) {
    let ptr = lapic_ptr(offset);
    unsafe {
        ptr.write_volatile(value);
    }
}

fn lapic_ptr(offset: u32) -> *mut u32 {
    const IA32_APIC_BASE: u32 = 0x1b;
    const LAPIC_BASE_MASK: u64 = 0xffff_f000;
    let base = unsafe { x86::msr::rdmsr(IA32_APIC_BASE) & LAPIC_BASE_MASK } as usize;
    unsafe { someboot::mem::phys_to_virt(base).add(offset as usize) }.cast()
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
        assert_eq!(trap_vector_irq_id(APIC_IPI_VECTOR), Some(irq));
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
        install_vector_route(vector, irq).unwrap();

        assert_eq!(trap_vector_irq_id(vector), Some(irq));
        assert_ne!(trap_vector_irq_id(vector), Some(ioapic_gsi_irq_id(3)));

        VECTOR_ROUTES.lock()[vector] = None;
    }

    #[test]
    fn unknown_vector_is_cpu_local_so_it_can_still_eoi() {
        let vector = 0x71;
        VECTOR_ROUTES.lock()[vector] = None;
        let irq = trap_vector_irq_id(vector).expect("unknown vectors still need ActiveIrq");

        assert_eq!(irq.domain, CPU_LOCAL_IRQ_DOMAIN);
        assert_eq!(irq.hwirq, HwIrq(vector as u32));
        assert_ne!(irq, ioapic_gsi_irq_id(vector as u32));
    }

    #[test]
    fn vector_route_rejects_reserved_out_of_range_and_collision() {
        assert_eq!(
            install_vector_route(APIC_TIMER_VECTOR, ioapic_gsi_irq_id(1)),
            Err(IrqError::Busy)
        );
        assert_eq!(
            install_vector_route(usize::from(u8::MAX) + 1, ioapic_gsi_irq_id(1)),
            Err(IrqError::InvalidIrq)
        );

        let vector = 0x72;
        let irq = ioapic_gsi_irq_id(7);
        install_vector_route(vector, irq).unwrap();
        assert_eq!(
            install_vector_route(vector, ioapic_gsi_irq_id(8)),
            Err(IrqError::Busy)
        );
        assert_eq!(install_vector_route(vector, irq), Ok(vector as u8));
        VECTOR_ROUTES.lock()[vector] = None;
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
