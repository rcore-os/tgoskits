use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use ax_kspin::SpinIrqSave;
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
        let (physical_base, input) = self.endpoint_for_gsi(gsi).ok_or(IrqError::NotFound)?;

        let _guard = self.mmio_lock.lock();
        let virtual_base = someboot::mem::phys_to_virt(physical_base as usize) as u64;
        let mut ioapic = unsafe { IoApic::new(virtual_base) };
        unsafe {
            if enabled {
                ioapic.enable_irq(input);
            } else {
                ioapic.disable_irq(input);
            }
        }
        Ok(())
    }

    fn reserve_gsi_endpoint(
        &self,
        route: &AcpiGsiRoute,
    ) -> Result<IoApicEndpointReservation<'_>, IrqError> {
        let key = encode_gsi_key(route.gsi);
        let encoded_route =
            encode_ioapic_gsi_route(route.controller_address, route.controller_input)?;
        let start = endpoint_slot_start(route.gsi);
        let mut first_empty = None;

        for offset in 0..EXTERNAL_VECTOR_CAPACITY {
            let slot_index = (start + offset) % EXTERNAL_VECTOR_CAPACITY;
            let slot = &self.endpoint_slots[slot_index];
            let existing_key = slot.gsi_key.load(Ordering::Acquire);
            if existing_key == key {
                let existing_route = slot.route.load(Ordering::Acquire);
                return if existing_route == encoded_route {
                    Ok(IoApicEndpointReservation::existing(
                        slot,
                        key,
                        encoded_route,
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
        Ok(IoApicEndpointReservation::new(slot, key, encoded_route))
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
}

impl IoApicEndpointSlot {
    const fn new() -> Self {
        Self {
            gsi_key: AtomicU64::new(0),
            route: AtomicU64::new(UNPUBLISHED_IOAPIC_GSI_ROUTE),
        }
    }
}

struct IoApicEndpointReservation<'a> {
    slot: &'a IoApicEndpointSlot,
    key: u64,
    route: u64,
    newly_reserved: bool,
    published: bool,
}

impl<'a> IoApicEndpointReservation<'a> {
    const fn new(slot: &'a IoApicEndpointSlot, key: u64, route: u64) -> Self {
        Self {
            slot,
            key,
            route,
            newly_reserved: true,
            published: false,
        }
    }

    const fn existing(slot: &'a IoApicEndpointSlot, key: u64, route: u64) -> Self {
        Self {
            slot,
            key,
            route,
            newly_reserved: false,
            published: true,
        }
    }

    fn publish(mut self) {
        if self.newly_reserved {
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

struct X86IoApicIntc {
    ioapics: Vec<X86IoApic>,
    routes: Vec<ProgrammedIoApicRoute>,
    destinations: Vec<(usize, u8)>,
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
            routes: Vec::with_capacity(EXTERNAL_VECTOR_CAPACITY),
            destinations: Vec::with_capacity(EXTERNAL_VECTOR_CAPACITY),
        }
    }

    fn remember_route(&mut self, route: ProgrammedIoApicRoute) {
        if let Some(existing) = self.routes.iter_mut().find(|r| {
            r.acpi.controller_id == route.acpi.controller_id
                && r.acpi.controller_address == route.acpi.controller_address
                && r.acpi.gsi == route.acpi.gsi
        }) {
            *existing = route;
        } else {
            self.routes.push(route);
        }
    }

    fn route_for_gsi(&self, gsi: u32) -> Option<ProgrammedIoApicRoute> {
        self.routes
            .iter()
            .copied()
            .find(|route| route.acpi.gsi == gsi)
    }

    fn set_gsi_enable(&mut self, gsi: u32, enable: bool) -> bool {
        let Some(route) = self.route_for_gsi(gsi) else {
            return false;
        };
        self.set_route_enable(&route, enable)
    }

    fn set_route_enable(&mut self, route: &ProgrammedIoApicRoute, enable: bool) -> bool {
        let dest = self.destination_for_vector(usize::from(route.vector));
        for ioapic in &mut self.ioapics {
            if ioapic.contains_route(&route.acpi) {
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
        let Some(route) = self.route_for_gsi(gsi) else {
            return false;
        };

        for ioapic in &mut self.ioapics {
            if ioapic.contains_route(&route.acpi) {
                ioapic.set_route_destination(&route, dest);
                self.remember_destination(usize::from(route.vector), dest);
                return true;
            }
        }
        false
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

    fn set_route_destination(&mut self, route: &ProgrammedIoApicRoute, dest: u8) {
        if !self.contains_route(&route.acpi) {
            return;
        }

        let _guard = IOAPIC_CPU_IF.mmio_lock.lock();
        unsafe {
            let input = route.acpi.controller_input;
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
        if !self.supports_acpi_gsi(route) {
            return Err(IrqError::Unsupported);
        }

        // Reserve every software resource before touching IOREGSEL/IOWIN. The
        // tokens roll back automatically on any validation or programming
        // failure. Once masked MMIO programming succeeds, publication is a
        // pair of infallible Release stores: vector first, endpoint last.
        let endpoint = IOAPIC_CPU_IF.reserve_gsi_endpoint(route)?;
        let vector = IOAPIC_CPU_IF.reserve_external_vector(translation.id, route.gsi)?;
        let programmed = ProgrammedIoApicRoute {
            acpi: *route,
            vector: vector.vector(),
        };
        if !self.set_route_enable(&programmed, false) {
            return Err(IrqError::Unsupported);
        }
        vector.publish();
        endpoint.publish();
        self.remember_route(programmed);
        Ok(())
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
            return IOAPIC_CPU_IF.set_gsi_enabled(irq.hwirq.0, enable);
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
