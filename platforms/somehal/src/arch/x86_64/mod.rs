use alloc::vec::Vec;

use rdif_intc::{AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger};
use rdrive::{
    DriverGeneric, module_driver,
    probe::{
        OnProbeError,
        acpi::{AcpiId, AcpiIoApic, ProbeAcpi},
    },
};
use x2apic::ioapic::{IoApic, IrqFlags, IrqMode};

use crate::common::PlatOp;

pub struct Plat;

const APIC_TIMER_VECTOR: usize = 0x20;
const LAPIC_REG_EOI: u32 = 0x0b0;
const LAPIC_REG_ICR_LOW: u32 = 0x300;
const LAPIC_REG_ICR_HIGH: u32 = 0x310;
const ICR_DELIVERY_PENDING: u32 = 1 << 12;
const ICR_FIXED_BASE: u32 = 0x0000_4000;
const ICR_DEST_SELF: u32 = 0x0004_0000;
const ICR_DEST_ALL_EXCLUDING_SELF: u32 = 0x000c_0000;

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
}

impl X86IoApicIntc {
    fn new(ioapics: &[AcpiIoApic]) -> Self {
        Self {
            ioapics: ioapics.iter().copied().map(X86IoApic::new).collect(),
            routes: Vec::new(),
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

    fn routes_for_vector(&self, vector: usize) -> Vec<AcpiGsiRoute> {
        let routes: Vec<_> = self
            .routes
            .iter()
            .copied()
            .filter(|r| r.vector == vector)
            .collect();
        if !routes.is_empty() {
            return routes;
        }

        rdrive::probe::acpi::with_acpi(|system| system.routing().resolve_vector(vector))
            .flatten()
            .into_iter()
            .collect()
    }

    fn set_vector_enable(&mut self, vector: usize, enable: bool) -> bool {
        let routes = self.routes_for_vector(vector);
        if routes.is_empty() {
            return false;
        }

        for route in routes {
            self.set_route_enable(&route, enable);
        }
        true
    }

    fn set_route_enable(&mut self, route: &AcpiGsiRoute, enable: bool) {
        for ioapic in &mut self.ioapics {
            if ioapic.contains_route(route) {
                ioapic.set_route_enable(route, enable);
                return;
            }
        }
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

    fn set_route_enable(&mut self, route: &AcpiGsiRoute, enable: bool) {
        if !self.contains_route(route) {
            return;
        }

        unsafe {
            let input = route.controller_input;
            let mut entry = self.ioapic.table_entry(input);
            entry.set_vector(route.vector as u8);
            entry.set_mode(IrqMode::Fixed);
            entry.set_flags(intx_flags(route.trigger, route.polarity) | IrqFlags::MASKED);
            entry.set_dest(0);
            self.ioapic.set_table_entry(input, entry);

            if enable {
                self.ioapic.enable_irq(input);
            }
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

    fn setup_irq_by_acpi(&mut self, route: &AcpiGsiRoute) -> rdrive::IrqId {
        self.remember_route(*route);
        self.set_route_enable(route, false);
        route.vector.into()
    }
}

fn probe_ioapic(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let ioapics = info.root.routing().io_apics();
    if ioapics.is_empty() {
        return Err(OnProbeError::NotMatch);
    }

    dev.register(rdif_intc::Intc::new(X86IoApicIntc::new(ioapics)));
    Ok(())
}

impl PlatOp for Plat {
    type ActiveIrq = ActiveIrq;

    fn irq_set_enable(irq: rdrive::IrqId, enable: bool) {
        let raw = irq.raw();

        if raw == someboot::irq::systimer_irq().raw() {
            someboot::irq::irq_set_enable(someboot::irq::IrqId::new(raw), enable);
            return;
        }

        set_ioapic_vector_enable(raw, enable);
    }

    fn send_ipi(irq: rdrive::IrqId, target: crate::irq::IpiTarget) {
        let vector = irq.raw() as u8;

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

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        if raw == APIC_TIMER_VECTOR {
            return Some(ActiveIrq::new(someboot::irq::systimer_irq().raw().into()));
        }

        Some(ActiveIrq::new(raw.into()))
    }

    fn active_irq_id(active: &Self::ActiveIrq) -> rdrive::IrqId {
        active.id()
    }

    fn systick_irq() -> rdrive::IrqId {
        someboot::irq::systimer_irq().raw().into()
    }

    fn secondary_init() {}

    fn secondary_init_intc(_cpu_idx: usize) {}

    fn secondary_init_systick() {}

    fn send_ipi_to_cpu(cpu_id: usize) {
        Self::send_ipi(
            APIC_TIMER_VECTOR.into(),
            crate::irq::IpiTarget::Other { cpu_id },
        );
    }
}

pub struct ActiveIrq {
    irq: rdrive::IrqId,
}

impl ActiveIrq {
    const fn new(irq: rdrive::IrqId) -> Self {
        Self { irq }
    }

    pub fn id(&self) -> rdrive::IrqId {
        self.irq
    }
}

impl Drop for ActiveIrq {
    fn drop(&mut self) {
        lapic_eoi();
    }
}

fn set_ioapic_vector_enable(vector: usize, enable: bool) {
    for intc in rdrive::get_list::<rdif_intc::Intc>() {
        if intc.descriptor().name.starts_with("ACPI IOAPIC")
            && let Ok(ioapic) = intc.downcast::<X86IoApicIntc>()
            && let Ok(mut ioapic) = ioapic.try_lock()
            && ioapic.set_vector_enable(vector, enable)
        {
            return;
        }
    }
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
    fn acpi_intx_flags_preserve_trigger_and_polarity() {
        let level_low = intx_flags(AcpiIrqTrigger::Level, AcpiIrqPolarity::ActiveLow);
        assert!(level_low.contains(IrqFlags::LEVEL_TRIGGERED));
        assert!(level_low.contains(IrqFlags::LOW_ACTIVE));

        let edge_high = intx_flags(AcpiIrqTrigger::Edge, AcpiIrqPolarity::ActiveHigh);
        assert!(!edge_high.contains(IrqFlags::LEVEL_TRIGGERED));
        assert!(!edge_high.contains(IrqFlags::LOW_ACTIVE));
    }
}
