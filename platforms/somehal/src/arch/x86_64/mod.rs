use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use ax_kspin::SpinIrqSave;
use irq_framework::{CpuId, IrqScope};
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
    irq::{
        CPU_LOCAL_IRQ_DOMAIN, CpuIpiTarget, HwIrq, IpiSendStatus, IrqDomainId, IrqError, IrqId,
        IrqSource, X86_LAPIC_DOMAIN,
    },
    irq_line::{BoundIrqStatus, IrqChipLine, PreparedIrqChipLine},
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
const PREFERRED_IOAPIC_VECTOR_BASE: usize = 0x30;
const FIRST_EXTERNAL_VECTOR: usize = 0x20;
const LAST_EXTERNAL_VECTOR: usize = 0xfe;
// The LAPIC timer and scheduler IPI occupy two vectors in this range.
const EXTERNAL_VECTOR_CAPACITY: usize = LAST_EXTERNAL_VECTOR - FIRST_EXTERNAL_VECTOR + 1 - 2;
const IRQ_ROUTE_VALID: u64 = 1 << 63;
const UNPUBLISHED_IOAPIC_GSI_ROUTE: u64 = 0;
const UNPUBLISHED_IOAPIC_GSI_CONFIG: u8 = 0;
const IOAPIC_GSI_CONFIG_VALID: u8 = 1 << 7;
const IOAPIC_GSI_CONFIG_LEVEL: u8 = 1 << 0;
const IOAPIC_GSI_CONFIG_ACTIVE_LOW: u8 = 1 << 1;

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
    vector_reservations: [AtomicU64; 256],
    vector_routes: [AtomicU64; 256],
    endpoint_slots: [IoApicEndpointSlot; EXTERNAL_VECTOR_CAPACITY],
    mmio_lock: SpinIrqSave<()>,
}

#[derive(Clone, Copy)]
struct IoApicLineEndpoint {
    physical_base: u64,
    input: u8,
}

impl X86IoApicCpuInterface {
    const fn new() -> Self {
        Self {
            vector_reservations: [const { AtomicU64::new(0) }; 256],
            vector_routes: [const { AtomicU64::new(0) }; 256],
            endpoint_slots: [const { IoApicEndpointSlot::new() }; EXTERNAL_VECTOR_CAPACITY],
            mmio_lock: SpinIrqSave::new(()),
        }
    }

    fn irq_for_vector(&self, vector: usize) -> Option<IrqId> {
        let vector = u8::try_from(vector).ok()?;
        decode_irq_id(self.vector_routes[usize::from(vector)].load(Ordering::Acquire))
    }

    fn endpoint_for_gsi(&self, gsi: u32) -> Option<(u64, u8)> {
        let key = encode_gsi_key(gsi);
        let start = endpoint_slot_start(gsi);
        for offset in 0..EXTERNAL_VECTOR_CAPACITY {
            let slot = &self.endpoint_slots[(start + offset) % EXTERNAL_VECTOR_CAPACITY];
            if slot.gsi_key.load(Ordering::Acquire) == key {
                return decode_ioapic_gsi_route(slot.route.load(Ordering::Acquire));
            }
        }
        None
    }

    fn set_gsi_enabled(&self, gsi: u32, enabled: bool) -> Result<(), IrqError> {
        let endpoint = self.line_endpoint(gsi)?;
        self.set_endpoint_enabled(endpoint, enabled);
        Ok(())
    }

    fn line_endpoint(&self, gsi: u32) -> Result<IoApicLineEndpoint, IrqError> {
        let (physical_base, input) = self.endpoint_for_gsi(gsi).ok_or(IrqError::NotFound)?;
        Ok(IoApicLineEndpoint {
            physical_base,
            input,
        })
    }

    fn set_endpoint_enabled(&self, endpoint: IoApicLineEndpoint, enabled: bool) {
        let _guard = self.mmio_lock.lock();
        let virtual_base = someboot::mem::phys_to_virt(endpoint.physical_base as usize) as u64;
        let mut ioapic = unsafe { IoApic::new(virtual_base) };
        unsafe {
            if enabled {
                ioapic.enable_irq(endpoint.input);
            } else {
                ioapic.disable_irq(endpoint.input);
            }
        }
    }

    fn set_endpoint_destination(&self, endpoint: IoApicLineEndpoint, dest: u8) {
        let _guard = self.mmio_lock.lock();
        let virtual_base = someboot::mem::phys_to_virt(endpoint.physical_base as usize) as u64;
        let mut ioapic = unsafe { IoApic::new(virtual_base) };
        unsafe {
            let mut entry = ioapic.table_entry(endpoint.input);
            entry.set_dest(dest);
            ioapic.set_table_entry(endpoint.input, entry);
        }
    }

    fn endpoint_enabled(&self, endpoint: IoApicLineEndpoint) -> bool {
        let _guard = self.mmio_lock.lock();
        let virtual_base = someboot::mem::phys_to_virt(endpoint.physical_base as usize) as u64;
        let mut ioapic = unsafe { IoApic::new(virtual_base) };
        let entry = unsafe { ioapic.table_entry(endpoint.input) };
        !entry.flags().contains(IrqFlags::MASKED)
    }

    fn reserve_gsi_endpoint(
        &self,
        route: &AcpiGsiRoute,
    ) -> Result<IoApicEndpointReservation<'_>, IrqError> {
        let key = encode_gsi_key(route.gsi);
        let encoded_route =
            encode_ioapic_gsi_route(route.controller_address, route.controller_input)?;
        let encoded_config = encode_ioapic_gsi_config(route.trigger, route.polarity);
        let start = endpoint_slot_start(route.gsi);
        let mut first_empty = None;

        for offset in 0..EXTERNAL_VECTOR_CAPACITY {
            let slot_index = (start + offset) % EXTERNAL_VECTOR_CAPACITY;
            let slot = &self.endpoint_slots[slot_index];
            let existing_key = slot.gsi_key.load(Ordering::Acquire);
            if existing_key == key {
                let existing_route = slot.route.load(Ordering::Acquire);
                let existing_config = slot.config.load(Ordering::Acquire);
                return if existing_route == encoded_route && existing_config == encoded_config {
                    Ok(IoApicEndpointReservation::existing(
                        slot,
                        key,
                        encoded_route,
                        encoded_config,
                    ))
                } else {
                    Err(IrqError::Busy)
                };
            }
            if existing_key == 0 && first_empty.is_none() {
                first_empty = Some(slot);
            }
        }

        let slot = first_empty.ok_or(IrqError::NoMemory)?;
        slot.gsi_key
            .compare_exchange(0, key, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| IrqError::Busy)?;
        Ok(IoApicEndpointReservation::new(
            slot,
            key,
            encoded_route,
            encoded_config,
        ))
    }

    fn reserve_external_vector(
        &self,
        irq: IrqId,
        gsi: u32,
    ) -> Result<ExternalVectorReservation<'_>, IrqError> {
        let encoded = encode_irq_id(irq);
        for vector in FIRST_EXTERNAL_VECTOR..=LAST_EXTERNAL_VECTOR {
            if self.vector_reservations[vector].load(Ordering::Acquire) == encoded {
                return if self.vector_routes[vector].load(Ordering::Acquire) == encoded {
                    Ok(ExternalVectorReservation::existing(
                        self,
                        vector as u8,
                        encoded,
                    ))
                } else {
                    Err(IrqError::Busy)
                };
            }
        }

        if let Some(preferred) = preferred_external_vector(gsi)
            && let Some(reservation) = self.try_reserve_external_vector(preferred, encoded)
        {
            return Ok(reservation);
        }

        for vector in (PREFERRED_IOAPIC_VECTOR_BASE..=LAST_EXTERNAL_VECTOR)
            .chain((FIRST_EXTERNAL_VECTOR + 1)..PREFERRED_IOAPIC_VECTOR_BASE)
        {
            if let Some(reservation) = self.try_reserve_external_vector(vector, encoded) {
                return Ok(reservation);
            }
        }
        Err(IrqError::NoMemory)
    }

    fn try_reserve_external_vector(
        &self,
        vector: usize,
        encoded: u64,
    ) -> Option<ExternalVectorReservation<'_>> {
        let vector = validate_external_vector(vector).ok()?;
        let index = usize::from(vector);
        if self.vector_routes[index].load(Ordering::Acquire) != 0 {
            return None;
        }
        self.vector_reservations[index]
            .compare_exchange(0, encoded, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(ExternalVectorReservation::new(self, vector, encoded))
    }
}

struct IoApicEndpointSlot {
    gsi_key: AtomicU64,
    route: AtomicU64,
    config: AtomicU8,
}

impl IoApicEndpointSlot {
    const fn new() -> Self {
        Self {
            gsi_key: AtomicU64::new(0),
            route: AtomicU64::new(UNPUBLISHED_IOAPIC_GSI_ROUTE),
            config: AtomicU8::new(UNPUBLISHED_IOAPIC_GSI_CONFIG),
        }
    }
}

struct IoApicEndpointReservation<'a> {
    slot: &'a IoApicEndpointSlot,
    key: u64,
    route: u64,
    config: u8,
    newly_reserved: bool,
    published: bool,
}

impl<'a> IoApicEndpointReservation<'a> {
    const fn new(slot: &'a IoApicEndpointSlot, key: u64, route: u64, config: u8) -> Self {
        Self {
            slot,
            key,
            route,
            config,
            newly_reserved: true,
            published: false,
        }
    }

    const fn existing(slot: &'a IoApicEndpointSlot, key: u64, route: u64, config: u8) -> Self {
        Self {
            slot,
            key,
            route,
            config,
            newly_reserved: false,
            published: true,
        }
    }

    const fn is_new(&self) -> bool {
        self.newly_reserved
    }

    fn publish(mut self) {
        if self.newly_reserved {
            self.slot.config.store(self.config, Ordering::Relaxed);
            self.slot.route.store(self.route, Ordering::Release);
            self.published = true;
        }
    }
}

impl Drop for IoApicEndpointReservation<'_> {
    fn drop(&mut self) {
        if !self.newly_reserved || self.published {
            return;
        }
        let published_route = self.slot.route.load(Ordering::Acquire);
        if published_route != UNPUBLISHED_IOAPIC_GSI_ROUTE {
            debug_assert_eq!(
                published_route, UNPUBLISHED_IOAPIC_GSI_ROUTE,
                "an unpublished endpoint token cannot own a published slot"
            );
            return;
        }
        self.slot
            .config
            .store(UNPUBLISHED_IOAPIC_GSI_CONFIG, Ordering::Relaxed);
        let _ =
            self.slot
                .gsi_key
                .compare_exchange(self.key, 0, Ordering::Release, Ordering::Relaxed);
    }
}

struct ExternalVectorReservation<'a> {
    cpu_if: &'a X86IoApicCpuInterface,
    vector: u8,
    irq: u64,
    newly_reserved: bool,
    published: bool,
}

impl<'a> ExternalVectorReservation<'a> {
    const fn new(cpu_if: &'a X86IoApicCpuInterface, vector: u8, irq: u64) -> Self {
        Self {
            cpu_if,
            vector,
            irq,
            newly_reserved: true,
            published: false,
        }
    }

    const fn existing(cpu_if: &'a X86IoApicCpuInterface, vector: u8, irq: u64) -> Self {
        Self {
            cpu_if,
            vector,
            irq,
            newly_reserved: false,
            published: true,
        }
    }

    const fn vector(&self) -> u8 {
        self.vector
    }

    const fn is_new(&self) -> bool {
        self.newly_reserved
    }

    fn publish(mut self) {
        if self.newly_reserved {
            self.cpu_if.vector_routes[usize::from(self.vector)].store(self.irq, Ordering::Release);
            self.published = true;
        }
    }
}

impl Drop for ExternalVectorReservation<'_> {
    fn drop(&mut self) {
        if !self.newly_reserved || self.published {
            return;
        }
        let published_route =
            self.cpu_if.vector_routes[usize::from(self.vector)].load(Ordering::Acquire);
        if published_route != 0 {
            debug_assert_eq!(
                published_route, 0,
                "an unpublished vector token cannot own a published route"
            );
            return;
        }
        let _ = self.cpu_if.vector_reservations[usize::from(self.vector)].compare_exchange(
            self.irq,
            0,
            Ordering::Release,
            Ordering::Relaxed,
        );
    }
}

const fn encode_gsi_key(gsi: u32) -> u64 {
    gsi as u64 + 1
}

fn endpoint_slot_start(gsi: u32) -> usize {
    (gsi as usize).wrapping_mul(0x9e37_79b1) % EXTERNAL_VECTOR_CAPACITY
}

fn preferred_external_vector(gsi: u32) -> Option<usize> {
    let vector = PREFERRED_IOAPIC_VECTOR_BASE.checked_add(gsi as usize)?;
    validate_external_vector(vector).ok().map(usize::from)
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

fn encode_ioapic_gsi_route(physical_base: u64, input: u8) -> Result<u64, IrqError> {
    if physical_base & 0xfff != 0 {
        return Err(IrqError::InvalidIrq);
    }
    let page = physical_base >> 12;
    if page == 0 || page >= (1u64 << 56) {
        return Err(IrqError::InvalidIrq);
    }
    Ok((page << 8) | u64::from(input))
}

fn decode_ioapic_gsi_route(encoded: u64) -> Option<(u64, u8)> {
    (encoded != UNPUBLISHED_IOAPIC_GSI_ROUTE).then_some(((encoded >> 8) << 12, encoded as u8))
}

const fn encode_ioapic_gsi_config(trigger: AcpiIrqTrigger, polarity: AcpiIrqPolarity) -> u8 {
    let mut config = IOAPIC_GSI_CONFIG_VALID;
    if matches!(trigger, AcpiIrqTrigger::Level) {
        config |= IOAPIC_GSI_CONFIG_LEVEL;
    }
    if matches!(polarity, AcpiIrqPolarity::ActiveLow) {
        config |= IOAPIC_GSI_CONFIG_ACTIVE_LOW;
    }
    config
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IoApicRouteReservation {
    ProgramMasked,
    ReuseLive,
}

fn classify_ioapic_route_reservation(
    endpoint_is_new: bool,
    vector_is_new: bool,
) -> Result<IoApicRouteReservation, IrqError> {
    match (endpoint_is_new, vector_is_new) {
        (true, true) => Ok(IoApicRouteReservation::ProgramMasked),
        (false, false) => Ok(IoApicRouteReservation::ReuseLive),
        // One half of a route without the other cannot be repaired by
        // rewriting live IOAPIC state. Retain the published half and make the
        // inconsistent discovery fail closed.
        _ => Err(IrqError::Busy),
    }
}

struct X86IoApicIntc {
    ioapics: Vec<X86IoApic>,
}

#[derive(Clone, Copy)]
struct ProgrammedIoApicRoute {
    acpi: AcpiGsiRoute,
    vector: u8,
}

impl X86IoApicIntc {
    fn new(ioapics: &[AcpiIoApic]) -> Self {
        Self {
            ioapics: ioapics.iter().copied().map(X86IoApic::new).collect(),
        }
    }

    fn set_route_enable(&mut self, route: &ProgrammedIoApicRoute, enable: bool) -> bool {
        for ioapic in &mut self.ioapics {
            if ioapic.contains_route(&route.acpi) {
                return ioapic.set_route_enable(route, enable, 0).is_ok();
            }
        }
        false
    }
}

struct X86IoApic {
    info: AcpiIoApic,
    ioapic: IoApic,
}

impl X86IoApic {
    fn new(info: AcpiIoApic) -> Self {
        let ioapic_base = someboot::mem::phys_to_virt(info.address as usize) as u64;
        let (ioapic, redirection_entries) = {
            let _guard = IOAPIC_CPU_IF.mmio_lock.lock();
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
            (ioapic, redirection_entries)
        };

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
        gsi.checked_sub(self.info.gsi_base)
            .is_some_and(|input| input < u32::from(self.info.redirection_entries))
    }

    fn contains_route(&self, route: &AcpiGsiRoute) -> bool {
        u16::from(self.info.id) == route.controller_id
            && u64::from(self.info.address) == route.controller_address
            && self.contains(route.gsi)
            && route.gsi.checked_sub(self.info.gsi_base).and_then(|input| {
                u8::try_from(input)
                    .ok()
                    .map(|input| input == route.controller_input)
            }) == Some(true)
    }

    fn set_route_enable(
        &mut self,
        route: &ProgrammedIoApicRoute,
        enable: bool,
        dest: u8,
    ) -> Result<(), IrqError> {
        if !self.contains_route(&route.acpi) {
            return Err(IrqError::InvalidIrq);
        }

        let _guard = IOAPIC_CPU_IF.mmio_lock.lock();
        unsafe {
            let input = route.acpi.controller_input;
            let mut entry = self.ioapic.table_entry(input);
            entry.set_vector(route.vector);
            entry.set_mode(IrqMode::Fixed);
            entry.set_flags(intx_flags(route.acpi.trigger, route.acpi.polarity) | IrqFlags::MASKED);
            entry.set_dest(dest);
            self.ioapic.set_table_entry(input, entry);

            if enable {
                self.ioapic.enable_irq(input);
            }
        }
        Ok(())
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
        if !self.supports_acpi_gsi(route) {
            return Err(IrqError::Unsupported);
        }

        // Reserve every software resource before touching IOREGSEL/IOWIN. A
        // GSI route is immutable after its first masked publication: another
        // PCI function sharing the same ACPI GSI may validate and reuse it,
        // but must never rewrite the live IOAPIC mask bit behind the IRQ
        // framework's desired/applied state.
        let endpoint = IOAPIC_CPU_IF.reserve_gsi_endpoint(route)?;
        let vector = IOAPIC_CPU_IF.reserve_external_vector(translation.id, route.gsi)?;
        let reservation = classify_ioapic_route_reservation(endpoint.is_new(), vector.is_new())?;
        let programmed = ProgrammedIoApicRoute {
            acpi: *route,
            vector: vector.vector(),
        };
        if reservation == IoApicRouteReservation::ProgramMasked
            && !self.set_route_enable(&programmed, false)
        {
            return Err(IrqError::Unsupported);
        }
        vector.publish();
        endpoint.publish();
        Ok(())
    }

    fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
        IOAPIC_CPU_IF
            .set_gsi_enabled(hwirq.0, enabled)
            .map_err(|_| IrqError::InvalidIrq)
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

    fn prepare_irq_line(
        irq: IrqId,
        scope: IrqScope,
        affinity: crate::irq::IrqAffinity,
    ) -> Result<PreparedIrqChipLine, IrqError> {
        let kind = if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            // A fixed-delivery LAPIC IPI vector has no per-vector receive-side
            // mask bit. It remains physically live while the framework gates
            // delivery through the action state.
            lapic::ipi_vector(irq)?;
            if !matches!(scope, IrqScope::PerCpu { .. }) {
                return Err(IrqError::InvalidIrq);
            }
            return Ok(PreparedIrqChipLine::action_gate_only());
        } else if irq.domain == X86_LAPIC_DOMAIN {
            if irq.hwirq.0 != 0 || !matches!(scope, IrqScope::PerCpu { .. }) {
                return Err(IrqError::InvalidIrq);
            }
            X86LineKind::LapicTimer
        } else if crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::X86IoApic) {
            if scope != IrqScope::Global {
                return Err(IrqError::InvalidIrq);
            }
            let endpoint = IOAPIC_CPU_IF.line_endpoint(irq.hwirq.0)?;
            let destination = ioapic_destination(affinity)?;
            IOAPIC_CPU_IF.set_endpoint_enabled(endpoint, false);
            IOAPIC_CPU_IF.set_endpoint_destination(endpoint, destination);
            X86LineKind::IoApic { endpoint }
        } else {
            return Err(IrqError::InvalidIrq);
        };
        Ok(PreparedIrqChipLine::maskable(Box::new(X86IrqChipLine {
            kind,
        })))
    }

    fn send_ipi(
        irq: IrqId,
        target: CpuIpiTarget,
        current_cpu: irq_framework::CpuId,
    ) -> IpiSendStatus {
        let Ok(vector) = lapic::ipi_vector(irq) else {
            return IpiSendStatus::Invalid;
        };

        let result = match target {
            CpuIpiTarget::Current { cpu } => {
                if current_cpu != cpu || crate::cpu::runtime_cpu_target(cpu).is_none() {
                    return IpiSendStatus::Invalid;
                }
                lapic::send_ipi(
                    0,
                    lapic::ICR_FIXED_BASE | lapic::ICR_DEST_SELF | u32::from(vector),
                )
            }
            CpuIpiTarget::Other { cpu } => {
                let Some(apic_id) = crate::cpu::runtime_cpu_target(cpu) else {
                    return IpiSendStatus::Invalid;
                };
                let Ok(apic_id) = u32::try_from(apic_id.as_usize()) else {
                    return IpiSendStatus::Invalid;
                };
                lapic::send_ipi_to_apic_id(apic_id, lapic::ICR_FIXED_BASE | u32::from(vector))
            }
            CpuIpiTarget::AllExceptCurrent { current, cpu_count } => {
                if cpu_count != someboot::smp::runtime_cpu_count()
                    || current_cpu != current
                    || crate::cpu::runtime_cpu_target(current).is_none()
                {
                    return IpiSendStatus::Invalid;
                }
                lapic::send_ipi(
                    0,
                    lapic::ICR_FIXED_BASE | lapic::ICR_DEST_ALL_EXCLUDING_SELF | u32::from(vector),
                )
            }
        };

        match result {
            Ok(()) => IpiSendStatus::Success,
            Err(IrqError::Timeout | IrqError::Busy | IrqError::Controller) => IpiSendStatus::Retry,
            Err(_) => IpiSendStatus::Invalid,
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

    fn init_boot_irq_cpu(_cpu_idx: usize, _role: crate::irq::CpuBootRole) -> Result<(), IrqError> {
        Ok(())
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

fn ioapic_destination(affinity: crate::irq::IrqAffinity) -> Result<u8, IrqError> {
    let dest = match affinity {
        crate::irq::IrqAffinity::Any => 0,
        crate::irq::IrqAffinity::Fixed { cpu_id } => {
            let Some(apic_id) = someboot::smp::cpu_idx_to_id(cpu_id) else {
                return Err(IrqError::InvalidCpu);
            };
            u8::try_from(apic_id).map_err(|_| IrqError::InvalidCpu)?
        }
    };
    Ok(dest)
}

struct X86IrqChipLine {
    kind: X86LineKind,
}

#[derive(Clone, Copy)]
enum X86LineKind {
    LapicTimer,
    IoApic { endpoint: IoApicLineEndpoint },
}

// SAFETY: the endpoint retains only shutdown-lifetime LAPIC/IOAPIC
// capabilities. Every live operation uses an IRQ-safe bounded MMIO critical
// section and cannot allocate, block, or re-enter the driver registry.
unsafe impl IrqChipLine for X86IrqChipLine {
    fn set_enabled(&self, cpu: Option<CpuId>, enabled: bool) {
        match self.kind {
            X86LineKind::LapicTimer => {
                let cpu = cpu.expect("LAPIC timer line requires a target CPU");
                assert_eq!(
                    crate::cpu::runtime_current_cpu(),
                    Some(cpu),
                    "prepared LAPIC timer line executed on the wrong CPU"
                );
                someboot::irq::irq_set_enable(someboot::irq::systimer_irq(), enabled);
            }
            X86LineKind::IoApic { endpoint } => {
                assert!(cpu.is_none(), "IOAPIC line cannot use a per-CPU target");
                IOAPIC_CPU_IF.set_endpoint_enabled(endpoint, enabled);
            }
        }
    }

    fn status(&self, _cpu: Option<CpuId>) -> BoundIrqStatus {
        let enabled = match self.kind {
            X86LineKind::IoApic { endpoint } => Some(IOAPIC_CPU_IF.endpoint_enabled(endpoint)),
            X86LineKind::LapicTimer => None,
        };
        BoundIrqStatus {
            enabled,
            ..BoundIrqStatus::default()
        }
    }
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
            ioapic_gsi_irq_id((APIC_IPI_VECTOR - PREFERRED_IOAPIC_VECTOR_BASE) as u32)
        );
    }

    #[test]
    fn ioapic_gsi_irq_ids_preserve_host_gsi_as_hwirq() {
        assert_eq!(ioapic_gsi_irq_id(4).hwirq, HwIrq(4));
        assert_eq!(ioapic_gsi_irq_id(18).hwirq, HwIrq(18));
    }

    #[test]
    fn ioapic_vector_reverse_route_does_not_assume_base_plus_gsi() {
        let cpu_if = X86IoApicCpuInterface::new();
        cpu_if
            .reserve_external_vector(ioapic_gsi_irq_id(99), 18)
            .unwrap()
            .publish();
        let irq = ioapic_gsi_irq_id(18);
        let reservation = cpu_if.reserve_external_vector(irq, 18).unwrap();
        let vector = reservation.vector();
        reservation.publish();

        assert_ne!(usize::from(vector), PREFERRED_IOAPIC_VECTOR_BASE + 18);
        assert_eq!(cpu_if.irq_for_vector(usize::from(vector)), Some(irq));
    }

    #[test]
    fn ioapic_cpu_interface_resolves_vector_without_controller_device() {
        let irq = ioapic_gsi_irq_id(21);
        let cpu_if = X86IoApicCpuInterface::new();
        let reservation = cpu_if.reserve_external_vector(irq, 21).unwrap();
        let vector = reservation.vector();

        reservation.publish();

        assert_eq!(cpu_if.irq_for_vector(usize::from(vector)), Some(irq));
        assert_eq!(cpu_if.irq_for_vector(usize::from(vector) + 1), None);
    }

    #[test]
    fn ioapic_cpu_interface_allocates_around_vector_conflicts() {
        let irq = ioapic_gsi_irq_id(22);
        let conflicting = ioapic_gsi_irq_id(23);
        let cpu_if = X86IoApicCpuInterface::new();
        let first = cpu_if.reserve_external_vector(irq, 22).unwrap();
        let first_vector = first.vector();
        first.publish();
        let repeated = cpu_if.reserve_external_vector(irq, 22).unwrap();
        let conflict = cpu_if.reserve_external_vector(conflicting, 22).unwrap();

        assert_eq!(repeated.vector(), first_vector);
        assert_ne!(conflict.vector(), first_vector);
        assert_eq!(cpu_if.irq_for_vector(usize::from(first_vector)), Some(irq));
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
    fn external_vector_validation_rejects_reserved_and_out_of_range_vectors() {
        assert_eq!(
            validate_external_vector(APIC_TIMER_VECTOR),
            Err(IrqError::Busy)
        );
        assert_eq!(
            validate_external_vector(APIC_IPI_VECTOR),
            Err(IrqError::Busy)
        );
        assert_eq!(
            validate_external_vector(SPURIOUS_VECTOR),
            Err(IrqError::Busy)
        );
        assert_eq!(validate_external_vector(0x1f), Err(IrqError::Busy));
        assert_eq!(
            validate_external_vector(usize::from(u8::MAX) + 1),
            Err(IrqError::InvalidIrq)
        );
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
            controller: rdif_intc::AcpiGsiController::IoApic,
            controller_id: 0,
            controller_address: 0xfec0_0000,
            controller_input: 10,
            trigger: AcpiIrqTrigger::Level,
            polarity: AcpiIrqPolarity::ActiveLow,
        };

        assert_eq!(route_to_rdif(route_to_irq_framework(route)), route);
    }

    #[test]
    fn repeated_identical_ioapic_route_reuses_live_hardware_state() {
        let cpu_if = X86IoApicCpuInterface::new();
        let route = ioapic_route(10, 0, 0xfec0_0000);
        let irq = ioapic_gsi_irq_id(route.gsi);

        let endpoint = cpu_if.reserve_gsi_endpoint(&route).unwrap();
        let vector = cpu_if.reserve_external_vector(irq, route.gsi).unwrap();
        assert_eq!(
            classify_ioapic_route_reservation(endpoint.is_new(), vector.is_new()),
            Ok(IoApicRouteReservation::ProgramMasked)
        );
        vector.publish();
        endpoint.publish();

        let endpoint = cpu_if.reserve_gsi_endpoint(&route).unwrap();
        let vector = cpu_if.reserve_external_vector(irq, route.gsi).unwrap();
        assert_eq!(
            classify_ioapic_route_reservation(endpoint.is_new(), vector.is_new()),
            Ok(IoApicRouteReservation::ReuseLive)
        );
    }

    #[test]
    fn repeated_ioapic_route_rejects_changed_electrical_configuration() {
        let cpu_if = X86IoApicCpuInterface::new();
        let route = ioapic_route(10, 0, 0xfec0_0000);
        cpu_if.reserve_gsi_endpoint(&route).unwrap().publish();

        let mut changed = route;
        changed.polarity = AcpiIrqPolarity::ActiveHigh;
        assert!(matches!(
            cpu_if.reserve_gsi_endpoint(&changed),
            Err(IrqError::Busy)
        ));

        changed = route;
        changed.trigger = AcpiIrqTrigger::Edge;
        assert!(matches!(
            cpu_if.reserve_gsi_endpoint(&changed),
            Err(IrqError::Busy)
        ));
    }

    #[test]
    fn half_published_ioapic_route_fails_closed() {
        assert_eq!(
            classify_ioapic_route_reservation(true, false),
            Err(IrqError::Busy)
        );
        assert_eq!(
            classify_ioapic_route_reservation(false, true),
            Err(IrqError::Busy)
        );
    }

    fn ioapic_route(gsi: u32, controller_id: u16, controller_address: u64) -> AcpiGsiRoute {
        AcpiGsiRoute {
            gsi,
            controller: rdif_intc::AcpiGsiController::IoApic,
            controller_id,
            controller_address,
            controller_input: 0,
            trigger: AcpiIrqTrigger::Level,
            polarity: AcpiIrqPolarity::ActiveLow,
        }
    }

    #[test]
    fn ioapic_endpoint_table_keys_the_full_gsi() {
        let cpu_if = X86IoApicCpuInterface::new();
        let route_256 = ioapic_route(256, 1, 0xfec0_1000);
        let route_sparse = ioapic_route(0x1_0000, 2, 0xfec0_2000);
        let route_max = ioapic_route(u32::MAX, 3, 0xfec0_3000);

        cpu_if.reserve_gsi_endpoint(&route_256).unwrap().publish();
        cpu_if
            .reserve_gsi_endpoint(&route_sparse)
            .unwrap()
            .publish();
        cpu_if.reserve_gsi_endpoint(&route_max).unwrap().publish();

        assert_eq!(
            cpu_if.endpoint_for_gsi(256),
            Some((route_256.controller_address, route_256.controller_input))
        );
        assert_eq!(
            cpu_if.endpoint_for_gsi(0x1_0000),
            Some((
                route_sparse.controller_address,
                route_sparse.controller_input
            ))
        );
        assert_eq!(
            cpu_if.endpoint_for_gsi(u32::MAX),
            Some((route_max.controller_address, route_max.controller_input))
        );
        assert_eq!(cpu_if.endpoint_for_gsi(0), None);
    }

    #[test]
    fn high_gsi_allocates_an_independent_external_vector() {
        let cpu_if = X86IoApicCpuInterface::new();
        let irq = ioapic_gsi_irq_id(256);

        let vector = cpu_if.reserve_external_vector(irq, 256).unwrap();

        assert!(validate_external_vector(usize::from(vector.vector())).is_ok());
        assert_ne!(usize::from(vector.vector()), 0x30 + 256);
    }

    #[test]
    fn dropping_unpublished_route_reservations_rolls_back_every_slot() {
        let cpu_if = X86IoApicCpuInterface::new();
        let route = ioapic_route(256, 1, 0xfec0_1000);
        let irq = ioapic_gsi_irq_id(route.gsi);
        let reserved_vector;

        {
            let endpoint = cpu_if.reserve_gsi_endpoint(&route).unwrap();
            let vector = cpu_if.reserve_external_vector(irq, route.gsi).unwrap();
            reserved_vector = vector.vector();
            assert_eq!(cpu_if.endpoint_for_gsi(route.gsi), None);
            assert_eq!(cpu_if.irq_for_vector(usize::from(reserved_vector)), None);
            drop((endpoint, vector));
        }

        assert_eq!(cpu_if.endpoint_for_gsi(route.gsi), None);
        let retried = cpu_if
            .reserve_external_vector(irq, route.gsi)
            .expect("rollback must release the vector reservation");
        assert_eq!(retried.vector(), reserved_vector);
    }
}
