use alloc::{
    collections::BTreeSet,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use core::{ptr::NonNull, str::FromStr};

use acpi::{
    AcpiError, AcpiTables, Handler, PhysicalMapping,
    aml::{
        AmlError, Interpreter,
        namespace::{AmlName, NamespaceLevelKind},
        object::Object,
        pci_routing::{IrqDescriptor, PciRoutingTable, Pin},
    },
    platform::{
        AcpiPlatform,
        interrupt::{InterruptModel, Polarity, TriggerMode},
        pci::PciConfigRegions,
    },
};
pub use rdif_base::irq::{AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger};
use spin::{Mutex, Once};

use crate::{
    DeviceId, PlatformDevice,
    error::DriverError,
    probe::{
        OnProbeError, ProbeError,
        pci::{PciAddress, PciInfo, PciIntxRoute},
    },
    register::{DriverRegister, ProbeKind},
};

pub const PCI_INTX_VECTOR_BASE: usize = 0x30;
const PCI_ROOT_FALLBACK_PATHS: &[&str] = &["\\_SB.PCI0", "\\_SB.PCI1", "\\_SB.PC00", "\\_SB.PC01"];

static SYSTEM: Once<System> = Once::new();
static NULL_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy)]
pub struct AcpiRoot {
    pub rsdp: usize,
    pub phys_to_virt: fn(usize) -> *mut u8,
}

impl core::fmt::Debug for AcpiRoot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AcpiRoot")
            .field("rsdp", &self.rsdp)
            .finish_non_exhaustive()
    }
}

impl AcpiRoot {
    pub const fn new(rsdp: usize, phys_to_virt: fn(usize) -> *mut u8) -> Self {
        Self { rsdp, phys_to_virt }
    }

    pub const fn identity(rsdp: usize) -> Self {
        Self::new(rsdp, identity_phys_to_virt)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiId {
    pub hid: &'static str,
    pub cids: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiPciEcam {
    pub segment_group: u16,
    pub bus_start: u8,
    pub bus_end: u8,
    pub base_address: u64,
}

impl AcpiPciEcam {
    pub fn size(&self) -> usize {
        let buses = usize::from(self.bus_end.saturating_sub(self.bus_start)) + 1;
        buses << 20
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiIoApic {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
    pub redirection_entries: u8,
}

impl AcpiIoApic {
    fn contains_gsi(self, gsi: u32) -> bool {
        let start = self.gsi_base;
        let end = start.saturating_add(u32::from(self.redirection_entries));
        (start..end).contains(&gsi)
    }
}

#[derive(Debug, Clone)]
pub struct AcpiRouting {
    io_apics: Vec<AcpiIoApic>,
}

impl AcpiRouting {
    pub const fn new() -> Self {
        Self {
            io_apics: Vec::new(),
        }
    }

    pub fn add_io_apic(&mut self, io_apic: AcpiIoApic) {
        self.io_apics.push(io_apic);
    }

    pub fn io_apics(&self) -> &[AcpiIoApic] {
        &self.io_apics
    }

    pub fn resolve_gsi(&self, gsi: u32) -> Option<AcpiGsiRoute> {
        let io_apic = self
            .io_apics
            .iter()
            .copied()
            .find(|io_apic| io_apic.contains_gsi(gsi))?;
        let input = u8::try_from(gsi.saturating_sub(io_apic.gsi_base)).ok()?;
        Some(AcpiGsiRoute {
            gsi,
            vector: PCI_INTX_VECTOR_BASE + gsi as usize,
            controller_id: io_apic.id,
            controller_address: io_apic.address,
            controller_input: input,
            trigger: AcpiIrqTrigger::Level,
            polarity: AcpiIrqPolarity::ActiveLow,
        })
    }

    pub fn resolve_vector(&self, vector: usize) -> Option<AcpiGsiRoute> {
        let gsi = vector.checked_sub(PCI_INTX_VECTOR_BASE)?;
        self.resolve_gsi(gsi as u32)
    }
}

impl Default for AcpiRouting {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use acpi::aml::{
        pci_routing::IrqDescriptor,
        resource::{InterruptPolarity, InterruptTrigger},
    };

    use super::{
        AcpiIoApic, AcpiIrqPolarity, AcpiIrqTrigger, AcpiRouting, is_pci_gsi,
        route_with_irq_descriptor_flags,
    };

    #[test]
    fn ioapic_routes_map_gsi_to_stable_vector() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });

        let irq = routing
            .resolve_gsi(16)
            .expect("gsi 16 should be handled by the IOAPIC");
        assert_eq!(irq.gsi, 16);
        assert_eq!(irq.controller_id, 0);
        assert_eq!(irq.controller_address, 0xfec0_0000);
        assert_eq!(irq.controller_input, 16);
        assert_eq!(irq.vector, 0x40);
        assert_eq!(irq.trigger, AcpiIrqTrigger::Level);
        assert_eq!(irq.polarity, AcpiIrqPolarity::ActiveLow);
        assert!(routing.resolve_gsi(24).is_none());
    }

    #[test]
    fn pci_intx_rejects_legacy_pic_irqs() {
        for irq in 0..16 {
            assert!(!is_pci_gsi(irq), "IRQ {irq} should use fallback routing");
        }

        for irq in [16, 17, 23, 24, 32] {
            assert!(is_pci_gsi(irq), "GSI {irq} should be accepted");
        }
    }

    #[test]
    fn pci_irq_route_preserves_descriptor_trigger_and_polarity() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });
        let route = routing.resolve_gsi(16).unwrap();
        let descriptor = IrqDescriptor {
            is_consumer: true,
            trigger: InterruptTrigger::Edge,
            polarity: InterruptPolarity::ActiveHigh,
            is_shared: false,
            is_wake_capable: false,
            irq: 16,
        };

        let route = route_with_irq_descriptor_flags(route, &descriptor);

        assert_eq!(route.trigger, AcpiIrqTrigger::Edge);
        assert_eq!(route.polarity, AcpiIrqPolarity::ActiveHigh);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiPciIrqRoute {
    pub address: PciAddress,
    pub interrupt_pin: u8,
    pub intx_route: PciIntxRoute,
    pub gsi: AcpiGsiRoute,
}

pub struct AcpiInfo<'a> {
    pub root: &'a System,
    pub path: &'a str,
    pub irq_route: Option<AcpiGsiRoute>,
}

impl AcpiInfo<'_> {
    pub const fn irq_route(&self) -> Option<AcpiGsiRoute> {
        self.irq_route
    }
}

pub struct ProbeAcpi<'a> {
    info: AcpiInfo<'a>,
    platform: PlatformDevice,
}

impl<'a> ProbeAcpi<'a> {
    #[allow(dead_code)]
    pub(crate) fn new(info: AcpiInfo<'a>, platform: PlatformDevice) -> Self {
        Self { info, platform }
    }

    pub const fn info(&self) -> &AcpiInfo<'a> {
        &self.info
    }

    pub fn into_platform_device(self) -> PlatformDevice {
        self.platform
    }

    pub fn into_parts(self) -> (AcpiInfo<'a>, PlatformDevice) {
        (self.info, self.platform)
    }
}

pub type FnOnProbe = for<'a> fn(ProbeAcpi<'a>) -> Result<(), OnProbeError>;

pub fn check_root(root: AcpiRoot) -> Result<(), DriverError> {
    if root.rsdp == 0 {
        return Err(acpi_error(AcpiError::NoValidRsdp));
    }
    root.tables().map(|_| ()).map_err(acpi_error)
}

pub fn init(root: AcpiRoot) -> Result<(), DriverError> {
    let system = System::new(root)?;
    info!(
        "ACPI initialized: {} PCI ECAM region(s), {} IOAPIC(s)",
        system.pci_ecam_regions().len(),
        system.routing().io_apics().len()
    );
    SYSTEM.call_once(|| system);
    Ok(())
}

pub(crate) fn try_probe_register(
    register: &DriverRegister,
) -> Option<Result<alloc::vec::Vec<Result<(), OnProbeError>>, ProbeError>> {
    SYSTEM.get().map(|system| system.probe_register(register))
}

pub(crate) fn try_system() -> Option<&'static System> {
    SYSTEM.get()
}

pub fn with_acpi<T>(f: impl FnOnce(&System) -> T) -> Option<T> {
    try_system().map(f)
}

fn acpi_error(err: AcpiError) -> DriverError {
    DriverError::Unknown(format!("{err:?}"))
}

fn on_probe_error(err: impl core::fmt::Debug) -> OnProbeError {
    OnProbeError::other(format!("{err:?}"))
}

fn identity_phys_to_virt(paddr: usize) -> *mut u8 {
    paddr as *mut u8
}

#[derive(Clone)]
struct AcpiHandler {
    root: AcpiRoot,
    pci_ecam_regions: Rc<Vec<AcpiPciEcam>>,
}

impl AcpiHandler {
    fn new(root: AcpiRoot, pci_ecam_regions: Vec<AcpiPciEcam>) -> Self {
        Self {
            root,
            pci_ecam_regions: Rc::new(pci_ecam_regions),
        }
    }

    fn virt_addr(&self, physical_address: usize) -> usize {
        (self.root.phys_to_virt)(physical_address) as usize
    }

    fn pci_config_ptr(
        &self,
        address: acpi::PciAddress,
        offset: u16,
        width: usize,
    ) -> Option<*mut u8> {
        let offset = usize::from(offset);
        if offset.checked_add(width)? > 4096 {
            return None;
        }

        let bus = address.bus();
        let region = self.pci_ecam_regions.iter().find(|region| {
            address.segment() == region.segment_group
                && bus >= region.bus_start
                && bus <= region.bus_end
        })?;
        let bus_offset = usize::from(bus - region.bus_start) << 20;
        let device_offset = usize::from(address.device()) << 15;
        let function_offset = usize::from(address.function()) << 12;
        let physical_address =
            region.base_address as usize + bus_offset + device_offset + function_offset + offset;

        Some((self.root.phys_to_virt)(physical_address))
    }
}

impl Handler for AcpiHandler {
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> PhysicalMapping<Self, T> {
        PhysicalMapping {
            physical_start: physical_address,
            virtual_start: NonNull::new(self.virt_addr(physical_address) as *mut T)
                .expect("ACPI physical mapping must not be null"),
            region_length: size,
            mapped_length: size,
            handler: self.clone(),
        }
    }

    fn unmap_physical_region<T>(_region: &PhysicalMapping<Self, T>) {}

    fn read_u8(&self, address: usize) -> u8 {
        unsafe { (self.virt_addr(address) as *const u8).read_volatile() }
    }

    fn read_u16(&self, address: usize) -> u16 {
        unsafe { (self.virt_addr(address) as *const u16).read_volatile() }
    }

    fn read_u32(&self, address: usize) -> u32 {
        unsafe { (self.virt_addr(address) as *const u32).read_volatile() }
    }

    fn read_u64(&self, address: usize) -> u64 {
        unsafe { (self.virt_addr(address) as *const u64).read_volatile() }
    }

    fn write_u8(&self, address: usize, value: u8) {
        unsafe { (self.virt_addr(address) as *mut u8).write_volatile(value) }
    }

    fn write_u16(&self, address: usize, value: u16) {
        unsafe { (self.virt_addr(address) as *mut u16).write_volatile(value) }
    }

    fn write_u32(&self, address: usize, value: u32) {
        unsafe { (self.virt_addr(address) as *mut u32).write_volatile(value) }
    }

    fn write_u64(&self, address: usize, value: u64) {
        unsafe { (self.virt_addr(address) as *mut u64).write_volatile(value) }
    }

    fn read_io_u8(&self, port: u16) -> u8 {
        read_io_u8(port)
    }

    fn read_io_u16(&self, port: u16) -> u16 {
        read_io_u16(port)
    }

    fn read_io_u32(&self, port: u16) -> u32 {
        read_io_u32(port)
    }

    fn write_io_u8(&self, port: u16, value: u8) {
        write_io_u8(port, value);
    }

    fn write_io_u16(&self, port: u16, value: u16) {
        write_io_u16(port, value);
    }

    fn write_io_u32(&self, port: u16, value: u32) {
        write_io_u32(port, value);
    }

    fn read_pci_u8(&self, address: acpi::PciAddress, offset: u16) -> u8 {
        if let Some(ptr) = self.pci_config_ptr(address, offset, 1) {
            return unsafe { ptr.read_volatile() };
        }
        pci_legacy_read_u8(address, offset).unwrap_or(u8::MAX)
    }

    fn read_pci_u16(&self, address: acpi::PciAddress, offset: u16) -> u16 {
        let lo = u16::from(self.read_pci_u8(address, offset));
        let hi = u16::from(self.read_pci_u8(address, offset.saturating_add(1)));
        lo | (hi << 8)
    }

    fn read_pci_u32(&self, address: acpi::PciAddress, offset: u16) -> u32 {
        let b0 = u32::from(self.read_pci_u8(address, offset));
        let b1 = u32::from(self.read_pci_u8(address, offset.saturating_add(1)));
        let b2 = u32::from(self.read_pci_u8(address, offset.saturating_add(2)));
        let b3 = u32::from(self.read_pci_u8(address, offset.saturating_add(3)));
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    fn write_pci_u8(&self, address: acpi::PciAddress, offset: u16, value: u8) {
        if let Some(ptr) = self.pci_config_ptr(address, offset, 1) {
            unsafe { ptr.write_volatile(value) };
            return;
        }
        pci_legacy_write_u8(address, offset, value);
    }

    fn write_pci_u16(&self, address: acpi::PciAddress, offset: u16, value: u16) {
        self.write_pci_u8(address, offset, value as u8);
        self.write_pci_u8(address, offset.saturating_add(1), (value >> 8) as u8);
    }

    fn write_pci_u32(&self, address: acpi::PciAddress, offset: u16, value: u32) {
        self.write_pci_u8(address, offset, value as u8);
        self.write_pci_u8(address, offset.saturating_add(1), (value >> 8) as u8);
        self.write_pci_u8(address, offset.saturating_add(2), (value >> 16) as u8);
        self.write_pci_u8(address, offset.saturating_add(3), (value >> 24) as u8);
    }

    fn nanos_since_boot(&self) -> u64 {
        0
    }

    fn stall(&self, microseconds: u64) {
        for _ in 0..microseconds.saturating_mul(100) {
            core::hint::spin_loop();
        }
    }

    fn sleep(&self, milliseconds: u64) {
        self.stall(milliseconds.saturating_mul(1000));
    }

    fn create_mutex(&self) -> acpi::Handle {
        acpi::Handle(0)
    }

    fn acquire(&self, _mutex: acpi::Handle, _timeout: u16) -> Result<(), acpi::aml::AmlError> {
        let _guard = NULL_LOCK.lock();
        Ok(())
    }

    fn release(&self, _mutex: acpi::Handle) {}
}

impl AcpiRoot {
    fn handler(self) -> AcpiHandler {
        AcpiHandler::new(self, Vec::new())
    }

    fn handler_with_pci_ecam(self, pci_ecam_regions: Vec<AcpiPciEcam>) -> AcpiHandler {
        AcpiHandler::new(self, pci_ecam_regions)
    }

    fn tables(self) -> Result<AcpiTables<AcpiHandler>, AcpiError> {
        unsafe { AcpiTables::from_rsdp(self.handler(), self.rsdp) }
    }
}

pub struct System {
    ecam_regions: Vec<AcpiPciEcam>,
    routing: AcpiRouting,
    pci: Option<AcpiPciNamespace>,
    probed_names: Mutex<BTreeSet<&'static str>>,
}

unsafe impl Send for System {}
unsafe impl Sync for System {}

struct AcpiPciNamespace {
    interpreter: Interpreter<AcpiHandler>,
    roots: Vec<AcpiPciRoot>,
}

struct AcpiPciRoot {
    segment: u16,
    bus: u8,
    path: String,
    prt: Option<PciRoutingTable>,
}

impl System {
    pub fn new(root: AcpiRoot) -> Result<Self, DriverError> {
        let tables = root.tables().map_err(acpi_error)?;
        let ecam_regions = read_pci_ecam_regions(&tables)?;
        let routing = read_interrupt_routing(&tables)?;
        let pci = match read_pci_namespace(root, ecam_regions.clone()) {
            Ok(pci) => Some(pci),
            Err(err) => {
                warn!("failed to discover ACPI PCI namespace: {err:?}");
                None
            }
        };

        Ok(Self {
            ecam_regions,
            routing,
            pci,
            probed_names: Mutex::new(BTreeSet::new()),
        })
    }

    pub fn pci_ecam_regions(&self) -> &[AcpiPciEcam] {
        &self.ecam_regions
    }

    pub fn routing(&self) -> &AcpiRouting {
        &self.routing
    }

    pub fn pci_irq_for_endpoint(
        &self,
        info: PciInfo,
    ) -> Result<Option<AcpiPciIrqRoute>, OnProbeError> {
        let Some(intx_route) = info.intx_route else {
            return Ok(None);
        };
        let Some(irq) = self.resolve_endpoint_gsi(info.address, intx_route)? else {
            return Ok(None);
        };
        let Some(gsi) = irq_descriptor_gsi(&irq) else {
            return Err(OnProbeError::other(format!(
                "ACPI PCI endpoint {} pin {} returned an invalid IRQ descriptor: {:?}",
                info.address, intx_route.root_pin, irq
            )));
        };
        if !is_pci_gsi(gsi) {
            return Ok(None);
        }

        let Some(route) = self.routing.resolve_gsi(gsi) else {
            return Err(OnProbeError::other(format!(
                "ACPI GSI {} for PCI endpoint {} is not covered by an IOAPIC",
                gsi, info.address
            )));
        };
        let route = route_with_irq_descriptor_flags(route, &irq);

        Ok(Some(AcpiPciIrqRoute {
            address: info.address,
            interrupt_pin: intx_route.root_pin,
            intx_route,
            gsi: route,
        }))
    }

    fn resolve_endpoint_gsi(
        &self,
        address: PciAddress,
        route: PciIntxRoute,
    ) -> Result<Option<IrqDescriptor>, OnProbeError> {
        let pin = acpi_pin(route.root_pin)?;
        let Some(pci) = &self.pci else {
            return Ok(None);
        };
        let roots = self.pci_root_candidates(address, pci);
        if roots.is_empty() {
            return Ok(None);
        }

        for root in roots {
            let Some(prt) = &root.prt else {
                continue;
            };

            match prt.route(
                u16::from(route.root_device),
                u16::from(route.root_function),
                pin,
                &pci.interpreter,
            ) {
                Ok(route) => return Ok(Some(route)),
                Err(AmlError::PrtNoEntry) => {}
                Err(err) => return Err(on_probe_error(err)),
            }
        }

        Ok(None)
    }

    fn pci_root_candidates<'a>(
        &self,
        address: PciAddress,
        pci: &'a AcpiPciNamespace,
    ) -> Vec<&'a AcpiPciRoot> {
        let mut roots = Vec::new();
        if let Some(root) = pci
            .roots
            .iter()
            .find(|root| root.segment == address.segment() && root.bus == address.bus())
            .or_else(|| {
                pci.roots
                    .iter()
                    .find(|root| root.segment == address.segment() && root.bus == 0)
            })
        {
            roots.push(root);
        }
        for path in PCI_ROOT_FALLBACK_PATHS {
            if let Some(root) = pci.roots.iter().find(|root| root.path == *path)
                && !roots.iter().any(|candidate| candidate.path == root.path)
            {
                roots.push(root);
            }
        }
        roots
    }

    fn probe_register(
        &self,
        register: &DriverRegister,
    ) -> Result<Vec<Result<(), OnProbeError>>, ProbeError> {
        let mut out = Vec::new();
        for probe in register.probe_kinds {
            let ProbeKind::Acpi { ids, on_probe } = probe else {
                continue;
            };
            if ids.is_empty() {
                continue;
            }
            if self.probed_names.lock().contains(register.name) {
                continue;
            }

            let desc = crate::Descriptor {
                name: register.name,
                device_id: DeviceId::new(),
                irq_parent: None,
            };
            let info = AcpiInfo {
                root: self,
                path: "\\",
                irq_route: None,
            };
            let res = on_probe(ProbeAcpi::new(info, PlatformDevice::new(desc)));
            if res.is_ok() {
                self.probed_names.lock().insert(register.name);
            }
            out.push(res);
        }
        Ok(out)
    }
}

fn read_pci_ecam_regions(
    tables: &AcpiTables<AcpiHandler>,
) -> Result<Vec<AcpiPciEcam>, DriverError> {
    let regions = PciConfigRegions::new(tables).map_err(acpi_error)?;
    Ok(regions
        .regions
        .iter()
        .map(|region| AcpiPciEcam {
            segment_group: region.pci_segment_group,
            bus_start: region.bus_number_start,
            bus_end: region.bus_number_end,
            base_address: region.base_address,
        })
        .collect())
}

fn read_interrupt_routing(tables: &AcpiTables<AcpiHandler>) -> Result<AcpiRouting, DriverError> {
    let (model, _) = InterruptModel::new(tables).map_err(acpi_error)?;
    let mut routing = AcpiRouting::new();
    if let InterruptModel::Apic(apic) = model {
        for io_apic in &apic.io_apics {
            routing.add_io_apic(AcpiIoApic {
                id: io_apic.id,
                address: io_apic.address,
                gsi_base: io_apic.global_system_interrupt_base,
                redirection_entries: 24,
            });
        }
    }
    Ok(routing)
}

fn read_pci_namespace(
    root: AcpiRoot,
    ecam_regions: Vec<AcpiPciEcam>,
) -> Result<AcpiPciNamespace, AcpiError> {
    let handler = root.handler_with_pci_ecam(ecam_regions);
    let tables = unsafe { AcpiTables::from_rsdp(handler.clone(), root.rsdp) }?;
    let platform = AcpiPlatform::new(tables, handler)?;
    let interpreter = Interpreter::new_from_platform(&platform)?;
    interpreter.initialize_namespace();

    let mut roots = Vec::new();
    {
        let mut namespace = interpreter.namespace.lock().clone();
        namespace
            .traverse(|path, level| {
                if level.kind == NamespaceLevelKind::Device && is_pci_root(&interpreter, path) {
                    let segment =
                        eval_integer_child(&interpreter, path, "_SEG")?.unwrap_or(0) as u16;
                    let bus = eval_integer_child(&interpreter, path, "_BBN")?.unwrap_or(0) as u8;
                    roots.push(AcpiPciRoot {
                        segment,
                        bus,
                        path: path.as_string(),
                        prt: None,
                    });
                }
                Ok(true)
            })
            .map_err(AcpiError::Aml)?;
    }

    for root in &mut roots {
        root.prt = read_pci_routing_table(&interpreter, &root.path)?;
    }
    for path in PCI_ROOT_FALLBACK_PATHS {
        if roots.iter().any(|root| root.path == *path) {
            continue;
        }
        let Some(prt) = read_pci_routing_table(&interpreter, path)? else {
            continue;
        };
        roots.push(AcpiPciRoot {
            segment: 0,
            bus: 0,
            path: path.to_string(),
            prt: Some(prt),
        });
    }

    Ok(AcpiPciNamespace { interpreter, roots })
}

fn read_pci_routing_table(
    interpreter: &Interpreter<AcpiHandler>,
    root_path: &str,
) -> Result<Option<PciRoutingTable>, AcpiError> {
    let prt_path = AmlName::from_str(&format!("{root_path}._PRT")).map_err(AcpiError::Aml)?;
    match PciRoutingTable::from_prt_path(prt_path, interpreter) {
        Ok(prt) => Ok(Some(prt)),
        Err(AmlError::ObjectDoesNotExist(_)) | Err(AmlError::LevelDoesNotExist(_)) => Ok(None),
        Err(err) => Err(AcpiError::Aml(err)),
    }
}

fn is_pci_root(interpreter: &Interpreter<AcpiHandler>, path: &AmlName) -> bool {
    has_pci_root_id(interpreter, path, "_HID") || has_pci_root_id(interpreter, path, "_CID")
}

fn has_pci_root_id(interpreter: &Interpreter<AcpiHandler>, path: &AmlName, name: &str) -> bool {
    let Ok(Some(value)) = eval_child(interpreter, path, name) else {
        return false;
    };
    object_matches_pci_root_id(&value)
}

fn object_matches_pci_root_id(value: &Object) -> bool {
    match value {
        Object::String(id) => matches!(id.as_str(), "PNP0A03" | "PNP0A08"),
        Object::Integer(id) => {
            let id = decode_eisa_id(*id as u32);
            matches!(id.as_deref(), Some("PNP0A03" | "PNP0A08"))
        }
        Object::Package(values) => values.iter().any(|value| object_matches_pci_root_id(value)),
        _ => false,
    }
}

fn eval_integer_child(
    interpreter: &Interpreter<AcpiHandler>,
    path: &AmlName,
    name: &str,
) -> Result<Option<u64>, AmlError> {
    eval_child(interpreter, path, name)?
        .map(|value| value.as_integer())
        .transpose()
}

fn eval_child(
    interpreter: &Interpreter<AcpiHandler>,
    path: &AmlName,
    name: &str,
) -> Result<Option<Rc<Object>>, AmlError> {
    let child = AmlName::from_str(name)?.resolve(path)?;
    match interpreter.evaluate_if_present(child, Vec::new())? {
        Some(value) => Ok(Some(Rc::new((*value).clone()))),
        None => Ok(None),
    }
}

fn decode_eisa_id(raw: u32) -> Option<String> {
    if raw == 0 {
        return None;
    }
    let chars = [
        (((raw >> 26) & 0x1f) as u8).wrapping_add(b'@'),
        (((raw >> 21) & 0x1f) as u8).wrapping_add(b'@'),
        (((raw >> 16) & 0x1f) as u8).wrapping_add(b'@'),
    ];
    if !chars.iter().all(u8::is_ascii_uppercase) {
        return None;
    }
    Some(format!(
        "{}{}{}{:04X}",
        chars[0] as char,
        chars[1] as char,
        chars[2] as char,
        raw & 0xffff
    ))
}

fn acpi_pin(interrupt_pin: u8) -> Result<Pin, OnProbeError> {
    match interrupt_pin {
        1 => Ok(Pin::IntA),
        2 => Ok(Pin::IntB),
        3 => Ok(Pin::IntC),
        4 => Ok(Pin::IntD),
        _ => Err(OnProbeError::other(format!(
            "invalid PCI interrupt pin {interrupt_pin}"
        ))),
    }
}

fn irq_descriptor_gsi(descriptor: &IrqDescriptor) -> Option<u32> {
    let irq = descriptor.irq;
    if !descriptor.is_consumer && irq.count_ones() == 1 && irq <= u16::MAX as u32 {
        Some(irq.trailing_zeros())
    } else {
        Some(irq)
    }
}

fn route_with_irq_descriptor_flags(
    route: AcpiGsiRoute,
    descriptor: &IrqDescriptor,
) -> AcpiGsiRoute {
    AcpiGsiRoute {
        trigger: irq_trigger(descriptor.trigger),
        polarity: irq_polarity(descriptor.polarity),
        ..route
    }
}

fn is_pci_gsi(irq: u32) -> bool {
    irq >= 16
}

fn irq_trigger(trigger: acpi::aml::resource::InterruptTrigger) -> AcpiIrqTrigger {
    match trigger {
        acpi::aml::resource::InterruptTrigger::Edge => AcpiIrqTrigger::Edge,
        acpi::aml::resource::InterruptTrigger::Level => AcpiIrqTrigger::Level,
    }
}

fn irq_polarity(polarity: acpi::aml::resource::InterruptPolarity) -> AcpiIrqPolarity {
    match polarity {
        acpi::aml::resource::InterruptPolarity::ActiveHigh => AcpiIrqPolarity::ActiveHigh,
        acpi::aml::resource::InterruptPolarity::ActiveLow => AcpiIrqPolarity::ActiveLow,
    }
}

#[cfg(target_arch = "x86_64")]
fn pci_legacy_read_u8(address: acpi::PciAddress, offset: u16) -> Option<u8> {
    let value = pci_legacy_read_aligned_u32(address, offset)?;
    let shift = u32::from(offset & 0b11) * 8;
    Some((value >> shift) as u8)
}

#[cfg(not(target_arch = "x86_64"))]
fn pci_legacy_read_u8(_address: acpi::PciAddress, _offset: u16) -> Option<u8> {
    None
}

#[cfg(target_arch = "x86_64")]
fn pci_legacy_write_u8(address: acpi::PciAddress, offset: u16, value: u8) {
    let Some(old) = pci_legacy_read_aligned_u32(address, offset) else {
        return;
    };
    let shift = u32::from(offset & 0b11) * 8;
    let mask = 0xff_u32 << shift;
    let new = (old & !mask) | (u32::from(value) << shift);
    pci_legacy_write_aligned_u32(address, offset, new);
}

#[cfg(not(target_arch = "x86_64"))]
fn pci_legacy_write_u8(_address: acpi::PciAddress, _offset: u16, _value: u8) {}

#[cfg(target_arch = "x86_64")]
fn pci_legacy_config_address(address: acpi::PciAddress, offset: u16) -> Option<u32> {
    if address.segment() != 0 || offset >= 256 {
        return None;
    }

    Some(
        0x8000_0000
            | (u32::from(address.bus()) << 16)
            | (u32::from(address.device()) << 11)
            | (u32::from(address.function()) << 8)
            | u32::from(offset & !0b11),
    )
}

#[cfg(target_arch = "x86_64")]
fn pci_legacy_read_aligned_u32(address: acpi::PciAddress, offset: u16) -> Option<u32> {
    let config_address = pci_legacy_config_address(address, offset)?;
    unsafe {
        x86::io::outl(0xcf8, config_address);
        Some(x86::io::inl(0xcfc))
    }
}

#[cfg(target_arch = "x86_64")]
fn pci_legacy_write_aligned_u32(address: acpi::PciAddress, offset: u16, value: u32) {
    if let Some(config_address) = pci_legacy_config_address(address, offset) {
        unsafe {
            x86::io::outl(0xcf8, config_address);
            x86::io::outl(0xcfc, value);
        }
    }
}

pub fn acpi_trigger(trigger: TriggerMode) -> AcpiIrqTrigger {
    match trigger {
        TriggerMode::Edge => AcpiIrqTrigger::Edge,
        TriggerMode::Level => AcpiIrqTrigger::Level,
        _ => AcpiIrqTrigger::Level,
    }
}

pub fn acpi_polarity(polarity: Polarity) -> AcpiIrqPolarity {
    match polarity {
        Polarity::ActiveHigh => AcpiIrqPolarity::ActiveHigh,
        Polarity::ActiveLow => AcpiIrqPolarity::ActiveLow,
        _ => AcpiIrqPolarity::ActiveLow,
    }
}

#[cfg(target_arch = "x86_64")]
fn read_io_u8(port: u16) -> u8 {
    unsafe { x86::io::inb(port) }
}

#[cfg(not(target_arch = "x86_64"))]
fn read_io_u8(_port: u16) -> u8 {
    0
}

#[cfg(target_arch = "x86_64")]
fn read_io_u16(port: u16) -> u16 {
    unsafe { x86::io::inw(port) }
}

#[cfg(not(target_arch = "x86_64"))]
fn read_io_u16(_port: u16) -> u16 {
    0
}

#[cfg(target_arch = "x86_64")]
fn read_io_u32(port: u16) -> u32 {
    unsafe { x86::io::inl(port) }
}

#[cfg(not(target_arch = "x86_64"))]
fn read_io_u32(_port: u16) -> u32 {
    0
}

#[cfg(target_arch = "x86_64")]
fn write_io_u8(port: u16, value: u8) {
    unsafe { x86::io::outb(port, value) }
}

#[cfg(not(target_arch = "x86_64"))]
fn write_io_u8(_port: u16, _value: u8) {}

#[cfg(target_arch = "x86_64")]
fn write_io_u16(port: u16, value: u16) {
    unsafe { x86::io::outw(port, value) }
}

#[cfg(not(target_arch = "x86_64"))]
fn write_io_u16(_port: u16, _value: u16) {}

#[cfg(target_arch = "x86_64")]
fn write_io_u32(port: u16, value: u32) {
    unsafe { x86::io::outl(port, value) }
}

#[cfg(not(target_arch = "x86_64"))]
fn write_io_u32(_port: u16, _value: u32) {}
