use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use core::{cell::Cell, mem::size_of, ptr::NonNull, str::FromStr};

use acpi::{
    AcpiError, AcpiTables, AmlTable, Handler, PhysicalMapping, RawPhysicalMapping,
    address::{AddressSpace, GenericAddress},
    aml::{
        AmlError, Interpreter,
        namespace::{AmlName, NamespaceLevelKind},
        object::{FieldUnit, FieldUnitKind, FieldUpdateRule, Object, ObjectType},
        op_region::{OpRegion, RegionSpace},
        pci_routing::{IrqDescriptor, PciRoutingTable, Pin},
        resource::{
            AddressSpaceResourceType, InterruptPolarity, InterruptTrigger, Irqs, Resource,
            resource_descriptor_list,
        },
    },
    platform::{
        AcpiPlatform,
        interrupt::{InterruptModel, Polarity, TriggerMode},
    },
    sdt::{SdtHeader, Signature, facs::Facs, fadt::Fadt, mcfg::Mcfg, spcr::SpcrInterfaceType},
};
use ax_lazyinit::OnceLock;
use ax_sync::RawSpinLock;
use kernutil::{
    acpi::spcr_mappings,
    memory::{MemoryDescriptor, MemoryType},
};
pub use pcie::ConfigSpaceLayout;
pub use rdif_base::irq::{AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger};

use crate::{
    DeviceId, DriverGeneric, PlatformDevice,
    error::DriverError,
    probe::{
        OnProbeError, ProbeError,
        pci::{PciAddress, PciInfo, PciIntxRoute},
    },
    register::{DriverRegister, ProbeKind},
};

pub const PCI_INTX_VECTOR_BASE: usize = 0x30;
const LOONGARCH_PCH_PIC_GSI_COUNT: u16 = 256;
// Linux 7.1's LoongArch ACPI boundary assigns GSI 16..48 to LIO's 32 inputs.
const LOONGARCH_LIO_GSI_BASE: u32 = 16;
const LOONGARCH_LIO_INPUTS: u32 = 32;
const PCI_ROOT_FALLBACK_PATHS: &[&str] = &["\\_SB.PCI0", "\\_SB.PCI1", "\\_SB.PC00", "\\_SB.PC01"];

static SYSTEM: OnceLock<System> = OnceLock::new();
static NULL_LOCK: RawSpinLock<()> = RawSpinLock::new(());

#[derive(Clone, Copy)]
pub struct AcpiRoot {
    pub rsdp: usize,
    pub phys_to_virt: fn(usize) -> *mut u8,
    /// Retained firmware backing used to validate variable-length SPCR tables.
    pub firmware_memory: &'static [MemoryDescriptor],
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
        Self {
            rsdp,
            phys_to_virt,
            firmware_memory: &[],
        }
    }

    /// Supplies immutable, permanently mapped firmware reservations.
    /// SPCR entries outside these reservations are reported as invalid.
    pub const fn with_firmware_memory(mut self, memory: &'static [MemoryDescriptor]) -> Self {
        self.firmware_memory = memory;
        self
    }

    pub const fn identity(rsdp: usize) -> Self {
        Self::new(rsdp, identity_phys_to_virt)
    }

    fn spcr_memory_contains(&self, address: usize, length: usize) -> bool {
        self.firmware_memory.iter().any(|region| {
            region.memory_type == MemoryType::Reserved
                && region
                    .physical_start
                    .checked_add(region.size_in_bytes)
                    .is_some()
                && address
                    .checked_sub(region.physical_start)
                    .and_then(|offset| offset.checked_add(length))
                    .is_some_and(|end| end <= region.size_in_bytes)
        })
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
    /// One layout shared by AML accesses and the PCI controller driver.
    pub layout: ConfigSpaceLayout,
    /// Firmware-declared cache coherency for the matching ACPI PCI root.
    /// `None` means that `_CCA` was not present.
    pub dma_coherent: Option<bool>,
}

impl AcpiPciEcam {
    pub fn size(&self) -> usize {
        self.layout
            .window_size(self.bus_end.saturating_sub(self.bus_start))
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
    fn gsi_source(self) -> AcpiGsiSource {
        AcpiGsiSource {
            controller: AcpiGsiController::IoApic,
            controller_id: u16::from(self.id),
            controller_address: u64::from(self.address),
            gsi_base: self.gsi_base,
            gsi_count: u16::from(self.redirection_entries),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiPchPic {
    pub id: u16,
    pub address: u64,
    pub mmio_size: u16,
    pub gsi_count: u16,
    pub gsi_base: u32,
}

impl AcpiPchPic {
    fn gsi_source(self) -> AcpiGsiSource {
        AcpiGsiSource {
            controller: AcpiGsiController::PchPic,
            controller_id: self.id,
            controller_address: self.address,
            gsi_base: self.gsi_base,
            gsi_count: self.gsi_count,
        }
    }
}

/// Firmware LIOPIC wiring. Only version 1 and one instance are supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiLioPic {
    pub version: u8,
    pub address: u64,
    pub mmio_size: u16,
    pub cascade: [u8; 2],
    /// Input bitmaps for the corresponding CPU cascade lines.
    pub cascade_map: [u32; 2],
}

impl AcpiLioPic {
    fn route(self, gsi: u32) -> Option<AcpiGsiRoute> {
        let input = gsi.checked_sub(LOONGARCH_LIO_GSI_BASE)?;
        if input >= LOONGARCH_LIO_INPUTS {
            return None;
        }
        let parent = self
            .cascade_map
            .iter()
            .position(|map| map & (1u32 << input) != 0)?;
        Some(AcpiGsiRoute {
            gsi,
            vector: usize::from(self.cascade[parent]),
            controller: AcpiGsiController::LioPic,
            controller_id: 0,
            controller_address: self.address,
            controller_input: input as u8,
            trigger: AcpiIrqTrigger::Level,
            polarity: AcpiIrqPolarity::ActiveHigh,
        })
    }
}

/// Firmware EIOPIC metadata, retained for CPU cascade conflict checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiEioPic {
    pub version: u8,
    pub cascade: u8,
    pub node: u8,
    pub node_map: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AcpiGsiSource {
    controller: AcpiGsiController,
    controller_id: u16,
    controller_address: u64,
    gsi_base: u32,
    gsi_count: u16,
}

impl AcpiGsiSource {
    fn contains_gsi(self, gsi: u32) -> bool {
        let start = self.gsi_base;
        let end = start.saturating_add(u32::from(self.gsi_count));
        (start..end).contains(&gsi)
    }

    fn route(
        self,
        gsi: u32,
        trigger: AcpiIrqTrigger,
        polarity: AcpiIrqPolarity,
    ) -> Option<AcpiGsiRoute> {
        let controller_input = u8::try_from(gsi.checked_sub(self.gsi_base)?).ok()?;
        Some(AcpiGsiRoute {
            gsi,
            vector: PCI_INTX_VECTOR_BASE + gsi as usize,
            controller: self.controller,
            controller_id: self.controller_id,
            controller_address: self.controller_address,
            controller_input,
            trigger,
            polarity,
        })
    }
}

#[derive(Debug, Clone)]
pub struct AcpiRouting {
    io_apics: Vec<AcpiIoApic>,
    pch_pics: Vec<AcpiPchPic>,
    lio_pics: Vec<AcpiLioPic>,
    eio_pics: Vec<AcpiEioPic>,
    isa_overrides: Vec<AcpiIsaIrqOverride>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiIsaIrqOverride {
    pub source: u8,
    pub gsi: u32,
    pub trigger: AcpiIrqTrigger,
    pub polarity: AcpiIrqPolarity,
}

impl AcpiRouting {
    pub const fn new() -> Self {
        Self {
            io_apics: Vec::new(),
            pch_pics: Vec::new(),
            lio_pics: Vec::new(),
            eio_pics: Vec::new(),
            isa_overrides: Vec::new(),
        }
    }

    pub fn add_io_apic(&mut self, io_apic: AcpiIoApic) {
        self.io_apics.push(io_apic);
    }

    pub fn io_apics(&self) -> &[AcpiIoApic] {
        &self.io_apics
    }

    pub fn add_pch_pic(&mut self, pch_pic: AcpiPchPic) {
        self.pch_pics.push(pch_pic);
    }

    pub fn pch_pics(&self) -> &[AcpiPchPic] {
        &self.pch_pics
    }

    pub fn lio_pics(&self) -> &[AcpiLioPic] {
        &self.lio_pics
    }

    pub fn eio_pics(&self) -> &[AcpiEioPic] {
        &self.eio_pics
    }

    pub fn add_isa_irq_override(&mut self, irq_override: AcpiIsaIrqOverride) {
        self.isa_overrides.push(irq_override);
    }

    pub fn resolve_gsi(&self, gsi: u32) -> Option<AcpiGsiRoute> {
        if let Some(lio) = self.lio_pics.first()
            && (LOONGARCH_LIO_GSI_BASE..LOONGARCH_LIO_GSI_BASE + LOONGARCH_LIO_INPUTS)
                .contains(&gsi)
        {
            let route = lio.route(gsi);
            if route.is_none() {
                warn!(
                    "ACPI LIOPIC at {:#x} does not route GSI {gsi} (input {}, cascade maps {:?})",
                    lio.address,
                    gsi - LOONGARCH_LIO_GSI_BASE,
                    lio.cascade_map
                );
            }
            return route;
        }
        self.gsi_sources()
            .find(|source| source.contains_gsi(gsi))?
            .route(gsi, self.default_trigger(gsi), self.default_polarity(gsi))
    }

    /// Resolves a legacy ISA IRQ through any MADT Interrupt Source Override.
    pub fn resolve_isa_irq(&self, irq: u8) -> Option<AcpiGsiRoute> {
        let gsi = self
            .isa_overrides
            .iter()
            .find(|irq_override| irq_override.source == irq)
            .map_or(u32::from(irq), |irq_override| irq_override.gsi);
        self.resolve_gsi(gsi)
    }

    fn gsi_sources(&self) -> impl Iterator<Item = AcpiGsiSource> + '_ {
        self.io_apics
            .iter()
            .copied()
            .map(AcpiIoApic::gsi_source)
            .chain(self.pch_pics.iter().copied().map(AcpiPchPic::gsi_source))
    }

    pub fn resolve_vector(&self, vector: usize) -> Option<AcpiGsiRoute> {
        // LIO CPU parent lines cannot identify one device input.
        let gsi = u32::try_from(vector.checked_sub(PCI_INTX_VECTOR_BASE)?).ok()?;
        self.gsi_sources()
            .find(|source| source.contains_gsi(gsi))?
            .route(gsi, self.default_trigger(gsi), self.default_polarity(gsi))
    }

    fn default_trigger(&self, gsi: u32) -> AcpiIrqTrigger {
        self.isa_overrides
            .iter()
            .find(|irq_override| irq_override.gsi == gsi)
            .map(|irq_override| irq_override.trigger)
            .unwrap_or(if gsi < 16 {
                AcpiIrqTrigger::Edge
            } else {
                AcpiIrqTrigger::Level
            })
    }

    fn default_polarity(&self, gsi: u32) -> AcpiIrqPolarity {
        self.isa_overrides
            .iter()
            .find(|irq_override| irq_override.gsi == gsi)
            .map(|irq_override| irq_override.polarity)
            .unwrap_or(if gsi < 16 {
                AcpiIrqPolarity::ActiveHigh
            } else {
                AcpiIrqPolarity::ActiveLow
            })
    }
}

impl Default for AcpiRouting {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiPciIrqRoute {
    pub address: PciAddress,
    pub interrupt_pin: u8,
    pub intx_route: PciIntxRoute,
    pub gsi: AcpiGsiRoute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiResourceRange {
    pub base: u64,
    pub size: u64,
}

/// Register model selected by the host SPCR table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiSerialInterface {
    Uart16550,
    Pl011,
}

/// Address-space kind selected by the host SPCR table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiSerialAddressSpace {
    Memory,
    Io,
}

/// Owned, parser-independent host serial-console description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpiSerialConsole {
    pub interface: AcpiSerialInterface,
    pub address_space: AcpiSerialAddressSpace,
    pub registers: AcpiResourceRange,
    pub access_size: u8,
    pub irq: Option<u32>,
    pub baud_rate: Option<u32>,
    pub clock_hz: Option<u32>,
    pub namespace_path: Option<String>,
}

/// Registers and input clock for a 16550 UART selected by SPCR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spcr16550 {
    Mmio(Spcr16550Mmio),
    Io {
        /// The part of the device's I/O resource starting at the SPCR port.
        registers: AcpiResourceRange,
        clock_hz: Option<u32>,
    },
}

/// Register layout and input clock for a 16550 MMIO UART selected by SPCR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spcr16550Mmio {
    /// The part of the device's MMIO resource starting at the SPCR base.
    pub registers: AcpiResourceRange,
    /// Byte distance between adjacent registers.
    pub register_stride: usize,
    /// Width in bytes of each MMIO transaction.
    pub access_width: usize,
    pub clock_hz: Option<u32>,
    pub baud_rate: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpiResourceDevice {
    pub path: String,
    pub hid: Option<String>,
    pub cids: Vec<String>,
    pub memory_ranges: Vec<AcpiResourceRange>,
    pub io_ranges: Vec<AcpiResourceRange>,
    pub irq_routes: Vec<AcpiGsiRoute>,
    /// Firmware-declared DMA coherency after ACPI `_CCA` inheritance.
    pub dma_coherent: Option<bool>,
}

#[derive(Debug, Clone)]
struct AcpiDeviceInfo {
    path: String,
    hid: Option<String>,
    cids: Vec<String>,
    memory_ranges: Vec<AcpiResourceRange>,
    io_ranges: Vec<AcpiResourceRange>,
    irq_routes: Vec<AcpiGsiRoute>,
    dma_coherent: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AcpiResourceAddressSpace {
    Memory,
    Io,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AcpiResourceAddress {
    pub space: AcpiResourceAddressSpace,
    pub base: u64,
}

impl AcpiResourceAddress {
    pub const fn memory(base: u64) -> Self {
        Self {
            space: AcpiResourceAddressSpace::Memory,
            base,
        }
    }

    pub const fn io(base: u64) -> Self {
        Self {
            space: AcpiResourceAddressSpace::Io,
            base,
        }
    }

    pub fn from_generic_address(address: GenericAddress) -> Option<Self> {
        match address.address_space {
            AddressSpace::SystemMemory => Some(Self::memory(address.address)),
            AddressSpace::SystemIo => Some(Self::io(address.address)),
            _ => None,
        }
    }
}

pub struct AcpiInfo<'a> {
    pub root: &'a System,
    pub path: &'a str,
    pub irq_route: Option<AcpiGsiRoute>,
    device: Option<&'a AcpiDeviceInfo>,
}

impl AcpiInfo<'_> {
    pub const fn irq_route(&self) -> Option<AcpiGsiRoute> {
        self.irq_route
    }

    pub fn hid(&self) -> Option<&str> {
        self.device
            .as_ref()
            .and_then(|device| device.hid.as_deref())
    }

    pub fn cids(&self) -> &[String] {
        self.device
            .as_ref()
            .map(|device| device.cids.as_slice())
            .unwrap_or_default()
    }

    pub fn memory_ranges(&self) -> &[AcpiResourceRange] {
        self.device
            .as_ref()
            .map(|device| device.memory_ranges.as_slice())
            .unwrap_or_default()
    }

    /// Returns the SPCR settings for this device's 16550 console, if any.
    ///
    /// Returns `None` for an independent UART. An invalid applicable SPCR
    /// returns a probe error.
    pub fn spcr_16550(&self) -> Result<Option<Spcr16550>, OnProbeError> {
        self.root.spcr_16550(self)
    }

    /// Returns the requested console window, starting at the explicit base.
    /// Returns `None` when the request addresses a different device.
    pub fn preferred_serial_registers(
        &self,
    ) -> Option<(AcpiResourceAddressSpace, AcpiResourceRange)> {
        let address = self.root.serial_console_override()?;
        let ranges = match address.space {
            AcpiResourceAddressSpace::Io => self.io_ranges(),
            AcpiResourceAddressSpace::Memory => self.memory_ranges(),
        };
        let registers = register_range_at(ranges, address.base)?;
        Some((address.space, registers))
    }

    pub fn io_ranges(&self) -> &[AcpiResourceRange] {
        self.device
            .as_ref()
            .map(|device| device.io_ranges.as_slice())
            .unwrap_or_default()
    }

    pub fn irq_routes(&self) -> &[AcpiGsiRoute] {
        self.device
            .as_ref()
            .map(|device| device.irq_routes.as_slice())
            .unwrap_or_default()
    }

    /// Returns the inherited ACPI `_CCA` value for this device.
    ///
    /// `None` means no ACPI device in this device's ancestor chain declared
    /// `_CCA`; the consuming architecture decides whether that is supported.
    pub fn dma_coherent(&self) -> Option<bool> {
        self.device.and_then(|device| device.dma_coherent)
    }
}

impl From<AcpiDeviceInfo> for AcpiResourceDevice {
    fn from(value: AcpiDeviceInfo) -> Self {
        Self {
            path: value.path,
            hid: value.hid,
            cids: value.cids,
            memory_ranges: value.memory_ranges,
            io_ranges: value.io_ranges,
            irq_routes: value.irq_routes,
            dma_coherent: value.dma_coherent,
        }
    }
}

pub struct ProbeAcpi<'a> {
    info: AcpiInfo<'a>,
    platform: PlatformDevice,
    registered_address: &'a Cell<Option<AcpiResourceAddress>>,
}

impl<'a> ProbeAcpi<'a> {
    fn new(
        info: AcpiInfo<'a>,
        platform: PlatformDevice,
        registered_address: &'a Cell<Option<AcpiResourceAddress>>,
    ) -> Self {
        Self {
            info,
            platform,
            registered_address,
        }
    }

    pub const fn info(&self) -> &AcpiInfo<'a> {
        &self.info
    }

    /// Returns this probe's existing device identity without registering it.
    ///
    /// Root providers can reserve resources for this owner before consuming
    /// the probe with `register_root_resource_device`.
    pub fn device_id(&self) -> DeviceId {
        self.platform.descriptor().device_id()
    }

    pub fn into_platform_device(self) -> PlatformDevice {
        self.platform
    }

    pub fn into_parts(self) -> (AcpiInfo<'a>, PlatformDevice) {
        (self.info, self.platform)
    }

    /// Registers a device discovered by an ACPI root callback and associates
    /// its register address with the resulting device identity.
    pub fn register_root_resource_device<T: DriverGeneric>(
        self,
        address: AcpiResourceAddress,
        driver: T,
    ) {
        debug_assert!(self.info.device.is_none());
        self.register_device_at(address, driver);
    }

    /// Registers the device and records its effective register base for ACPI lookup.
    ///
    /// A namespace driver may select a register block within a declared resource.
    pub fn register_device_at<T: DriverGeneric>(self, address: AcpiResourceAddress, driver: T) {
        self.platform.register(driver);
        self.registered_address.set(Some(address));
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
    init_system(system)
}

pub fn init_without_aml(root: AcpiRoot) -> Result<(), DriverError> {
    let system = System::new_without_aml(root)?;
    init_system(system)
}

fn init_system(system: System) -> Result<(), DriverError> {
    info!(
        "ACPI initialized: {} PCI ECAM region(s), {} IOAPIC(s), {} PCH-PIC(s)",
        system.pci_ecam_regions().len(),
        system.routing().io_apics().len(),
        system.routing().pch_pics().len()
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

struct SpcrConsoleSelection {
    namespace: Option<String>,
    address: AcpiResourceAddress,
}

impl SpcrConsoleSelection {
    fn matches_device(&self, device: &AcpiDeviceInfo) -> bool {
        if self
            .namespace
            .as_ref()
            .is_some_and(|path| *path != device.path)
        {
            return false;
        }
        let ranges = match self.address.space {
            AcpiResourceAddressSpace::Memory => &device.memory_ranges,
            AcpiResourceAddressSpace::Io => &device.io_ranges,
        };
        ranges.iter().any(|range| {
            self.address
                .base
                .checked_sub(range.base)
                .is_some_and(|offset| offset < range.size)
        })
    }
}

/// The firmware selected a serial console that could not be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpcrConsoleNotFound;

/// Resolves the SPCR-selected console. Returns `Ok(None)` if SPCR provides
/// no supported, enabled serial console.
pub fn spcr_console_device_id() -> Option<DeviceId> {
    spcr_console_device_id_result().ok().flatten()
}

pub fn spcr_console_device_id_result() -> Result<Option<DeviceId>, SpcrConsoleNotFound> {
    match try_system() {
        Some(system) => system.spcr_console_device_id_result(),
        None => Ok(None),
    }
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

enum PciRegisterAccess {
    Mapped(NonNull<u8>),
    Legacy,
    Unavailable,
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

    fn pci_register(&self, address: acpi::PciAddress, offset: u16) -> PciRegisterAccess {
        if offset >= 4096 {
            return PciRegisterAccess::Unavailable;
        }

        let bus = address.bus();
        let Some(region) = self.pci_ecam_regions.iter().find(|region| {
            address.segment() == region.segment_group
                && bus >= region.bus_start
                && bus <= region.bus_end
        }) else {
            return PciRegisterAccess::Legacy;
        };
        let local_address = PciAddress::new(
            address.segment(),
            bus - region.bus_start,
            address.device(),
            address.function(),
        );
        let physical_address = region
            .layout
            .register_offset(local_address, offset)
            .and_then(|offset| {
                usize::try_from(region.base_address)
                    .ok()?
                    .checked_add(offset)
            });
        let Some(physical_address) = physical_address else {
            return PciRegisterAccess::Unavailable;
        };
        NonNull::new((self.root.phys_to_virt)(physical_address))
            .map_or(PciRegisterAccess::Unavailable, PciRegisterAccess::Mapped)
    }
}

impl Handler for AcpiHandler {
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> RawPhysicalMapping<T> {
        #[cfg(test)]
        if let Some(region) = self.root.firmware_memory.iter().find(|region| {
            physical_address
                .checked_sub(region.physical_start)
                .is_some_and(|offset| offset < region.size_in_bytes)
        }) {
            assert!(
                physical_address
                    .checked_sub(region.physical_start)
                    .and_then(|offset| offset.checked_add(size))
                    .is_some_and(|end| end <= region.size_in_bytes),
                "ACPI mapping exceeds fixture backing"
            );
        }
        let virtual_start = NonNull::new(self.virt_addr(physical_address) as *mut T)
            .expect("ACPI physical mapping must not be null");
        // SAFETY: The caller supplies a valid T and its complete extent. The
        // root reserves permanent firmware backing; this descriptor transfers
        // the borrowed mapping to ACPI without allocating or freeing memory.
        unsafe { RawPhysicalMapping::new(physical_address, virtual_start, size, size) }
    }

    unsafe fn unmap_physical_region<T>(&self, _region: RawPhysicalMapping<T>) {}

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
        match self.pci_register(address, offset) {
            PciRegisterAccess::Mapped(ptr) => {
                // SAFETY: The region owns a permanent firmware mapping, and
                // pci_register checked the byte's layout and address range.
                unsafe { ptr.as_ptr().read_volatile() }
            }
            PciRegisterAccess::Legacy => pci_legacy_read_u8(address, offset).unwrap_or(u8::MAX),
            PciRegisterAccess::Unavailable => u8::MAX,
        }
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
        match self.pci_register(address, offset) {
            PciRegisterAccess::Mapped(ptr) => {
                // SAFETY: The permanent firmware mapping contains this byte;
                // no Rust reference to the device register is constructed.
                unsafe { ptr.as_ptr().write_volatile(value) };
            }
            PciRegisterAccess::Legacy => pci_legacy_write_u8(address, offset, value),
            PciRegisterAccess::Unavailable => {}
        }
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

    fn handle_fatal_error(&self, fatal_type: u8, fatal_code: u32, fatal_arg: u64) {
        panic!(
            "Fatal AML error: type={fatal_type:#x}, code={fatal_code:#x}, argument={fatal_arg:#x}"
        );
    }
}

fn load_aml_interpreter(
    platform: &AcpiPlatform<AcpiHandler>,
) -> Result<Interpreter<AcpiHandler>, DriverError> {
    let dsdt = platform.tables.dsdt().map_err(acpi_error)?;
    let facs = platform
        .tables
        .find_table::<Fadt>()
        .and_then(|fadt| fadt.facs_address().ok())
        .map(|address| {
            // SAFETY: The platform's FADT identifies firmware-owned FACS memory.
            // AcpiHandler uses a permanent mapping, retained by the interpreter.
            unsafe { PhysicalMapping::new(address, size_of::<Facs>(), platform.handler.clone()) }
        });
    let interpreter = Interpreter::new(
        platform.handler.clone(),
        dsdt.revision,
        platform.registers.clone(),
        facs,
    );
    // SAFETY: These descriptors originate from this platform's firmware tables.
    // The ACPI root reserves their immutable backing for the whole load.
    unsafe {
        load_aml_tables(
            &interpreter,
            &platform.handler,
            core::iter::once(dsdt).chain(platform.tables.ssdts()),
        )?;
    }
    Ok(interpreter)
}

/// Loads firmware tables in order, stopping at the first malformed table.
///
/// # Safety
///
/// For an accepted range, each table must describe initialized, immutable firmware
/// memory accessible through `handler` until this call returns. This is a backing
/// and mapping precondition, not a claim that the table checksum was validated.
unsafe fn load_aml_tables(
    interpreter: &Interpreter<AcpiHandler>,
    handler: &AcpiHandler,
    tables: impl IntoIterator<Item = AmlTable>,
) -> Result<(), DriverError> {
    let header_length = size_of::<SdtHeader>();
    for table in tables {
        let length = table.length as usize;
        if length < header_length
            || length > isize::MAX as usize
            || table.phys_address.checked_add(length).is_none()
        {
            return Err(DriverError::Unsupported("invalid AML table range"));
        }
        // SAFETY: The caller guarantees live immutable firmware backing.
        // The checked extent includes the header and fits a Rust slice.
        let mapping = unsafe { PhysicalMapping::<_, u8>::new(table.phys_address, length, handler) };
        // SAFETY: The mapping covers `length` initialized bytes; byte alignment is
        // sufficient. The mapping remains live until load_table returns.
        let bytes = unsafe { core::slice::from_raw_parts(mapping.as_ptr(), length) };
        interpreter
            .load_table(&bytes[header_length..])
            .map_err(|error| acpi_error(AcpiError::Aml(error)))?;
    }
    Ok(())
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
    interpreter: Option<Interpreter<AcpiHandler>>,
    handler: AcpiHandler,
    pci: Option<AcpiPciNamespace>,
    probed_names: RawSpinLock<BTreeSet<&'static str>>,
    serial_console_override: OnceLock<AcpiResourceAddress>,
    populated_paths: RawSpinLock<BTreeMap<String, DeviceId>>,
    populated_resources: RawSpinLock<BTreeMap<AcpiResourceAddress, DeviceId>>,
}

unsafe impl Send for System {}
unsafe impl Sync for System {}

struct AcpiPciNamespace {
    link_allocator: RawSpinLock<PciLinkAllocator>,
    roots: Vec<AcpiPciRoot>,
}

struct AcpiPciRoot {
    segment: u16,
    bus: u8,
    path: String,
    dma_coherent: Option<bool>,
    prt: Option<PciRoutingTable>,
    link_prt: Option<PciLinkRoutingTable>,
}

impl System {
    pub fn new(root: AcpiRoot) -> Result<Self, DriverError> {
        Self::new_with_options(root, true)
    }

    pub fn new_without_aml(root: AcpiRoot) -> Result<Self, DriverError> {
        Self::new_with_options(root, false)
    }

    fn new_with_options(root: AcpiRoot, load_aml: bool) -> Result<Self, DriverError> {
        let handler = root.handler();
        let tables =
            unsafe { AcpiTables::from_rsdp(handler.clone(), root.rsdp) }.map_err(acpi_error)?;
        let mut ecam_regions = read_pci_ecam_regions(&tables)?;
        let routing = read_interrupt_routing(&tables)?;
        let namespace_handler = root.handler_with_pci_ecam(ecam_regions.clone());
        let (interpreter, pci) = if load_aml {
            let platform =
                AcpiPlatform::new(tables, namespace_handler.clone()).map_err(acpi_error)?;
            let interpreter = load_aml_interpreter(&platform)?;
            interpreter.initialize_namespace();
            let pci = match read_pci_namespace(&interpreter) {
                Ok(pci) => Some(pci),
                Err(err) => {
                    warn!("failed to discover ACPI PCI namespace: {err:?}");
                    None
                }
            };
            if let Some(pci) = &pci {
                apply_pci_root_dma_coherency(&mut ecam_regions, pci)?;
            }
            (Some(interpreter), pci)
        } else {
            (None, None)
        };

        Ok(Self {
            ecam_regions,
            routing,
            interpreter,
            handler: namespace_handler,
            pci,
            probed_names: RawSpinLock::new(BTreeSet::new()),
            serial_console_override: OnceLock::new(),
            populated_paths: RawSpinLock::new(BTreeMap::new()),
            populated_resources: RawSpinLock::new(BTreeMap::new()),
        })
    }

    pub fn pci_ecam_regions(&self) -> &[AcpiPciEcam] {
        &self.ecam_regions
    }

    pub fn routing(&self) -> &AcpiRouting {
        &self.routing
    }

    pub fn path_to_device_id(&self, path: &str) -> Option<DeviceId> {
        self.populated_paths.lock().get(path).copied()
    }

    pub fn resource_address_to_device_id(&self, address: AcpiResourceAddress) -> Option<DeviceId> {
        self.populated_resources.lock().get(&address).copied()
    }

    /// Records the platform's explicit serial address before driver probing.
    /// The first selection is retained for the life of the system.
    pub fn prefer_serial_console(&self, address: AcpiResourceAddress) {
        self.serial_console_override.call_once(|| address);
    }

    /// The address explicitly selected by the platform, if supplied.
    pub fn serial_console_override(&self) -> Option<AcpiResourceAddress> {
        self.serial_console_override.get().copied()
    }

    /// Resolves the SPCR-selected console in this system's registered devices.
    pub fn spcr_console_device_id(&self) -> Option<DeviceId> {
        self.spcr_console_device_id_result().ok().flatten()
    }

    pub fn spcr_console_device_id_result(&self) -> Result<Option<DeviceId>, SpcrConsoleNotFound> {
        self.spcr_console_selection()?
            .map(|selection| {
                let id = self
                    .resource_address_to_device_id(selection.address)
                    .ok_or(SpcrConsoleNotFound)?;
                if let Some(path) = selection.namespace
                    && self.path_to_device_id(&path) != Some(id)
                {
                    return Err(SpcrConsoleNotFound);
                }
                Ok(id)
            })
            .transpose()
    }

    fn spcr_console_selection(&self) -> Result<Option<SpcrConsoleSelection>, SpcrConsoleNotFound> {
        // SAFETY: the ACPI root and mapping handler remain valid for this System.
        let tables = unsafe { AcpiTables::from_rsdp(self.handler.clone(), self.handler.root.rsdp) }
            .map_err(|_| SpcrConsoleNotFound)?;
        // SAFETY: the directory retains valid headers. The root validates each
        // complete extent against its immutable, permanently mapped reservations.
        let mut mappings = unsafe {
            spcr_mappings(&tables, self.handler.clone(), |address, length| {
                self.handler.root.spcr_memory_contains(address, length)
            })
        };
        mappings
            .find_map(|mapping| {
                let Some(spcr) = mapping.parse() else {
                    return Some(Err(SpcrConsoleNotFound));
                };
                if !is_supported_spcr_interface(spcr.interface_type()) {
                    return None;
                }
                let gas = spcr.base_address()?;
                let selection = || {
                    let gas = gas.ok()?;
                    if gas.address_space == AddressSpace::SystemIo
                        && matches!(
                            spcr.interface_type(),
                            SpcrInterfaceType::Full16550
                                | SpcrInterfaceType::Full16450
                                | SpcrInterfaceType::Generic16550
                        )
                    {
                        spcr.ns16550_io_port()?;
                    }
                    let address = AcpiResourceAddress::from_generic_address(gas)?;
                    let namespace = match spcr.namespace_path().ok()? {
                        Some(path) => Some(normalize_spcr_path(path)?),
                        None => None,
                    };
                    Some(SpcrConsoleSelection { namespace, address })
                };
                Some(selection().ok_or(SpcrConsoleNotFound))
            })
            .transpose()
    }

    /// Returns the SPCR 16550 settings that match an ACPI device.
    ///
    /// An SPCR namespace path identifies the device when present. Its resource
    /// must cover the GAS base. Without a path, match by resource coverage alone.
    /// An explicit address takes precedence only on devices whose resources
    /// cover it. At the same address, use the SPCR layout when valid.
    fn spcr_16550(&self, device: &AcpiInfo<'_>) -> Result<Option<Spcr16550>, OnProbeError> {
        let requested = device
            .preferred_serial_registers()
            .map(|(space, registers)| AcpiResourceAddress {
                space,
                base: registers.base,
            });
        // SAFETY: the ACPI root and physical mapping handler remain valid for
        // this System, as in the other SPCR queries.
        let tables = unsafe { AcpiTables::from_rsdp(self.handler.clone(), self.handler.root.rsdp) }
            .map_err(on_probe_error)?;
        // SAFETY: the root validates complete extents against retained firmware
        // reservations before this handler maps them.
        let mut mappings = unsafe {
            spcr_mappings(&tables, self.handler.clone(), |address, length| {
                self.handler.root.spcr_memory_contains(address, length)
            })
        };
        mappings
            .find_map(|mapping| {
                let spcr = mapping.parse()?;
                let gas = spcr.base_address()?.ok()?;
                match spcr.interface_type() {
                    SpcrInterfaceType::Full16550
                    | SpcrInterfaceType::Full16450
                    | SpcrInterfaceType::Generic16550 => {}
                    _ => return None,
                }
                if requested.is_some()
                    && AcpiResourceAddress::from_generic_address(gas) != requested
                {
                    return None;
                }
                let ranges = match gas.address_space {
                    AddressSpace::SystemMemory => device.memory_ranges(),
                    AddressSpace::SystemIo => device.io_ranges(),
                    _ => return None,
                };
                let namespace = if requested.is_none() {
                    match spcr.namespace_path() {
                        Ok(path) => path,
                        Err(_) => {
                            return register_range_at(ranges, gas.address).map(|_| {
                                Err(OnProbeError::other("invalid SPCR UART namespace path"))
                            });
                        }
                    }
                } else {
                    None
                };
                let registers = match spcr_uart_range(device.path, namespace, ranges, gas.address)?
                {
                    Ok(registers) => registers,
                    Err(error) => return Some(Err(error)),
                };
                let clock_hz = spcr.uart_clock_frequency();
                if gas.address_space == AddressSpace::SystemIo {
                    if spcr.ns16550_io_port().is_none() {
                        return requested.is_none().then_some(Err(OnProbeError::Unsupported(
                            "SPCR 16550 I/O port layout",
                        )));
                    }
                    return Some(Ok(Spcr16550::Io {
                        registers,
                        clock_hz,
                    }));
                }
                let Some((register_stride, access_width)) = spcr.ns16550_mmio_layout() else {
                    return requested.is_none().then_some(Err(OnProbeError::Unsupported(
                        "SPCR NS16550 MMIO register layout",
                    )));
                };
                Some(Ok(Spcr16550::Mmio(Spcr16550Mmio {
                    registers,
                    register_stride,
                    access_width,
                    clock_hz,
                    baud_rate: spcr.baud_rate(),
                })))
            })
            .transpose()
    }

    /// Returns whether a fixed serial resource can be probed as a fallback.
    ///
    /// An available namespace device matching any of `ids` suppresses the
    /// fallback even if its driver could not bind it. A console override must
    /// select `address`. Without an override, any SPCR selection must match
    /// the address and omit a namespace path. Firmware errors are returned.
    pub fn serial_fallback_allowed(
        &self,
        address: AcpiResourceAddress,
        ids: &[AcpiId],
    ) -> Result<bool, OnProbeError> {
        let selection_matches = match self.serial_console_override() {
            Some(requested) => requested == address,
            None => self
                .spcr_console_selection()
                .map_err(on_probe_error)?
                .is_none_or(|selection| {
                    selection.address == address && selection.namespace.is_none()
                }),
        };
        if !selection_matches || self.resource_address_to_device_id(address).is_some() {
            return Ok(false);
        }

        Ok(self
            .device_infos_for_ids(ids)
            .map_err(on_probe_error)?
            .is_empty())
    }

    pub fn resource_devices(&self) -> Result<Vec<AcpiResourceDevice>, ProbeError> {
        self.device_infos().map(|devices| {
            devices
                .into_iter()
                .map(AcpiResourceDevice::from)
                .collect::<Vec<_>>()
        })
    }

    /// Returns the SPCR-selected serial console as an owned descriptor.
    pub fn serial_console(&self) -> Result<Option<AcpiSerialConsole>, DriverError> {
        Ok(self
            .serial_console_with_register_shift()?
            .map(|(serial, _)| serial))
    }

    /// Returns the SPCR console and its 16550 MMIO register shift.
    ///
    /// Port I/O and non-16550 consoles have a zero shift. The descriptor's
    /// range and access size describe the same effective register layout.
    pub fn serial_console_with_register_shift(
        &self,
    ) -> Result<Option<(AcpiSerialConsole, u8)>, DriverError> {
        let tables = unsafe { AcpiTables::from_rsdp(self.handler.clone(), self.handler.root.rsdp) }
            .map_err(acpi_error)?;
        // SAFETY: the root validates complete extents against retained firmware
        // reservations before this handler maps them.
        let Some(mapping) = (unsafe {
            spcr_mappings(&tables, self.handler.clone(), |address, length| {
                self.handler.root.spcr_memory_contains(address, length)
            })
        })
        .next() else {
            return Ok(None);
        };
        let spcr = mapping
            .parse()
            .ok_or_else(|| DriverError::Unknown("invalid host SPCR table length".into()))?;
        let interface = match spcr.interface_type() {
            SpcrInterfaceType::Full16550
            | SpcrInterfaceType::Full16450
            | SpcrInterfaceType::Generic16550 => AcpiSerialInterface::Uart16550,
            SpcrInterfaceType::ArmPL011 => AcpiSerialInterface::Pl011,
            _ => return Err(DriverError::Unsupported("host SPCR serial interface")),
        };
        let Some(address) = spcr.base_address() else {
            return Ok(None);
        };
        let address = address.map_err(acpi_error)?;
        let address_space = match address.address_space {
            AddressSpace::SystemMemory => AcpiSerialAddressSpace::Memory,
            AddressSpace::SystemIo => AcpiSerialAddressSpace::Io,
            _ => return Err(DriverError::Unsupported("host SPCR serial address space")),
        };
        let namespace_path = spcr
            .namespace_path()
            .map_err(|_| DriverError::Unknown("invalid host SPCR namespace".into()))?
            .map(|path| {
                normalize_spcr_path(path)
                    .ok_or_else(|| DriverError::Unknown(format!("invalid host SPCR path: {path}")))
            })
            .transpose()?;
        let (size, access_size, register_shift) = if address_space == AcpiSerialAddressSpace::Io
            && interface == AcpiSerialInterface::Uart16550
        {
            spcr.ns16550_io_port()
                .ok_or(DriverError::Unsupported("host SPCR 16550 I/O port layout"))?;
            (8, 1, 0)
        } else if address_space == AcpiSerialAddressSpace::Memory
            && interface == AcpiSerialInterface::Uart16550
        {
            let (stride, width) = spcr.ns16550_mmio_layout().ok_or(DriverError::Unsupported(
                "host SPCR 16550 MMIO register layout",
            ))?;
            (
                8 * stride as u64,
                1 + width.ilog2() as u8,
                stride.ilog2() as u8,
            )
        } else {
            (
                spcr_uart_register_size(address.access_size),
                address.access_size,
                0,
            )
        };
        let console = AcpiSerialConsole {
            interface,
            address_space,
            registers: AcpiResourceRange {
                base: address.address,
                size,
            },
            access_size,
            irq: spcr.irq(),
            baud_rate: spcr.baud_rate(),
            clock_hz: spcr.uart_clock_frequency(),
            namespace_path,
        };
        Ok(Some((console, register_shift)))
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
        let Some(gsi) = pci_irq_descriptor_gsi(&irq) else {
            return Err(OnProbeError::other(format!(
                "ACPI PCI endpoint {} pin {} returned an invalid IRQ descriptor: {:?}",
                info.address, intx_route.root_pin, irq
            )));
        };
        if gsi == 0 {
            return Ok(None);
        }

        let Some(route) = self.routing.resolve_gsi(gsi) else {
            return Err(OnProbeError::other(format!(
                "ACPI GSI {} for PCI endpoint {} is not covered by a registered GSI controller",
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

            if let Some(link_prt) = &root.link_prt
                && let Some(route) = link_prt
                    .route(
                        u16::from(route.root_device),
                        u16::from(route.root_function),
                        pin,
                        self.interpreter
                            .as_ref()
                            .expect("ACPI PCI routing requires an AML interpreter"),
                        &self.handler,
                        &mut pci.link_allocator.lock(),
                    )
                    .map_err(on_probe_error)?
            {
                return Ok(Some(route));
            }

            match prt.route(
                u16::from(route.root_device),
                u16::from(route.root_function),
                pin,
                self.interpreter
                    .as_ref()
                    .expect("ACPI PCI routing requires an AML interpreter"),
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

    fn device_infos_for_ids(&self, ids: &[AcpiId]) -> Result<Vec<AcpiDeviceInfo>, ProbeError> {
        let Some(interpreter) = &self.interpreter else {
            return Ok(Vec::new());
        };
        let mut devices = Vec::new();
        let mut device_paths = Vec::new();
        let mut namespace = interpreter.namespace.lock().clone();
        namespace
            .traverse(|path, level| {
                if level.kind != NamespaceLevelKind::Device {
                    return Ok(true);
                }
                device_paths.push(path.clone());
                let Some((hid, cids)) = acpi_device_ids(interpreter, path)? else {
                    return Ok(true);
                };
                if !acpi_ids_match(&hid, &cids, ids) || !acpi_device_is_available(interpreter, path)
                {
                    return Ok(true);
                }
                let resources = read_device_resources(interpreter, path, &self.routing)?;
                devices.push(AcpiDeviceInfo {
                    path: path.as_string(),
                    hid: Some(hid),
                    cids,
                    memory_ranges: resources.memory_ranges,
                    io_ranges: resources.io_ranges,
                    irq_routes: resources.irq_routes,
                    dma_coherent: inherited_device_cca(interpreter, path, &device_paths),
                });
                Ok(true)
            })
            .map_err(|err| ProbeError::OnProbe(OnProbeError::other(format!("{err:?}"))))?;
        Ok(devices)
    }

    fn device_infos(&self) -> Result<Vec<AcpiDeviceInfo>, ProbeError> {
        let Some(interpreter) = &self.interpreter else {
            return Ok(Vec::new());
        };
        let mut devices = Vec::new();
        let mut device_paths = Vec::new();
        let mut namespace = interpreter.namespace.lock().clone();
        namespace
            .traverse(|path, level| {
                if level.kind != NamespaceLevelKind::Device {
                    return Ok(true);
                }
                device_paths.push(path.clone());
                let Some((hid, cids)) = acpi_device_ids(interpreter, path)? else {
                    return Ok(true);
                };
                let resources = read_device_resources(interpreter, path, &self.routing)?;
                devices.push(AcpiDeviceInfo {
                    path: path.as_string(),
                    hid: Some(hid),
                    cids,
                    memory_ranges: resources.memory_ranges,
                    io_ranges: resources.io_ranges,
                    irq_routes: resources.irq_routes,
                    dma_coherent: inherited_device_cca(interpreter, path, &device_paths),
                });
                Ok(true)
            })
            .map_err(|err| ProbeError::OnProbe(OnProbeError::other(format!("{err:?}"))))?;
        Ok(devices)
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
            if self.probed_names.lock().contains(register.name) {
                continue;
            }

            if !ids.is_empty() && !is_root_acpi_id_list(ids) {
                let mut devices = self.device_infos_for_ids(ids)?;
                // This registration binds only one namespace device. Try the
                // selected console before independent devices of the same model.
                if devices.len() > 1
                    && let Some(selection) = self
                        .serial_console_override()
                        .map(|address| SpcrConsoleSelection {
                            namespace: None,
                            address,
                        })
                        .or_else(|| self.spcr_console_selection().ok().flatten())
                    && let Some(index) = devices
                        .iter()
                        .position(|device| selection.matches_device(device))
                {
                    devices[..=index].rotate_right(1);
                }
                for device in devices {
                    if self.probed_names.lock().contains(register.name) {
                        break;
                    }
                    let desc = crate::Descriptor {
                        name: register.name,
                        device_id: DeviceId::new(),
                        irq_parent: None,
                        fdt_node: None,
                    };
                    let device_id = desc.device_id();
                    let info = AcpiInfo {
                        root: self,
                        path: &device.path,
                        irq_route: device.irq_routes.first().copied(),
                        device: Some(&device),
                    };
                    let address = Cell::new(None);
                    let res = on_probe(ProbeAcpi::new(info, PlatformDevice::new(desc), &address));
                    if res.is_ok() {
                        self.probed_names.lock().insert(register.name);
                        self.note_populated_device(device_id, &device, address.get());
                    }
                    out.push(res);
                }
                continue;
            }

            let desc = crate::Descriptor {
                name: register.name,
                device_id: DeviceId::new(),
                irq_parent: None,
                fdt_node: None,
            };
            let info = AcpiInfo {
                root: self,
                path: "\\",
                irq_route: None,
                device: None,
            };
            let device_id = desc.device_id();
            let address = Cell::new(None);
            let res = on_probe(ProbeAcpi::new(info, PlatformDevice::new(desc), &address));
            if res.is_ok() {
                self.probed_names.lock().insert(register.name);
                if let Some(address) = address.get() {
                    self.note_populated_resource(device_id, address);
                }
            }
            out.push(res);
        }
        Ok(out)
    }

    fn note_populated_device(
        &self,
        device_id: DeviceId,
        device: &AcpiDeviceInfo,
        registered_address: Option<AcpiResourceAddress>,
    ) {
        self.populated_paths
            .lock()
            .insert(device.path.clone(), device_id);

        if let Some(address) = registered_address {
            self.note_populated_resource(device_id, address);
            return;
        }
        let mut resources = self.populated_resources.lock();
        for range in &device.memory_ranges {
            resources.insert(AcpiResourceAddress::memory(range.base), device_id);
        }
        for range in &device.io_ranges {
            resources.insert(AcpiResourceAddress::io(range.base), device_id);
        }
    }

    fn note_populated_resource(&self, device_id: DeviceId, address: AcpiResourceAddress) {
        self.populated_resources.lock().insert(address, device_id);
    }
}

fn spcr_uart_range(
    device: &str,
    spcr_device: Option<&str>,
    ranges: &[AcpiResourceRange],
    base: u64,
) -> Option<Result<AcpiResourceRange, OnProbeError>> {
    let registers = register_range_at(ranges, base);
    if let Some(path) = spcr_device {
        match normalize_spcr_path(path) {
            Some(path) if path == device => {}
            // A second namespace node covering the same GAS base is not an
            // independent UART and must not bind the selected registers.
            Some(_) => return registers.map(|_| Err(OnProbeError::NotMatch)),
            None => {
                return registers
                    .map(|_| Err(OnProbeError::other("invalid SPCR UART namespace path")));
            }
        }
        return Some(registers.ok_or_else(|| {
            OnProbeError::other(format!(
                "SPCR UART {path} has no resource covering GAS base {base:#x}"
            ))
        }));
    }
    registers.map(Ok)
}

fn register_range_at(ranges: &[AcpiResourceRange], base: u64) -> Option<AcpiResourceRange> {
    ranges.iter().find_map(|range| {
        let offset = base.checked_sub(range.base)?;
        let size = range.size.checked_sub(offset)?;
        (size > 0).then_some(AcpiResourceRange { base, size })
    })
}

fn is_supported_spcr_interface(interface: SpcrInterfaceType) -> bool {
    matches!(
        interface,
        SpcrInterfaceType::Full16550
            | SpcrInterfaceType::Full16450
            | SpcrInterfaceType::Generic16550
            | SpcrInterfaceType::ArmPL011
            | SpcrInterfaceType::ArmSBSAGeneric32bit
            | SpcrInterfaceType::ArmSBSAGeneric
    )
}

fn normalize_spcr_path(path: &str) -> Option<String> {
    let relative = path.strip_prefix('\\')?;
    // Validate each NameSeg before parsing. In particular the AML parser
    // assumes four available bytes in its invalid NameSeg error path.
    for segment in relative.split('.') {
        let mut bytes = segment.bytes();
        let first = bytes.next()?;
        if segment.len() > 4
            || !(first == b'_' || first.is_ascii_uppercase())
            || !bytes.all(|byte| byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return None;
        }
    }
    Some(AmlName::from_str(path).ok()?.as_string())
}

fn read_pci_ecam_regions(
    tables: &AcpiTables<AcpiHandler>,
) -> Result<Vec<AcpiPciEcam>, DriverError> {
    let mcfg = tables
        .find_table::<Mcfg>()
        .ok_or_else(|| acpi_error(AcpiError::TableNotFound(Signature::MCFG)))?;
    let header = mcfg.header;
    let oem_revision = header.oem_revision;
    info!(
        "ACPI MCFG: OEM {:?}/{:?}, revision {oem_revision:#x}",
        header.oem_id, header.oem_table_id,
    );
    Ok(mcfg
        .entries()
        .iter()
        .map(|region| AcpiPciEcam {
            segment_group: region.pci_segment_group,
            bus_start: region.bus_number_start,
            bus_end: region.bus_number_end,
            base_address: region.base_address,
            layout: if mcfg.header.oem_id == *b"LOONGS"
                && (mcfg.header.oem_table_id == *b"LOONGSON" || mcfg.header.oem_table_id == [0; 8])
                && mcfg.header.oem_revision == 1
                && region.pci_segment_group < 8
            {
                ConfigSpaceLayout::Loongson
            } else {
                ConfigSpaceLayout::Ecam
            },
            dma_coherent: None,
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
        for irq_override in &apic.interrupt_source_overrides {
            routing.add_isa_irq_override(AcpiIsaIrqOverride {
                source: irq_override.isa_source,
                gsi: irq_override.global_system_interrupt,
                trigger: match irq_override.trigger_mode {
                    TriggerMode::Edge => AcpiIrqTrigger::Edge,
                    TriggerMode::Level => AcpiIrqTrigger::Level,
                    _ => AcpiIrqTrigger::Edge,
                },
                polarity: match irq_override.polarity {
                    Polarity::ActiveHigh => AcpiIrqPolarity::ActiveHigh,
                    Polarity::ActiveLow => AcpiIrqPolarity::ActiveLow,
                    _ => AcpiIrqPolarity::ActiveHigh,
                },
            });
        }
    }
    read_loongarch_interrupt_routing(tables, &mut routing)?;
    Ok(routing)
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RawMadtEntryHeader {
    entry_type: u8,
    length: u8,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RawMadtBioPic {
    header: RawMadtEntryHeader,
    version: u8,
    address: u64,
    size: u16,
    id: u16,
    gsi_base: u16,
}

// ACPI 6.5 MADT layouts, also defined in Linux 7.1 include/acpi/actbl2.h.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RawMadtLioPic {
    header: RawMadtEntryHeader,
    version: u8,
    address: u64,
    size: u16,
    cascade: [u8; 2],
    cascade_map: [u32; 2],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RawMadtEioPic {
    header: RawMadtEntryHeader,
    version: u8,
    cascade: u8,
    node: u8,
    node_map: u64,
}

const ACPI_MADT_TYPE_LIO_PIC: u8 = 18;
const ACPI_MADT_TYPE_EIO_PIC: u8 = 20;
const ACPI_MADT_TYPE_BIO_PIC: u8 = 22;
const RAW_MADT_HEADER_LEN: usize = core::mem::size_of::<acpi::sdt::SdtHeader>() + 8;

fn read_loongarch_interrupt_routing(
    tables: &AcpiTables<AcpiHandler>,
    routing: &mut AcpiRouting,
) -> Result<(), DriverError> {
    let Some(madt) = tables.find_table::<acpi::sdt::madt::Madt>() else {
        return Ok(());
    };

    // SAFETY: The MADT mapping owns its firmware backing for this entire scan.
    let base = unsafe { madt.as_ptr().cast::<u8>() };
    let length = madt.raw().region_length();
    if length > isize::MAX as usize || (base as usize).checked_add(length).is_none() {
        return Err(DriverError::Unsupported("invalid MADT mapping range"));
    }
    // SAFETY: The ACPI table mapping retains initialized firmware bytes for
    // `region_length`. The extent fits a slice and is borrowed only while the
    // immutable mapping remains live. Byte alignment is sufficient.
    let bytes = unsafe { core::slice::from_raw_parts(base, length) };
    parse_loongarch_interrupt_routing(bytes, routing)
}

fn parse_loongarch_interrupt_routing(
    bytes: &[u8],
    routing: &mut AcpiRouting,
) -> Result<(), DriverError> {
    if bytes.len() < RAW_MADT_HEADER_LEN {
        return Err(DriverError::Unsupported("short MADT header"));
    }
    let mut offset = RAW_MADT_HEADER_LEN;
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        if remaining.len() < size_of::<RawMadtEntryHeader>() {
            return Err(DriverError::Unknown(format!(
                "truncated MADT entry header at offset {offset:#x}"
            )));
        }
        let entry_type = remaining[0];
        let entry_len = usize::from(remaining[1]);
        if entry_len < size_of::<RawMadtEntryHeader>() || entry_len > remaining.len() {
            return Err(DriverError::Unknown(format!(
                "invalid MADT entry type {entry_type} length {entry_len} at offset {offset:#x}"
            )));
        }
        let entry = &remaining[..entry_len];
        match entry_type {
            ACPI_MADT_TYPE_BIO_PIC => read_loongarch_bio_pic_entry(entry, routing),
            ACPI_MADT_TYPE_LIO_PIC => read_loongarch_lio_pic_entry(entry, routing)?,
            ACPI_MADT_TYPE_EIO_PIC => read_loongarch_eio_pic_entry(entry, routing)?,
            _ => {}
        }
        offset += entry_len;
    }
    Ok(())
}

fn read_loongarch_bio_pic_entry(bytes: &[u8], routing: &mut AcpiRouting) {
    if bytes.len() < size_of::<RawMadtBioPic>() {
        warn!(
            "ignore short LoongArch BIO_PIC MADT entry: {} bytes",
            bytes.len()
        );
        return;
    }
    // SAFETY: The slice contains the complete packed wire layout. All fields
    // are integers, so every initialized bit pattern is valid; no borrow escapes.
    let entry = unsafe { bytes.as_ptr().cast::<RawMadtBioPic>().read_unaligned() };
    routing.add_pch_pic(AcpiPchPic {
        id: u16::from_le(entry.id),
        address: u64::from_le(entry.address),
        mmio_size: u16::from_le(entry.size),
        gsi_count: LOONGARCH_PCH_PIC_GSI_COUNT,
        gsi_base: u32::from(u16::from_le(entry.gsi_base)),
    });
}

fn read_loongarch_lio_pic_entry(
    bytes: &[u8],
    routing: &mut AcpiRouting,
) -> Result<(), DriverError> {
    if bytes.len() < size_of::<RawMadtLioPic>() {
        return Err(DriverError::Unsupported(
            "short LoongArch LIOPIC MADT entry",
        ));
    }
    // SAFETY: The length check covers the packed layout of integer-only
    // fields. read_unaligned copies it without creating misaligned references.
    let entry = unsafe { bytes.as_ptr().cast::<RawMadtLioPic>().read_unaligned() };
    let lio = AcpiLioPic {
        version: entry.version,
        address: u64::from_le(entry.address),
        mmio_size: u16::from_le(entry.size),
        cascade: entry.cascade,
        cascade_map: entry.cascade_map.map(u32::from_le),
    };
    if lio.version != 1 {
        return Err(DriverError::Unsupported("LoongArch LIOPIC version"));
    }
    if !routing.lio_pics.is_empty() {
        return Err(DriverError::Unsupported(
            "multiple LoongArch LIOPIC instances",
        ));
    }
    // Core registers occupy the first 0x40 bytes; core 0's ISR is a u32 at 0x40.
    if lio.address == 0
        || lio.mmio_size < 0x44
        || lio.address.checked_add(u64::from(lio.mmio_size)).is_none()
    {
        return Err(DriverError::Unknown(format!(
            "invalid LIOPIC register range: {lio:?}"
        )));
    }
    let [first, second] = lio.cascade_map;
    if first | second == 0 {
        return Err(DriverError::Unknown(format!(
            "invalid LIOPIC cascade maps: {lio:?}"
        )));
    }
    for (index, map) in lio.cascade_map.iter().enumerate() {
        if *map != 0 && !(2..=5).contains(&lio.cascade[index]) {
            return Err(DriverError::Unknown(format!(
                "invalid LIOPIC CPU cascade: {lio:?}"
            )));
        }
    }
    if first != 0 && second != 0 && lio.cascade[0] == lio.cascade[1] {
        return Err(DriverError::Unknown(format!(
            "duplicate LIOPIC CPU cascade: {lio:?}"
        )));
    }
    routing.lio_pics.push(lio);
    Ok(())
}

fn read_loongarch_eio_pic_entry(
    bytes: &[u8],
    routing: &mut AcpiRouting,
) -> Result<(), DriverError> {
    if bytes.len() < size_of::<RawMadtEioPic>() {
        return Err(DriverError::Unsupported(
            "short LoongArch EIOPIC MADT entry",
        ));
    }
    // SAFETY: The checked slice covers the packed integer-only wire layout.
    // The copied value does not retain pointers or borrows into the slice.
    let entry = unsafe { bytes.as_ptr().cast::<RawMadtEioPic>().read_unaligned() };
    routing.eio_pics.push(AcpiEioPic {
        version: entry.version,
        cascade: entry.cascade,
        node: entry.node,
        node_map: u64::from_le(entry.node_map),
    });
    Ok(())
}

fn spcr_uart_register_size(access_size: u8) -> u64 {
    let access_bytes = match access_size {
        1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
        _ => 1,
    };
    access_bytes * 8
}

#[derive(Default)]
struct AcpiDeviceResources {
    memory_ranges: Vec<AcpiResourceRange>,
    io_ranges: Vec<AcpiResourceRange>,
    irq_routes: Vec<AcpiGsiRoute>,
}

fn is_root_acpi_id_list(ids: &[AcpiId]) -> bool {
    ids.iter().any(|id| id.hid == "ACPIIOAP")
}

fn acpi_ids_match(hid: &str, cids: &[String], ids: &[AcpiId]) -> bool {
    ids.iter().any(|id| {
        id.hid == hid
            || id.cids.contains(&hid)
            || cids
                .iter()
                .any(|cid| id.hid == cid || id.cids.contains(&cid.as_str()))
    })
}

fn acpi_device_is_available(interpreter: &Interpreter<AcpiHandler>, path: &AmlName) -> bool {
    const PRESENT_AND_ENABLED: u64 = 0b11;
    match eval_integer_child(interpreter, path, "_STA") {
        Ok(Some(status)) => status & PRESENT_AND_ENABLED == PRESENT_AND_ENABLED,
        Ok(None) => true,
        Err(err) => {
            warn!("skipping ACPI device {path}: failed to evaluate _STA: {err:?}");
            false
        }
    }
}

fn acpi_device_ids(
    interpreter: &Interpreter<AcpiHandler>,
    path: &AmlName,
) -> Result<Option<(String, Vec<String>)>, AmlError> {
    let Some(hid_object) = eval_child(interpreter, path, "_HID")? else {
        return Ok(None);
    };
    let Some(hid) = acpi_id_from_object(&hid_object) else {
        return Ok(None);
    };
    let cids = match eval_child(interpreter, path, "_CID")? {
        Some(value) => acpi_ids_from_object(&value),
        None => Vec::new(),
    };
    Ok(Some((hid, cids)))
}

fn acpi_id_from_object(value: &Object) -> Option<String> {
    match value {
        Object::String(id) => Some(id.clone()),
        Object::Integer(id) => decode_eisa_id(*id as u32),
        _ => None,
    }
}

fn acpi_ids_from_object(value: &Object) -> Vec<String> {
    match value {
        Object::Package(values) => values
            .iter()
            .filter_map(|value| acpi_id_from_object(value))
            .collect(),
        _ => acpi_id_from_object(value).into_iter().collect(),
    }
}

fn read_device_resources(
    interpreter: &Interpreter<AcpiHandler>,
    path: &AmlName,
    routing: &AcpiRouting,
) -> Result<AcpiDeviceResources, AmlError> {
    let Some(value) = eval_wrapped_child(interpreter, path, "_CRS")? else {
        return Ok(AcpiDeviceResources::default());
    };
    let resources = resource_descriptor_list(value)?;
    let mut out = AcpiDeviceResources::default();
    for resource in resources {
        match resource {
            Resource::MemoryRange(memory) => match memory {
                acpi::aml::resource::MemoryRangeDescriptor::FixedLocation {
                    base_address,
                    range_length,
                    ..
                } => out.memory_ranges.push(AcpiResourceRange {
                    base: u64::from(base_address),
                    size: u64::from(range_length),
                }),
            },
            Resource::AddressSpace(address) => {
                let range = AcpiResourceRange {
                    base: address.address_range.0,
                    size: address.length,
                };
                match address.resource_type {
                    AddressSpaceResourceType::MemoryRange => out.memory_ranges.push(range),
                    AddressSpaceResourceType::IORange => out.io_ranges.push(range),
                    AddressSpaceResourceType::BusNumberRange => {}
                }
            }
            Resource::IOPort(io) => out.io_ranges.push(AcpiResourceRange {
                base: u64::from(io.memory_range.0),
                size: u64::from(io.range_length),
            }),
            Resource::Irq(irq) => {
                for &gsi in &irq.irqs {
                    if let Some(route) = routing.resolve_gsi(gsi) {
                        out.irq_routes
                            .push(route_with_irq_descriptor_flags(route, &irq));
                    } else {
                        warn!(
                            "ACPI device {} _CRS GSI {gsi} has no controller route",
                            path.as_string()
                        );
                    }
                }
            }
            // DMA and controller connections are not scalar MMIO, port-I/O or
            // GSI resources. Do not reinterpret their channels or pins as GSIs.
            Resource::Dma(_) | Resource::SerialBus(_) | Resource::GPIO(_) => {}
        }
    }
    Ok(out)
}

fn read_pci_namespace(
    interpreter: &Interpreter<AcpiHandler>,
) -> Result<AcpiPciNamespace, AcpiError> {
    let mut roots = Vec::new();
    let mut device_paths = Vec::new();
    {
        let mut namespace = interpreter.namespace.lock().clone();
        namespace
            .traverse(|path, level| {
                if level.kind != NamespaceLevelKind::Device {
                    return Ok(true);
                }
                device_paths.push(path.clone());
                if is_pci_root(interpreter, path) {
                    let segment =
                        eval_integer_child(interpreter, path, "_SEG")?.unwrap_or(0) as u16;
                    let bus = eval_integer_child(interpreter, path, "_BBN")?.unwrap_or(0) as u8;
                    roots.push(AcpiPciRoot {
                        segment,
                        bus,
                        path: path.as_string(),
                        dma_coherent: inherited_device_cca(interpreter, path, &device_paths),
                        prt: None,
                        link_prt: None,
                    });
                }
                Ok(true)
            })
            .map_err(AcpiError::Aml)?;
    }

    for root in &mut roots {
        root.prt = read_pci_routing_table(interpreter, &root.path)?;
        root.link_prt = read_pci_link_routing_table(interpreter, &root.path)?;
    }
    for path in PCI_ROOT_FALLBACK_PATHS {
        if roots.iter().any(|root| root.path == *path) {
            continue;
        }
        let Some(prt) = read_pci_routing_table(interpreter, path)? else {
            continue;
        };
        let link_prt = read_pci_link_routing_table(interpreter, path)?;
        roots.push(AcpiPciRoot {
            segment: 0,
            bus: 0,
            path: path.to_string(),
            dma_coherent: inherited_device_cca(
                interpreter,
                &AmlName::from_str(path).map_err(AcpiError::Aml)?,
                &device_paths,
            ),
            prt: Some(prt),
            link_prt,
        });
    }

    Ok(AcpiPciNamespace {
        link_allocator: RawSpinLock::new(PciLinkAllocator::default()),
        roots,
    })
}

fn inherited_device_cca(
    interpreter: &Interpreter<AcpiHandler>,
    path: &AmlName,
    device_paths: &[AmlName],
) -> Option<bool> {
    let mut lineage = Vec::new();
    let mut current = path.clone();
    loop {
        lineage.push(current.clone());
        match current.parent() {
            Ok(parent) => current = parent,
            Err(AmlError::RootHasNoParent) => break,
            Err(_) => return None,
        }
    }
    lineage.reverse();

    for ancestor in lineage {
        if !device_paths.iter().any(|device| device == &ancestor) {
            continue;
        }
        // Linux treats any failed `_CCA` integer evaluation as absent. On
        // architectures that require `_CCA`, the consumer rejects the final
        // `None`; other architectures apply their specified coherent default.
        if let Ok(Some(value)) = eval_integer_child(interpreter, &ancestor, "_CCA") {
            return Some(value != 0);
        }
    }
    None
}

fn apply_pci_root_dma_coherency(
    regions: &mut [AcpiPciEcam],
    pci: &AcpiPciNamespace,
) -> Result<(), DriverError> {
    for region in regions {
        let mut declared = pci
            .roots
            .iter()
            .filter(|root| {
                root.segment == region.segment_group
                    && (region.bus_start..=region.bus_end).contains(&root.bus)
            })
            .filter_map(|root| root.dma_coherent);
        let first = declared.next();
        if declared.any(|value| Some(value) != first) {
            return Err(DriverError::Unknown(format!(
                "ACPI PCI roots in segment {} buses {}..={} declare conflicting _CCA values",
                region.segment_group, region.bus_start, region.bus_end
            )));
        }
        region.dma_coherent = first;
    }
    Ok(())
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

fn read_pci_link_routing_table(
    interpreter: &Interpreter<AcpiHandler>,
    root_path: &str,
) -> Result<Option<PciLinkRoutingTable>, AcpiError> {
    let prt_path = AmlName::from_str(&format!("{root_path}._PRT")).map_err(AcpiError::Aml)?;
    match PciLinkRoutingTable::from_prt_path(prt_path, interpreter) {
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
    match eval_wrapped_child(interpreter, path, name)? {
        Some(value) => Ok(Some(Rc::new((*value).clone()))),
        None => Ok(None),
    }
}

fn eval_wrapped_child(
    interpreter: &Interpreter<AcpiHandler>,
    path: &AmlName,
    name: &str,
) -> Result<Option<acpi::aml::object::WrappedObject>, AmlError> {
    let child = AmlName::from_str(name)?.resolve(path)?;
    interpreter.evaluate_if_present(child, Vec::new())
}

fn decode_eisa_id(raw: u32) -> Option<String> {
    if raw == 0 {
        return None;
    }
    let bytes = raw.to_le_bytes();
    let chars = [
        ((bytes[0] >> 2) & 0x1f).wrapping_add(b'@'),
        (((bytes[0] & 0x03) << 3) | (bytes[1] >> 5)).wrapping_add(b'@'),
        (bytes[1] & 0x1f).wrapping_add(b'@'),
    ];
    if !chars.iter().all(u8::is_ascii_uppercase) {
        return None;
    }
    Some(format!(
        "{}{}{}{:02X}{:02X}",
        chars[0] as char, chars[1] as char, chars[2] as char, bytes[2], bytes[3]
    ))
}

#[derive(Debug)]
struct PciLinkRoutingTable {
    entries: Vec<PciLinkRoute>,
}

#[derive(Debug)]
struct PciLinkRoute {
    device: u16,
    function: u16,
    pin: Pin,
    link: AmlName,
    source_index: usize,
}

impl PciLinkRoutingTable {
    fn from_prt_path(
        prt_path: AmlName,
        interpreter: &Interpreter<AcpiHandler>,
    ) -> Result<Self, AmlError> {
        let prt = interpreter.evaluate(prt_path.clone(), Vec::new())?;
        let Object::Package(ref entries) = *prt else {
            return Err(AmlError::InvalidOperationOnObject {
                op: acpi::aml::Operation::DecodePrt,
                typ: prt.typ(),
            });
        };

        let mut routes = Vec::new();
        for entry in entries {
            let Object::Package(ref package) = **entry else {
                return Err(AmlError::InvalidOperationOnObject {
                    op: acpi::aml::Operation::DecodePrt,
                    typ: entry.typ(),
                });
            };
            if package.len() != 4 {
                return Err(AmlError::UnexpectedResourceType);
            }

            let Object::Integer(address) = *package[0] else {
                return Err(AmlError::PrtInvalidAddress);
            };
            let entry_device =
                u16::try_from((address >> 16) & 0xffff).map_err(|_| AmlError::PrtInvalidAddress)?;
            let entry_function =
                u16::try_from(address & 0xffff).map_err(|_| AmlError::PrtInvalidAddress)?;
            let entry_pin = match *package[1] {
                Object::Integer(0) => Pin::IntA,
                Object::Integer(1) => Pin::IntB,
                Object::Integer(2) => Pin::IntC,
                Object::Integer(3) => Pin::IntD,
                _ => return Err(AmlError::PrtInvalidPin),
            };
            let source = package[2].clone().unwrap_reference();
            let link = match *source {
                Object::NamePath {
                    ref name,
                    ref scope,
                } => interpreter.namespace.lock().search_for_level(name, scope)?,
                Object::String(ref name) => interpreter
                    .namespace
                    .lock()
                    .search_for_level(&AmlName::from_str(name)?, &prt_path)?,
                Object::Integer(0) => continue,
                _ => return Err(AmlError::PrtInvalidSource),
            };
            let Object::Integer(source_index) = *package[3] else {
                return Err(AmlError::PrtInvalidSource);
            };
            let source_index =
                usize::try_from(source_index).map_err(|_| AmlError::PrtInvalidSource)?;
            routes.push(PciLinkRoute {
                device: entry_device,
                function: entry_function,
                pin: entry_pin,
                link,
                source_index,
            });
        }

        Ok(Self { entries: routes })
    }

    fn route(
        &self,
        device: u16,
        function: u16,
        pin: Pin,
        interpreter: &Interpreter<AcpiHandler>,
        handler: &AcpiHandler,
        allocator: &mut PciLinkAllocator,
    ) -> Result<Option<IrqDescriptor>, AmlError> {
        let Some(route) = self.entries.iter().find(|entry| {
            entry.device == device
                && (entry.function == 0xffff || entry.function == function)
                && entry.pin == pin
        }) else {
            return Ok(None);
        };

        resolve_pci_link_irq(
            interpreter,
            handler,
            allocator,
            &route.link,
            route.source_index,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkIrqResourceKind {
    SmallIrq,
    ExtendedIrq,
}

#[derive(Clone, Debug)]
struct LinkIrqResource {
    kind: LinkIrqResourceKind,
    descriptor: IrqDescriptor,
}

struct LinkIrqSelection<'a> {
    resource: &'a LinkIrqResource,
    irq: u32,
    needs_programming: bool,
}

#[derive(Default)]
struct PciLinkAllocator {
    assigned: BTreeMap<String, u32>,
    irq_use_count: BTreeMap<u32, usize>,
}

impl PciLinkAllocator {
    fn assigned_irq(&self, link: &AmlName) -> Option<u32> {
        self.assigned.get(&link.as_string()).copied()
    }

    fn commit(&mut self, link: &AmlName, irq: u32) {
        let key = link.as_string();
        match self.assigned.insert(key, irq) {
            Some(old_irq) if old_irq == irq => return,
            Some(old_irq) => {
                if let Some(count) = self.irq_use_count.get_mut(&old_irq) {
                    *count = count.saturating_sub(1);
                }
            }
            None => {}
        }
        *self.irq_use_count.entry(irq).or_default() += 1;
    }

    fn penalty(&self, irq: u32) -> usize {
        let isa_penalty = match irq {
            0..=2 => usize::MAX / 2,
            3..=8 => 16 * 16 * 16 * 16,
            9..=11 => 0,
            12..=15 => 16 * 16 * 16 * 16 * 16,
            _ => 0,
        };
        isa_penalty + self.irq_use_count.get(&irq).copied().unwrap_or(0) * 16 * 16 * 16
    }
}

fn resolve_pci_link_irq(
    interpreter: &Interpreter<AcpiHandler>,
    handler: &AcpiHandler,
    allocator: &mut PciLinkAllocator,
    link: &AmlName,
    source_index: usize,
) -> Result<Option<IrqDescriptor>, AmlError> {
    let current = evaluate_link_irq_resource(interpreter, link, "_CRS", source_index)?;
    let possible = evaluate_link_irq_resource(interpreter, link, "_PRS", source_index)?;
    let Some(selection) = select_pci_link_irq(link, current.as_ref(), possible.as_ref(), allocator)
    else {
        return Ok(None);
    };

    if selection.needs_programming {
        let srs = build_link_srs_buffer(selection.resource, selection.irq)?;
        let srs_path = AmlName::from_str("_SRS")?.resolve(link)?;
        if let Err(err) = interpreter.evaluate(srs_path.clone(), vec![Object::Buffer(srs).wrap()]) {
            if is_buffer_field_to_field_unit_store_gap(&err) {
                let _ = interpreter.namespace.lock().remove_level(srs_path);
                // Current acpi's AML Store path cannot coerce a BufferField into a FieldUnit.
                // SeaBIOS PIRQ link _SRS methods use that exact pattern; program the already
                // decoded link field directly so PCI link allocation still follows _PRS/_SRS.
                program_known_pci_link_field(interpreter, handler, link, selection.irq)?;
            } else {
                let _ = interpreter.namespace.lock().remove_level(srs_path);
                return Err(err);
            }
        }
    }

    allocator.commit(link, selection.irq);
    Ok(Some(selection.resource.descriptor_for_irq(selection.irq)))
}

fn evaluate_link_irq_resource(
    interpreter: &Interpreter<AcpiHandler>,
    link: &AmlName,
    method: &str,
    source_index: usize,
) -> Result<Option<LinkIrqResource>, AmlError> {
    let path = AmlName::from_str(method)?.resolve(link)?;
    let value = match interpreter.evaluate_if_present(path.clone(), Vec::new()) {
        Ok(Some(value)) => value,
        Ok(None) => return Ok(None),
        Err(err) if method == "_CRS" && is_buffer_field_to_field_unit_store_gap(&err) => {
            let _ = interpreter.namespace.lock().remove_level(path);
            return Ok(None);
        }
        Err(err) => {
            let _ = interpreter.namespace.lock().remove_level(path);
            return Err(err);
        }
    };
    let resources = parse_link_irq_resources(&value)?;
    Ok(resources.into_iter().nth(source_index))
}

fn is_buffer_field_to_field_unit_store_gap(err: &AmlError) -> bool {
    matches!(
        err,
        AmlError::ObjectNotOfExpectedType {
            expected: ObjectType::Integer,
            got: ObjectType::BufferField,
        }
    )
}

fn program_known_pci_link_field(
    interpreter: &Interpreter<AcpiHandler>,
    handler: &AcpiHandler,
    link: &AmlName,
    irq: u32,
) -> Result<(), AmlError> {
    let Some(field_path) = known_pci_link_irq_field(interpreter, link) else {
        return Err(AmlError::ObjectNotOfExpectedType {
            expected: ObjectType::Integer,
            got: ObjectType::BufferField,
        });
    };
    let field = interpreter.namespace.lock().get(field_path)?.clone();
    let Object::FieldUnit(ref field) = *field else {
        return Err(AmlError::ObjectNotOfExpectedType {
            expected: ObjectType::FieldUnit,
            got: field.typ(),
        });
    };
    write_field_unit(interpreter, handler, field, irq as u64)
}

fn known_pci_link_irq_field(
    interpreter: &Interpreter<AcpiHandler>,
    link: &AmlName,
) -> Option<AmlName> {
    let mut namespace = interpreter.namespace.lock();
    for field in pci_link_irq_field_candidates(link) {
        let path = AmlName::from_str(field).ok()?;
        if namespace.get(path.clone()).is_ok() {
            return Some(path);
        }
    }
    None
}

fn pci_link_irq_field_candidates(link: &AmlName) -> &'static [&'static str] {
    match link.as_string().rsplit('.').next().unwrap_or_default() {
        "LNKA" => &["\\_SB.PRQA", "\\_SB.PRQ0"],
        "LNKB" => &["\\_SB.PRQB", "\\_SB.PRQ1"],
        "LNKC" => &["\\_SB.PRQC", "\\_SB.PRQ2"],
        "LNKD" => &["\\_SB.PRQD", "\\_SB.PRQ3"],
        "LNKE" => &["\\_SB.PRQE"],
        "LNKF" => &["\\_SB.PRQF"],
        "LNKG" => &["\\_SB.PRQG"],
        "LNKH" => &["\\_SB.PRQH"],
        _ => &[],
    }
}

fn write_field_unit(
    interpreter: &Interpreter<AcpiHandler>,
    handler: &AcpiHandler,
    field: &FieldUnit,
    value: u64,
) -> Result<(), AmlError> {
    let access_width_bits = field.flags.access_type_bytes()? * 8;
    let FieldUnitKind::Normal { ref region } = field.kind else {
        return Err(AmlError::LibUnimplemented);
    };
    let Object::OpRegion(ref region) = **region else {
        return Err(AmlError::ObjectNotOfExpectedType {
            expected: ObjectType::OpRegion,
            got: region.typ(),
        });
    };

    let value_bytes = value.to_le_bytes();
    let native_accesses =
        (field.bit_length + (field.bit_index % access_width_bits)).div_ceil(access_width_bits);
    let mut written_so_far = 0;

    for i in 0..native_accesses {
        let aligned_bit = align_down(field.bit_index + i * access_width_bits, access_width_bits);
        let dst_index = if i == 0 {
            field.bit_index % access_width_bits
        } else {
            0
        };
        let remaining = field.bit_length - written_so_far;
        let length = if i == 0 {
            usize::min(
                remaining,
                access_width_bits - (field.bit_index % access_width_bits),
            )
        } else {
            usize::min(remaining, access_width_bits)
        };

        let mut bytes = if dst_index > 0 || remaining < access_width_bits {
            match field.flags.update_rule() {
                FieldUpdateRule::Preserve => read_native_region(
                    interpreter,
                    handler,
                    region,
                    aligned_bit / 8,
                    access_width_bits / 8,
                )?
                .to_le_bytes(),
                FieldUpdateRule::WriteAsOnes => [0xff; 8],
                FieldUpdateRule::WriteAsZeros => [0; 8],
            }
        } else {
            [0; 8]
        };

        copy_bits(&value_bytes, written_so_far, &mut bytes, dst_index, length);
        write_native_region(
            interpreter,
            handler,
            region,
            aligned_bit / 8,
            access_width_bits / 8,
            u64::from_le_bytes(bytes),
        )?;
        written_so_far += length;
    }

    Ok(())
}

fn read_native_region(
    interpreter: &Interpreter<AcpiHandler>,
    handler: &AcpiHandler,
    region: &OpRegion,
    offset: usize,
    length: usize,
) -> Result<u64, AmlError> {
    match region.space {
        RegionSpace::SystemMemory => {
            let address = region.base as usize + offset;
            match length {
                1 => Ok(u64::from(handler.read_u8(address))),
                2 => Ok(u64::from(handler.read_u16(address))),
                4 => Ok(u64::from(handler.read_u32(address))),
                8 => Ok(handler.read_u64(address)),
                _ => Err(AmlError::InvalidFieldFlags),
            }
        }
        RegionSpace::SystemIO => {
            let address = region.base as u16 + offset as u16;
            match length {
                1 => Ok(u64::from(handler.read_io_u8(address))),
                2 => Ok(u64::from(handler.read_io_u16(address))),
                4 => Ok(u64::from(handler.read_io_u32(address))),
                _ => Err(AmlError::InvalidFieldFlags),
            }
        }
        RegionSpace::PciConfig => {
            let address = pci_address_for_region(interpreter, region)?;
            let offset = region.base as u16 + offset as u16;
            match length {
                1 => Ok(u64::from(handler.read_pci_u8(address, offset))),
                2 => Ok(u64::from(handler.read_pci_u16(address, offset))),
                4 => Ok(u64::from(handler.read_pci_u32(address, offset))),
                _ => Err(AmlError::InvalidFieldFlags),
            }
        }
        _ => Err(AmlError::NoHandlerForRegionAccess(region.space)),
    }
}

fn write_native_region(
    interpreter: &Interpreter<AcpiHandler>,
    handler: &AcpiHandler,
    region: &OpRegion,
    offset: usize,
    length: usize,
    value: u64,
) -> Result<(), AmlError> {
    match region.space {
        RegionSpace::SystemMemory => {
            let address = region.base as usize + offset;
            match length {
                1 => handler.write_u8(address, value as u8),
                2 => handler.write_u16(address, value as u16),
                4 => handler.write_u32(address, value as u32),
                8 => handler.write_u64(address, value),
                _ => return Err(AmlError::InvalidFieldFlags),
            }
            Ok(())
        }
        RegionSpace::SystemIO => {
            let address = region.base as u16 + offset as u16;
            match length {
                1 => handler.write_io_u8(address, value as u8),
                2 => handler.write_io_u16(address, value as u16),
                4 => handler.write_io_u32(address, value as u32),
                _ => return Err(AmlError::InvalidFieldFlags),
            }
            Ok(())
        }
        RegionSpace::PciConfig => {
            let address = pci_address_for_region(interpreter, region)?;
            let offset = region.base as u16 + offset as u16;
            match length {
                1 => handler.write_pci_u8(address, offset, value as u8),
                2 => handler.write_pci_u16(address, offset, value as u16),
                4 => handler.write_pci_u32(address, offset, value as u32),
                _ => return Err(AmlError::InvalidFieldFlags),
            }
            Ok(())
        }
        _ => Err(AmlError::NoHandlerForRegionAccess(region.space)),
    }
}

fn pci_address_for_region(
    interpreter: &Interpreter<AcpiHandler>,
    region: &OpRegion,
) -> Result<acpi::PciAddress, AmlError> {
    let path = &region.parent_device_path;
    let segment = eval_integer_child(interpreter, path, "_SEG")?.unwrap_or(0) as u16;
    let bus = eval_integer_child(interpreter, path, "_BBN")?.unwrap_or(0) as u8;
    let address = eval_integer_child(interpreter, path, "_ADR")?.unwrap_or(0);
    Ok(acpi::PciAddress::new(
        segment,
        bus,
        ((address >> 16) & 0xff) as u8,
        (address & 0xff) as u8,
    ))
}

fn copy_bits(src: &[u8], src_offset: usize, dst: &mut [u8], dst_offset: usize, length: usize) {
    for bit in 0..length {
        let src_bit = src_offset + bit;
        let dst_bit = dst_offset + bit;
        let is_set = src[src_bit / 8] & (1 << (src_bit % 8)) != 0;
        if is_set {
            dst[dst_bit / 8] |= 1 << (dst_bit % 8);
        } else {
            dst[dst_bit / 8] &= !(1 << (dst_bit % 8));
        }
    }
}

fn align_down(value: usize, align: usize) -> usize {
    assert!(align.is_power_of_two());
    value & !(align - 1)
}

fn parse_link_irq_resources(
    value: &acpi::aml::object::WrappedObject,
) -> Result<Vec<LinkIrqResource>, AmlError> {
    let Object::Buffer(ref bytes) = **value else {
        return Err(AmlError::InvalidOperationOnObject {
            op: acpi::aml::Operation::ParseResource,
            typ: value.typ(),
        });
    };

    let mut resources = Vec::new();
    let mut rest = bytes.as_slice();
    while !rest.is_empty() {
        if rest[0] & 0x80 != 0 {
            if rest.len() < 3 {
                return Err(AmlError::InvalidResourceDescriptor);
            }
            let length = usize::from(u16::from_le_bytes([rest[1], rest[2]]));
            if rest.len() < length + 3 {
                return Err(AmlError::InvalidResourceDescriptor);
            }
            let descriptor = &rest[..length + 3];
            if rest[0] & 0x7f == 0x09 {
                resources.push(parse_extended_irq_resource(descriptor)?);
            }
            rest = &rest[length + 3..];
        } else {
            let length = usize::from(rest[0] & 0b111);
            if rest.len() < length + 1 {
                return Err(AmlError::InvalidResourceDescriptor);
            }
            let descriptor = &rest[..length + 1];
            match (rest[0] >> 3) & 0x0f {
                0x04 => resources.push(parse_small_irq_resource(descriptor)?),
                0x0f => break,
                _ => {}
            }
            rest = &rest[length + 1..];
        }
    }
    Ok(resources)
}

fn parse_extended_irq_resource(bytes: &[u8]) -> Result<LinkIrqResource, AmlError> {
    if bytes.len() < 5 {
        return Err(AmlError::InvalidResourceDescriptor);
    }
    let count = usize::from(bytes[4]);
    if bytes.len() < 5 + count * 4 {
        return Err(AmlError::InvalidResourceDescriptor);
    }

    let mut irqs = Irqs::new();
    for idx in 0..count {
        let start = 5 + idx * 4;
        irqs.push(u32::from_le_bytes([
            bytes[start],
            bytes[start + 1],
            bytes[start + 2],
            bytes[start + 3],
        ]));
    }

    Ok(LinkIrqResource {
        kind: LinkIrqResourceKind::ExtendedIrq,
        descriptor: IrqDescriptor {
            is_consumer: bytes[3] & 0b0000_0001 != 0,
            trigger: if bytes[3] & 0b0000_0010 != 0 {
                InterruptTrigger::Edge
            } else {
                InterruptTrigger::Level
            },
            polarity: if bytes[3] & 0b0000_0100 != 0 {
                InterruptPolarity::ActiveLow
            } else {
                InterruptPolarity::ActiveHigh
            },
            is_shared: bytes[3] & 0b0000_1000 != 0,
            is_wake_capable: bytes[3] & 0b0001_0000 != 0,
            irqs,
        },
    })
}

fn parse_small_irq_resource(bytes: &[u8]) -> Result<LinkIrqResource, AmlError> {
    if bytes.len() != 3 && bytes.len() != 4 {
        return Err(AmlError::InvalidResourceDescriptor);
    }

    let mask = u16::from_le_bytes([bytes[1], bytes[2]]);
    let mut irqs = Irqs::new();
    for irq in 0..16 {
        if mask & (1 << irq) != 0 {
            irqs.push(irq);
        }
    }

    let (trigger, polarity, is_shared, is_wake_capable) = if bytes.len() == 4 {
        (
            if bytes[3] & 0b0000_0001 != 0 {
                InterruptTrigger::Edge
            } else {
                InterruptTrigger::Level
            },
            if bytes[3] & 0b0000_1000 != 0 {
                InterruptPolarity::ActiveLow
            } else {
                InterruptPolarity::ActiveHigh
            },
            bytes[3] & 0b0001_0000 != 0,
            bytes[3] & 0b0010_0000 != 0,
        )
    } else {
        (
            InterruptTrigger::Edge,
            InterruptPolarity::ActiveHigh,
            false,
            false,
        )
    };

    Ok(LinkIrqResource {
        kind: LinkIrqResourceKind::SmallIrq,
        descriptor: IrqDescriptor {
            is_consumer: false,
            trigger,
            polarity,
            is_shared,
            is_wake_capable,
            irqs,
        },
    })
}

fn select_pci_link_irq<'a>(
    link: &AmlName,
    current: Option<&'a LinkIrqResource>,
    possible: Option<&'a LinkIrqResource>,
    allocator: &PciLinkAllocator,
) -> Option<LinkIrqSelection<'a>> {
    if let Some(irq) = allocator.assigned_irq(link) {
        let resource = possible
            .filter(|possible| possible.descriptor.irqs.contains(&irq))
            .or(current.filter(|current| current.descriptor.irqs.contains(&irq)))?;
        return Some(LinkIrqSelection {
            resource,
            irq,
            needs_programming: current
                .is_none_or(|current| pci_irq_descriptor_gsi(&current.descriptor) != Some(irq)),
        });
    }

    if let Some(current) = current
        && let Some(irq) = pci_irq_descriptor_gsi(&current.descriptor).filter(|irq| *irq != 0)
        && possible.is_none_or(|possible| possible.descriptor.irqs.contains(&irq))
    {
        return Some(LinkIrqSelection {
            resource: possible.unwrap_or(current),
            irq,
            needs_programming: false,
        });
    }

    let possible = possible?;
    let irq = preferred_pci_link_irq(&possible.descriptor.irqs, allocator)?;
    Some(LinkIrqSelection {
        resource: possible,
        irq,
        needs_programming: true,
    })
}

fn preferred_pci_link_irq(irqs: &[u32], allocator: &PciLinkAllocator) -> Option<u32> {
    irqs.iter()
        .copied()
        .filter(|irq| *irq != 0)
        .min_by_key(|irq| (allocator.penalty(*irq), link_irq_tiebreaker(*irq)))
}

fn link_irq_tiebreaker(irq: u32) -> usize {
    match irq {
        10 => 0,
        11 => 1,
        9 => 2,
        16.. => 3 + irq as usize,
        _ => 1024 + irq as usize,
    }
}

fn build_link_srs_buffer(resource: &LinkIrqResource, irq: u32) -> Result<Vec<u8>, AmlError> {
    match resource.kind {
        LinkIrqResourceKind::ExtendedIrq => {
            let mut buffer = Vec::new();
            buffer.extend_from_slice(&[
                0x89,
                0x06,
                0x00,
                extended_irq_flags(&resource.descriptor),
                1,
            ]);
            buffer.extend_from_slice(&irq.to_le_bytes());
            buffer.extend_from_slice(&[0x79, 0x00]);
            Ok(buffer)
        }
        LinkIrqResourceKind::SmallIrq => {
            if irq >= 16 {
                return Err(AmlError::InvalidResourceDescriptor);
            }
            let mask = 1u16 << irq;
            let mut buffer = Vec::new();
            buffer.push((0x04 << 3) | 3);
            buffer.extend_from_slice(&mask.to_le_bytes());
            buffer.push(small_irq_flags(&resource.descriptor));
            buffer.extend_from_slice(&[0x79, 0x00]);
            Ok(buffer)
        }
    }
}

impl LinkIrqResource {
    fn descriptor_for_irq(&self, irq: u32) -> IrqDescriptor {
        let mut descriptor = self.descriptor.clone();
        descriptor.irqs = Irqs::from_buf([irq]);
        descriptor
    }
}

fn extended_irq_flags(descriptor: &IrqDescriptor) -> u8 {
    let mut flags = 0;
    if descriptor.is_consumer {
        flags |= 1 << 0;
    }
    if descriptor.trigger == InterruptTrigger::Edge {
        flags |= 1 << 1;
    }
    if descriptor.polarity == InterruptPolarity::ActiveLow {
        flags |= 1 << 2;
    }
    if descriptor.is_shared {
        flags |= 1 << 3;
    }
    if descriptor.is_wake_capable {
        flags |= 1 << 4;
    }
    flags
}

fn small_irq_flags(descriptor: &IrqDescriptor) -> u8 {
    let mut flags = 0;
    if descriptor.trigger == InterruptTrigger::Edge {
        flags |= 1 << 0;
    }
    if descriptor.polarity == InterruptPolarity::ActiveLow {
        flags |= 1 << 3;
    }
    if descriptor.is_shared {
        flags |= 1 << 4;
    }
    if descriptor.is_wake_capable {
        flags |= 1 << 5;
    }
    flags
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

/// PCI routing requires one selected IRQ, never a list of possible choices.
fn pci_irq_descriptor_gsi(descriptor: &IrqDescriptor) -> Option<u32> {
    match descriptor.irqs.as_slice() {
        [irq] => Some(*irq),
        _ => None,
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

#[cfg(test)]
mod tests {
    mod pci_config;

    fn bytes(words: &mut [u64]) -> &mut [u8] {
        // SAFETY: The exclusive borrow covers initialized u64 storage. Every byte
        // pattern remains a valid u64, and the slice stays within that allocation.
        unsafe {
            core::slice::from_raw_parts_mut(
                words.as_mut_ptr().cast(),
                core::mem::size_of_val(words),
            )
        }
    }

    fn checksum(bytes: &mut [u8], at: usize) {
        bytes[at] = 0;
        bytes[at] = bytes
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
    }

    fn sdt(signature: &[u8; 4], oem: &[u8; 6], table: &[u8; 8], body: &[u8]) -> Vec<u64> {
        let length = 36 + body.len();
        let mut words = alloc::vec![0; length.div_ceil(8)];
        let contents = &mut bytes(&mut words)[..length];
        contents[..4].copy_from_slice(signature);
        contents[4..8].copy_from_slice(&(length as u32).to_le_bytes());
        contents[8] = 1;
        contents[10..16].copy_from_slice(oem);
        contents[16..24].copy_from_slice(table);
        contents[24..28].copy_from_slice(&1u32.to_le_bytes());
        contents[28..32].copy_from_slice(b"TEST");
        contents[36..].copy_from_slice(body);
        checksum(contents, 9);
        words
    }

    fn rsdp_for(xsdt: &[u64]) -> Vec<u64> {
        let mut rsdp = alloc::vec![0u64; 5];
        let rsdp_bytes = &mut bytes(&mut rsdp)[..36];
        rsdp_bytes[..8].copy_from_slice(b"RSD PTR ");
        rsdp_bytes[9..15].copy_from_slice(b"TESTOS");
        rsdp_bytes[15] = 2;
        rsdp_bytes[20..24].copy_from_slice(&36u32.to_le_bytes());
        rsdp_bytes[24..32].copy_from_slice(&(xsdt.as_ptr() as u64).to_le_bytes());
        checksum(&mut rsdp_bytes[..20], 8);
        checksum(rsdp_bytes, 32);

        rsdp
    }

    fn root_with_spcr(rsdp: &[u64], spcr: &[u64]) -> super::AcpiRoot {
        let memory = alloc::boxed::Box::leak(alloc::boxed::Box::new([
            kernutil::memory::MemoryDescriptor {
                physical_start: spcr.as_ptr() as usize,
                size_in_bytes: core::mem::size_of_val(spcr),
                memory_type: kernutil::memory::MemoryType::Reserved,
            },
        ]));
        super::AcpiRoot::identity(rsdp.as_ptr() as usize).with_firmware_memory(memory)
    }

    #[test]
    fn spcr_lookup_keeps_an_explicit_namespace_selection() {
        use super::{AcpiResourceAddress, SpcrConsoleNotFound};
        use crate::DeviceId;

        let registered = DeviceId::from(1);
        let other = DeviceId::from(2);
        for (namespace, named_device, expected) in [
            (
                b"\\_SB.COM1\0".as_slice(),
                registered,
                Err(SpcrConsoleNotFound),
            ),
            (
                b"invalid\0".as_slice(),
                registered,
                Err(SpcrConsoleNotFound),
            ),
            (b"\\_SB.COM0\0".as_slice(), other, Err(SpcrConsoleNotFound)),
            (b"\\_SB.COM0\0".as_slice(), registered, Ok(Some(registered))),
            (b".\0".as_slice(), registered, Ok(Some(registered))),
        ] {
            let mut body = alloc::vec![0; 52 + namespace.len()];
            body[0] = 0x12; // Generic 16550.
            body[5] = 8; // GAS register width.
            body[7] = 1; // Byte access.
            body[8..16].copy_from_slice(&0x2000_u64.to_le_bytes());
            body[48..50].copy_from_slice(&(namespace.len() as u16).to_le_bytes());
            body[50..52].copy_from_slice(&88_u16.to_le_bytes());
            body[52..].copy_from_slice(namespace);
            let mut spcr = sdt(b"SPCR", b"TESTOS", b"UARTTEST", &body);
            let spcr_bytes = &mut bytes(&mut spcr)[..36 + body.len()];
            spcr_bytes[8] = 4;
            checksum(spcr_bytes, 9);

            let xsdt = sdt(
                b"XSDT",
                b"TESTOS",
                b"UARTTEST",
                &(spcr.as_ptr() as u64).to_le_bytes(),
            );
            let rsdp = rsdp_for(&xsdt);
            let mut system = test_system();
            system.handler = AcpiHandler::new(root_with_spcr(&rsdp, &spcr), Vec::new());
            // An address match exists, but the firmware explicitly selected an ACPI object.
            system.note_populated_resource(registered, AcpiResourceAddress::memory(0x2000));
            system
                .populated_paths
                .lock()
                .insert(String::from(r"\_SB_.COM0"), named_device);

            assert_eq!(
                system.spcr_console_device_id_result(),
                expected,
                "namespace {namespace:?}"
            );
        }
    }

    #[test]
    fn spcr_lookup_distinguishes_truncated_tables_from_disabled_redirection() {
        for (revision, body_len, declared_length, expected) in [
            (2, 44, 80, Ok(None)),
            (2, 43, 79, Err(super::SpcrConsoleNotFound)),
            (4, 44, 80, Err(super::SpcrConsoleNotFound)),
            (2, 44, 81, Err(super::SpcrConsoleNotFound)),
            (4, 52, 4096, Err(super::SpcrConsoleNotFound)),
            (4, 52, u32::MAX, Err(super::SpcrConsoleNotFound)),
        ] {
            let mut spcr = sdt(b"SPCR", b"TESTOS", b"UARTTEST", &alloc::vec![0; body_len]);
            let table = &mut bytes(&mut spcr)[..36 + body_len];
            table[8] = revision;
            table[4..8].copy_from_slice(&declared_length.to_le_bytes());
            checksum(table, 9);
            let xsdt = sdt(
                b"XSDT",
                b"TESTOS",
                b"UARTTEST",
                &(spcr.as_ptr() as u64).to_le_bytes(),
            );
            let rsdp = rsdp_for(&xsdt);
            let mut system = test_system();
            system.handler = AcpiHandler::new(root_with_spcr(&rsdp, &spcr), Vec::new());

            assert_eq!(
                system.spcr_console_device_id_result(),
                expected,
                "SPCR revision {revision}, body length {body_len}"
            );
        }
    }

    #[test]
    fn invalid_spcr_namespace_blocks_default_layout_at_gas_address() {
        let namespace = b"\\_SB.COM0"; // Missing its terminating NUL.
        let mut body = alloc::vec![0; 52 + namespace.len()];
        body[0] = 0x12;
        body[5] = 32;
        body[7] = 3;
        body[8..16].copy_from_slice(&0x2000_u64.to_le_bytes());
        body[48..50].copy_from_slice(&(namespace.len() as u16).to_le_bytes());
        body[50..52].copy_from_slice(&88_u16.to_le_bytes());
        body[52..].copy_from_slice(namespace);
        let mut spcr = sdt(b"SPCR", b"TESTOS", b"UARTTEST", &body);
        let table = &mut bytes(&mut spcr)[..36 + body.len()];
        table[8] = 4;
        checksum(table, 9);
        let xsdt = sdt(
            b"XSDT",
            b"TESTOS",
            b"UARTTEST",
            &(spcr.as_ptr() as u64).to_le_bytes(),
        );
        let rsdp = rsdp_for(&xsdt);
        let mut system = test_system();
        system.handler = AcpiHandler::new(root_with_spcr(&rsdp, &spcr), Vec::new());

        let device = super::AcpiDeviceInfo {
            path: String::from(r"\_SB_.COM0"),
            hid: None,
            cids: Vec::new(),
            memory_ranges: alloc::vec![super::AcpiResourceRange {
                base: 0x2000,
                size: 0x100
            }],
            io_ranges: Vec::new(),
            irq_routes: Vec::new(),
            dma_coherent: None,
        };
        let settings = |device: &super::AcpiDeviceInfo| {
            super::AcpiInfo {
                root: &system,
                path: &device.path,
                irq_route: None,
                device: Some(device),
            }
            .spcr_16550()
        };
        assert!(settings(&device).is_err());
        assert_eq!(
            system.spcr_console_device_id_result(),
            Err(super::SpcrConsoleNotFound)
        );
    }

    #[test]
    fn legacy_spcr_ignores_v4_extension_outside_the_table() {
        use kernutil::acpi::Spcr;

        let mut bytes = [0; 100];
        bytes[..4].copy_from_slice(b"SPCR");
        bytes[4..8].copy_from_slice(&80_u32.to_le_bytes());
        bytes[8] = 3;
        bytes[36] = 0x12; // Generic 16550.
        bytes[41] = 32;
        bytes[43] = 1;
        bytes[44..52].copy_from_slice(&0x2000_u64.to_le_bytes());
        bytes[58] = 7;
        bytes[76..80].copy_from_slice(&24_000_000_u32.to_le_bytes());
        // These bytes can belong to the next ACPI record.
        bytes[80..84].copy_from_slice(&1_500_000_u32.to_le_bytes());
        let legacy = Spcr::from_bytes(&bytes).unwrap();
        assert_eq!(legacy.baud_rate(), Some(115_200));
        assert_eq!(legacy.uart_clock_frequency(), Some(24_000_000));
        assert_eq!(legacy.namespace_path(), Ok(None));
        assert_eq!(legacy.ns16550_mmio_layout(), Some((4, 1)));

        bytes[43] = 0;
        assert_eq!(
            Spcr::from_bytes(&bytes).unwrap().ns16550_mmio_layout(),
            Some((4, 4))
        );
        bytes[41] = 8;
        bytes[43] = 3;
        assert_eq!(
            Spcr::from_bytes(&bytes).unwrap().ns16550_mmio_layout(),
            None
        );

        bytes[8] = 2;
        assert_eq!(
            Spcr::from_bytes(&bytes).unwrap().uart_clock_frequency(),
            None
        );

        bytes[36] = 1; // Full 16450 with a SystemMemory GAS.
        bytes[41] = 32;
        assert_eq!(
            Spcr::from_bytes(&bytes).unwrap().ns16550_mmio_layout(),
            Some((4, 4))
        );

        // Disabled redirection can leave the GAS access attributes populated.
        bytes[44..52].fill(0);
        assert!(Spcr::from_bytes(&bytes).unwrap().base_address().is_none());
    }

    #[test]
    fn revision_four_requires_the_extension_and_bounds_its_namespace() {
        use kernutil::acpi::{InvalidSpcrNamespace, Spcr};

        let mut bytes = [0; 100];
        bytes[..4].copy_from_slice(b"SPCR");
        bytes[4..8].copy_from_slice(&80_u32.to_le_bytes());
        bytes[8] = 4;
        assert!(Spcr::from_bytes(&bytes).is_none());

        bytes[4..8].copy_from_slice(&90_u32.to_le_bytes());
        bytes[80..84].copy_from_slice(&1_500_000_u32.to_le_bytes());
        bytes[84..86].copy_from_slice(&2_u16.to_le_bytes());
        bytes[86..88].copy_from_slice(&88_u16.to_le_bytes());
        bytes[88] = b'.';
        let spcr = Spcr::from_bytes(&bytes).unwrap();
        assert_eq!(spcr.baud_rate(), Some(1_500_000));
        assert_eq!(spcr.namespace_path(), Ok(None));

        bytes[84..86].copy_from_slice(&3_u16.to_le_bytes());
        assert_eq!(
            Spcr::from_bytes(&bytes).unwrap().namespace_path(),
            Err(InvalidSpcrNamespace)
        );

        bytes[4..8].copy_from_slice(&99_u32.to_le_bytes());
        bytes[84..86].copy_from_slice(&11_u16.to_le_bytes());
        bytes[88..97].copy_from_slice(br"\_SB.COM1");
        assert_eq!(
            Spcr::from_bytes(&bytes).unwrap().namespace_path(),
            Ok(Some(r"\_SB.COM1"))
        );
    }

    #[test]
    fn spcr_mmio_range_starts_at_the_gas_inside_the_uart_resource() {
        use super::{AcpiResourceRange, spcr_uart_range};
        let ranges = [
            AcpiResourceRange {
                base: 0x1000,
                size: 0x100,
            },
            AcpiResourceRange {
                base: 0x2000,
                size: 0x100,
            },
        ];
        let path = r"\_SB_.COM1";
        let firmware_path = r"\_SB.COM1";
        let expected = AcpiResourceRange {
            base: 0x2020,
            size: 0xe0,
        };
        assert!(matches!(
            spcr_uart_range(r"\_SB_.COM0", Some(path), &ranges, 0x2020),
            Some(Err(_))
        ));
        assert!(spcr_uart_range(r"\_SB_.COM0", Some(path), &ranges, 0x3000).is_none());
        assert_eq!(
            spcr_uart_range(path, Some(firmware_path), &ranges, 0x2020)
                .unwrap()
                .ok(),
            Some(expected)
        );
        assert_eq!(
            spcr_uart_range(path, None, &ranges, 0x2020).unwrap().ok(),
            Some(expected)
        );
        assert!(
            spcr_uart_range(path, Some(path), &ranges, 0x2100)
                .unwrap()
                .is_err()
        );
        assert!(spcr_uart_range(path, None, &ranges, 0x2100).is_none());
    }

    use alloc::{
        string::{String, ToString},
        sync::Arc,
        vec::Vec,
    };
    use core::str::FromStr;

    use acpi::{
        address::{AddressSpace, GenericAddress},
        aml::{
            Interpreter,
            namespace::{AmlName, NamespaceLevelKind},
            object::Object,
            pci_routing::IrqDescriptor,
            resource::{InterruptPolarity, InterruptTrigger, Irqs},
        },
        registers::{FixedRegisters, Pm1ControlRegisterBlock, Pm1EventRegisterBlock},
    };

    use super::{
        AcpiGsiController, AcpiHandler, AcpiId, AcpiIoApic, AcpiIrqPolarity, AcpiIrqTrigger,
        AcpiIsaIrqOverride, AcpiPciEcam, AcpiPciNamespace, AcpiPciRoot, AcpiResourceRange,
        AcpiRoot, AcpiRouting, LinkIrqResource, LinkIrqResourceKind, PciLinkAllocator, RawSpinLock,
        System, apply_pci_root_dma_coherency, inherited_device_cca,
        is_buffer_field_to_field_unit_store_gap, pci_irq_descriptor_gsi,
        pci_link_irq_field_candidates, route_with_irq_descriptor_flags, select_pci_link_irq,
    };
    use crate::register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority};

    #[expect(
        clippy::arc_with_non_send_sync,
        reason = "acpi::Interpreter requires Arc even for this single-threaded test handler"
    )]
    fn test_fixed_registers(handler: &AcpiHandler) -> Arc<FixedRegisters<AcpiHandler>> {
        let event_gas = GenericAddress {
            address_space: AddressSpace::SystemIo,
            bit_width: 16,
            bit_offset: 0,
            access_size: 2,
            address: 0x1000,
        };
        let event_enable_gas = GenericAddress {
            address: 0x1002,
            ..event_gas
        };
        let control_gas = GenericAddress {
            address_space: AddressSpace::SystemIo,
            bit_width: 16,
            bit_offset: 0,
            access_size: 2,
            address: 0x1004,
        };
        Arc::new(FixedRegisters {
            pm1_event_registers: Pm1EventRegisterBlock {
                pm1_event_length: 4,
                // SAFETY: These distinct 16-bit port ranges model the two
                // halves of the four-byte PM1 event block in this test handler.
                pm1a_sts: unsafe {
                    acpi::address::MappedGas::map_gas(event_gas, handler.clone()).unwrap()
                },
                pm1a_en: unsafe {
                    acpi::address::MappedGas::map_gas(event_enable_gas, handler.clone()).unwrap()
                },
                pm1b_sts: None,
                pm1b_en: None,
            },
            pm1_control_registers: Pm1ControlRegisterBlock {
                pm1a: unsafe {
                    acpi::address::MappedGas::map_gas(control_gas, handler.clone()).unwrap()
                },
                pm1b: None,
            },
        })
    }

    #[test]
    fn aml_tables_load_in_order_and_stop_on_error() {
        let table_bytes = |aml: &[u8]| {
            let mut bytes = vec![0; core::mem::size_of::<acpi::sdt::SdtHeader>()];
            bytes.extend_from_slice(aml);
            bytes
        };
        let first = table_bytes(&[0x08, b'O', b'U', b'T', b'0', 0x01]);
        let malformed = table_bytes(&[0xfe]);
        let last = table_bytes(&[0x08, b'L', b'A', b'T', b'E', 0x01]);
        let descriptor = |bytes: &[u8]| acpi::AmlTable {
            phys_address: bytes.as_ptr() as usize,
            length: bytes.len().try_into().unwrap(),
            revision: 2,
        };
        let handler = AcpiHandler::new(AcpiRoot::identity(0x1000), Vec::new());
        let interpreter =
            Interpreter::new(handler.clone(), 2, test_fixed_registers(&handler), None);
        let expected = interpreter.load_table(&[0xfe]).unwrap_err();
        // SAFETY: The initialized buffers are immutable and remain allocated
        // until after the interpreter drops. Identity mapping borrows them.
        let result = unsafe {
            super::load_aml_tables(
                &interpreter,
                &handler,
                [
                    descriptor(&first),
                    descriptor(&malformed),
                    descriptor(&last),
                ],
            )
        };
        assert!(matches!(
            result,
            Err(crate::error::DriverError::Unknown(message))
                if message == format!("{:?}", acpi::AcpiError::Aml(expected))
        ));
        let value = interpreter
            .evaluate(AmlName::from_str("\\OUT0").unwrap(), Vec::new())
            .unwrap();
        assert!(matches!(*value, Object::Integer(1)));
        assert!(
            interpreter
                .evaluate(AmlName::from_str("\\LATE").unwrap(), Vec::new())
                .is_err()
        );

        // A truncated header is rejected before mapping or executing its AML.
        let mut short = descriptor(&last);
        short.length = core::mem::size_of::<acpi::sdt::SdtHeader>() as u32 - 1;
        // SAFETY: `last` remains immutable and outlives the interpreter.
        assert!(matches!(
            unsafe { super::load_aml_tables(&interpreter, &handler, [short]) },
            Err(crate::error::DriverError::Unsupported(
                "invalid AML table range"
            ))
        ));
    }

    #[test]
    #[should_panic(expected = "Fatal AML error")]
    fn aml_fatal_stops_execution() {
        let handler = AcpiHandler::new(AcpiRoot::identity(0x1000), Vec::new());
        let interpreter =
            Interpreter::new(handler.clone(), 2, test_fixed_registers(&handler), None);
        // Fatal (type 1, code 2, argument One) must not inherit a log-only handler.
        let _ = interpreter.load_table(&[0x5b, 0x32, 0x01, 0x02, 0x00, 0x00, 0x00, 0x01]);
    }

    fn interpreter_with_devices(handler: AcpiHandler) -> Interpreter<AcpiHandler> {
        let interpreter =
            Interpreter::new(handler.clone(), 2, test_fixed_registers(&handler), None);
        {
            let mut namespace = interpreter.namespace.lock();
            let pnp = AmlName::from_str("\\_SB.RTC0").unwrap();
            namespace
                .add_level(pnp.clone(), NamespaceLevelKind::Device)
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str("_HID").unwrap().resolve(&pnp).unwrap(),
                    Object::Integer(0x000b_d041).wrap(),
                )
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str("_CRS").unwrap().resolve(&pnp).unwrap(),
                    Object::Buffer(Vec::from([
                        0x47, 0x01, 0x70, 0x00, 0x70, 0x00, 0x01, 0x08, 0x22, 0x00, 0x01, 0x79,
                        0x00,
                    ]))
                    .wrap(),
                )
                .unwrap();

            let loon = AmlName::from_str("\\_SB.LRTC").unwrap();
            namespace
                .add_level(loon.clone(), NamespaceLevelKind::Device)
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str("_HID").unwrap().resolve(&loon).unwrap(),
                    Object::String(String::from("LOON0001")).wrap(),
                )
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str("_CRS").unwrap().resolve(&loon).unwrap(),
                    Object::Buffer(Vec::from([
                        0x86, 0x09, 0x00, 0x01, 0x00, 0x01, 0x0d, 0x10, 0x00, 0x01, 0x00, 0x00,
                        0x79, 0x00,
                    ]))
                    .wrap(),
                )
                .unwrap();
        }
        interpreter
    }

    fn test_system() -> System {
        let handler = AcpiHandler::new(AcpiRoot::identity(0x1000), Vec::new());
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });
        System {
            ecam_regions: Vec::new(),
            routing,
            interpreter: Some(interpreter_with_devices(handler.clone())),
            handler,
            pci: None,
            probed_names: RawSpinLock::new(alloc::collections::BTreeSet::new()),
            serial_console_override: super::OnceLock::new(),
            populated_paths: RawSpinLock::new(alloc::collections::BTreeMap::new()),
            populated_resources: RawSpinLock::new(alloc::collections::BTreeMap::new()),
        }
    }

    static LAST_PATH: RawSpinLock<Option<String>> = RawSpinLock::new(None);

    fn probe_rtc(probe: super::ProbeAcpi<'_>) -> Result<(), crate::probe::OnProbeError> {
        let info = probe.info();
        assert_eq!(info.hid(), Some("PNP0B00"));
        assert_eq!(
            info.io_ranges(),
            &[AcpiResourceRange {
                base: 0x70,
                size: 8,
            }]
        );
        assert_eq!(
            info.irq_routes()
                .iter()
                .map(|route| route.gsi)
                .collect::<Vec<_>>(),
            Vec::from([8])
        );
        *LAST_PATH.lock() = Some(info.path.to_string());
        Ok(())
    }

    static RTC_REGISTER: DriverRegister = DriverRegister {
        name: "test acpi rtc",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: &[ProbeKind::Acpi {
            ids: &[AcpiId {
                hid: "PNP0B00",
                cids: &[],
            }],
            on_probe: probe_rtc,
        }],
    };

    #[test]
    fn acpi_probe_matches_namespace_device_and_exposes_io_and_irq_resources() {
        let system = test_system();

        let results = system.probe_register(&RTC_REGISTER).unwrap();

        assert_eq!(results.len(), 1);
        assert!(results.into_iter().all(|result| result.is_ok()));
        assert_eq!(LAST_PATH.lock().as_deref(), Some("\\_SB_.RTC0"));
    }

    #[test]
    fn disabled_namespace_device_is_skipped() {
        let system = test_system();
        let path = AmlName::from_str(r"\_SB.RTC0._STA").unwrap();
        system
            .interpreter
            .as_ref()
            .unwrap()
            .namespace
            .lock()
            .insert(path, Object::Integer(0).wrap())
            .unwrap();
        let ids = &[AcpiId {
            hid: "PNP0B00",
            cids: &[],
        }];
        assert!(system.device_infos_for_ids(ids).unwrap().is_empty());
    }

    #[test]
    fn invalid_namespace_device_status_does_not_hide_matching_sibling() {
        let system = test_system();
        {
            let mut namespace = system.interpreter.as_ref().unwrap().namespace.lock();
            namespace
                .add_level(
                    AmlName::from_str(r"\_SB.RTC1").unwrap(),
                    NamespaceLevelKind::Device,
                )
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str(r"\_SB.RTC1._HID").unwrap(),
                    Object::String(String::from("PNP0B00")).wrap(),
                )
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str(r"\_SB.RTC1._STA").unwrap(),
                    Object::Package(Vec::new()).wrap(),
                )
                .unwrap();
        }
        let devices = system
            .device_infos_for_ids(&[AcpiId {
                hid: "PNP0B00",
                cids: &[],
            }])
            .unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].path, r"\_SB_.RTC0");
    }

    #[test]
    fn acpi_unavailable_console_keeps_independent_uart_registered() {
        for gas_space in [0, 1] {
            for (override_address, namespace_offset) in [(false, 88), (true, 88), (false, 52)] {
                assert_unavailable_console_keeps_independent_uart_registered(
                    gas_space,
                    override_address,
                    namespace_offset,
                );
            }
        }
        // A reserved GAS address space cannot identify a namespace UART.
        assert_unavailable_console_keeps_independent_uart_registered(0x0b, false, 88);
    }

    #[test]
    fn acpi_host_serial_resource_covers_strided_registers() {
        for (bits, access, shift) in [(32, 1, 2), (64, 4, 3)] {
            let (console, register_shift) = parse_host_serial_gas(0, bits, access, 0x1000)
                .unwrap()
                .unwrap();
            assert_eq!(console.registers.base, 0x1000);
            assert_eq!(console.registers.size, 8 << shift);
            assert_eq!(console.access_size, access);
            assert_eq!(register_shift, shift);
        }
    }

    #[test]
    fn acpi_host_serial_rejects_unsupported_16550_io_port_layouts() {
        let (console, shift) = parse_host_serial_gas(1, 8, 1, 0xfff8).unwrap().unwrap();
        assert_eq!(console.registers.base, 0xfff8);
        assert_eq!(console.registers.size, 8);
        assert_eq!(shift, 0);
        assert!(parse_host_serial_gas(1, 8, 1, 0xfff9).is_err());
        assert!(parse_host_serial_gas(1, 16, 2, 0x3f8).is_err());
    }

    fn parse_host_serial_gas(
        address_space: u8,
        bit_width: u8,
        access_size: u8,
        address: u64,
    ) -> Result<Option<(super::AcpiSerialConsole, u8)>, crate::DriverError> {
        let mut body = [0; 44];
        body[0] = 0x12; // Generic 16550.
        body[4] = address_space;
        body[5] = bit_width;
        body[7] = access_size;
        body[8..16].copy_from_slice(&address.to_le_bytes());
        let mut spcr = sdt(b"SPCR", b"TESTOS", b"UARTTEST", &body);
        let table = &mut bytes(&mut spcr)[..36 + body.len()];
        table[8] = 2;
        checksum(table, 9);
        let xsdt = sdt(
            b"XSDT",
            b"TESTOS",
            b"UARTTEST",
            &(spcr.as_ptr() as u64).to_le_bytes(),
        );
        let rsdp = rsdp_for(&xsdt);
        let mut system = test_system();
        system.handler = AcpiHandler::new(root_with_spcr(&rsdp, &spcr), Vec::new());
        system.serial_console_with_register_shift()
    }

    fn assert_unavailable_console_keeps_independent_uart_registered(
        gas_space: u8,
        override_address: bool,
        namespace_offset: u16,
    ) {
        use crate::probe::OnProbeError;

        const UART_IDS: &[AcpiId] = &[AcpiId {
            hid: "PNP0501",
            cids: &[],
        }];

        fn probe_uart(probe: super::ProbeAcpi<'_>) -> Result<(), OnProbeError> {
            let info = probe.info();
            assert!(info.preferred_serial_registers().is_none());
            info.spcr_16550()?;
            let range = info.memory_ranges().first().ok_or(OnProbeError::NotMatch)?;
            let address = super::AcpiResourceAddress::memory(range.base);
            probe.register_device_at(address, crate::driver::Empty);
            Ok(())
        }

        crate::init(crate::Platform::Static).unwrap();
        let register = DriverRegister {
            name: "test ACPI UART fallback",
            level: ProbeLevel::PostKernel,
            priority: ProbePriority::DEFAULT,
            probe_kinds: &[
                ProbeKind::Acpi {
                    ids: UART_IDS,
                    on_probe: probe_uart,
                },
                ProbeKind::Acpi {
                    ids: &[],
                    on_probe: |probe| {
                        let root = probe.info().root;
                        let address = super::AcpiResourceAddress::io(0x3f8);
                        if root
                            .serial_fallback_allowed(address, UART_IDS)
                            .unwrap_or(false)
                        {
                            Ok(())
                        } else {
                            Err(OnProbeError::NotMatch)
                        }
                    },
                },
            ],
        };

        let absent = {
            let xsdt = sdt(b"XSDT", b"TESTOS", b"UARTTEST", &[]);
            let rsdp = rsdp_for(&xsdt);
            let mut system = test_system();
            system.handler =
                AcpiHandler::new(AcpiRoot::identity(rsdp.as_ptr() as usize), Vec::new());
            system.probe_register(&register).unwrap()
        };
        assert!(matches!(absent.as_slice(), [Ok(())]));

        let mut system = test_system();
        let interpreter = system.interpreter.as_ref().unwrap();
        {
            let mut namespace = interpreter.namespace.lock();
            for name in [r"\_SB.COM0", r"\_SB.COM1"] {
                let path = AmlName::from_str(name).unwrap();
                namespace
                    .add_level(path.clone(), NamespaceLevelKind::Device)
                    .unwrap();
                namespace
                    .insert(
                        AmlName::from_str("_HID").unwrap().resolve(&path).unwrap(),
                        Object::String(String::from("PNP0501")).wrap(),
                    )
                    .unwrap();
                if name.ends_with("COM1") {
                    namespace
                        .insert(
                            AmlName::from_str("_CRS").unwrap().resolve(&path).unwrap(),
                            Object::Buffer(alloc::vec![
                                0x86, 0x09, 0x00, 0x01, 0x00, 0x10, 0x00, 0x00, 0x00, 0x01, 0x00,
                                0x00, 0x79, 0x00,
                            ])
                            .wrap(),
                        )
                        .unwrap();
                }
            }
        }
        let namespace = b"\\_SB.COM0\0";
        let mut body = alloc::vec![0; 52 + namespace.len()];
        body[0] = 0x12; // Generic 16550.
        body[4] = gas_space;
        body[5] = 8; // GAS register width.
        body[7] = 1; // Byte access.
        body[8..16].copy_from_slice(&0x2000_u64.to_le_bytes());
        body[48..50].copy_from_slice(&(namespace.len() as u16).to_le_bytes());
        body[50..52].copy_from_slice(&namespace_offset.to_le_bytes());
        body[52..].copy_from_slice(namespace);
        let mut spcr = sdt(b"SPCR", b"TESTOS", b"UARTTEST", &body);
        let table = &mut bytes(&mut spcr)[..36 + body.len()];
        table[8] = 4;
        checksum(table, 9);
        let xsdt = sdt(
            b"XSDT",
            b"TESTOS",
            b"UARTTEST",
            &(spcr.as_ptr() as u64).to_le_bytes(),
        );
        let rsdp = rsdp_for(&xsdt);
        system.handler = AcpiHandler::new(root_with_spcr(&rsdp, &spcr), Vec::new());

        // Firmware cannot authorize the root fallback if the named UART is
        // unavailable and no namespace devices can be probed.
        let interpreter = system.interpreter.take();
        let results = system.probe_register(&register).unwrap();
        assert!(matches!(results.as_slice(), [Err(OnProbeError::NotMatch)]));
        system.interpreter = interpreter;

        let missing = super::AcpiResourceAddress::io(0x3e8);
        if override_address {
            system.prefer_serial_console(missing);
        }
        let results = system.probe_register(&register).unwrap();
        assert!(matches!(results.last(), Some(Ok(()))), "{results:?}");
        let independent = system.path_to_device_id(r"\_SB_.COM1").unwrap();
        assert_eq!(
            system.resource_address_to_device_id(super::AcpiResourceAddress::memory(0x1000)),
            Some(independent)
        );
        assert_eq!(system.resource_address_to_device_id(missing), None);
        assert_eq!(
            system.spcr_console_device_id_result(),
            Err(super::SpcrConsoleNotFound)
        );
    }

    #[test]
    fn fixed_serial_fallback_requires_no_available_namespace_uart() {
        let xsdt = sdt(b"XSDT", b"TESTOS", b"UARTTEST", &[]);
        let rsdp = rsdp_for(&xsdt);
        let mut system = test_system();
        system.handler = AcpiHandler::new(AcpiRoot::identity(rsdp.as_ptr() as usize), Vec::new());
        let address = super::AcpiResourceAddress::io(0x3f8);
        let ids = &[AcpiId {
            hid: "PNP0501",
            cids: &[],
        }];
        assert!(system.serial_fallback_allowed(address, ids).unwrap());

        let interpreter = system.interpreter.as_ref().unwrap();
        {
            let mut namespace = interpreter.namespace.lock();
            namespace
                .add_level(
                    AmlName::from_str(r"\_SB.COM0").unwrap(),
                    NamespaceLevelKind::Device,
                )
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str(r"\_SB.COM0._HID").unwrap(),
                    Object::String(String::from("PNP0501")).wrap(),
                )
                .unwrap();
        }
        assert!(!system.serial_fallback_allowed(address, ids).unwrap());

        interpreter
            .namespace
            .lock()
            .insert(
                AmlName::from_str(r"\_SB.COM0._STA").unwrap(),
                Object::Integer(0).wrap(),
            )
            .unwrap();
        assert!(system.serial_fallback_allowed(address, ids).unwrap());
    }

    #[test]
    fn acpi_probes_spcr_uart_before_an_unrelated_uart() {
        assert_acpi_probes_selected_uart(0x12, [0, 32, 0, 3], 0x2000, None, Some((4, 4)));
        assert_acpi_probes_selected_uart(0x12, [0, 64, 0, 3], 0x2000, None, Some((8, 4)));
        assert_acpi_probes_selected_uart(0x12, [1, 8, 0, 0], 0x3f8, None, None);
        assert_acpi_probes_selected_uart(0x01, [1, 8, 0, 0], 0x3f8, None, None);
        assert_acpi_probes_selected_uart(
            0x12,
            [1, 8, 0, 0],
            0x3f8,
            Some(super::AcpiResourceAddress::io(0x2f8)),
            None,
        );
        assert_acpi_probes_selected_uart(
            0x12,
            [0, 32, 1, 3],
            0x2000,
            Some(super::AcpiResourceAddress::memory(0x2000)),
            None,
        );
    }

    #[test]
    fn acpi_uart_keeps_its_spcr_layout_after_the_explicit_console_fails() {
        assert_acpi_probes_selected_uart(
            0x12,
            [0, 64, 0, 3],
            0x2000,
            Some(super::AcpiResourceAddress::memory(0x1000)),
            Some((8, 4)),
        );
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn explicit_mmio_console_retains_the_spcr_qword_layout() {
        assert_acpi_probes_selected_uart(
            0x12,
            [0, 64, 0, 4],
            0x2000,
            Some(super::AcpiResourceAddress::memory(0x2000)),
            Some((8, 8)),
        );
    }

    fn assert_acpi_probes_selected_uart(
        interface: u8,
        gas_attributes: [u8; 4],
        uart_base: u64,
        explicit: Option<super::AcpiResourceAddress>,
        expected_mmio_layout: Option<(usize, usize)>,
    ) {
        use crate::probe::OnProbeError;

        crate::init(crate::Platform::Static).unwrap();
        let mut system = test_system();
        let explicit_io =
            explicit.is_some_and(|address| address.space == super::AcpiResourceAddressSpace::Io);
        let interpreter = system.interpreter.as_ref().unwrap();
        {
            let mut namespace = interpreter.namespace.lock();
            for (name, base) in [(r"\_SB.COM0", 0x1000_u32), (r"\_SB.COM1", 0x2000)] {
                let path = AmlName::from_str(name).unwrap();
                namespace
                    .add_level(path.clone(), NamespaceLevelKind::Device)
                    .unwrap();
                namespace
                    .insert(
                        AmlName::from_str("_HID").unwrap().resolve(&path).unwrap(),
                        Object::String(String::from(if interface == 0x01 {
                            "PNP0500"
                        } else {
                            "PNP0501"
                        }))
                        .wrap(),
                    )
                    .unwrap();
                let mut crs = alloc::vec![0x86, 0x09, 0x00, 0x01];
                crs.extend_from_slice(&base.to_le_bytes());
                let size = if base == 0x1000 { 4_u32 } else { 0x100 };
                crs.extend_from_slice(&size.to_le_bytes());
                let ports: &[u16] = match (name.ends_with("COM1"), explicit_io) {
                    (true, false) => &[0x2f8, 0x3f8],
                    (true, true) => &[0x2f8],
                    (false, true) => &[0x3f8],
                    (false, false) => &[],
                };
                for &port in ports {
                    let (start, size) = if port == 0x3f8 && !explicit_io {
                        (0x3f0_u16, 16)
                    } else {
                        (port, 8)
                    };
                    let [low, high] = start.to_le_bytes();
                    crs.extend_from_slice(&[0x47, 1, low, high, low, high, 1, size]);
                }
                crs.extend_from_slice(&[0x79, 0]);
                namespace
                    .insert(
                        AmlName::from_str("_CRS").unwrap().resolve(&path).unwrap(),
                        Object::Buffer(crs).wrap(),
                    )
                    .unwrap();
            }
        }

        let namespace = if explicit_io {
            b"\\_SB.COM0\0"
        } else {
            b"\\_SB.COM1\0"
        };
        let mut body = alloc::vec![0; 52 + namespace.len()];
        body[0] = interface;
        // GAS address space, register bit width, bit offset, and access size.
        body[4..8].copy_from_slice(&gas_attributes);
        body[8..16].copy_from_slice(&uart_base.to_le_bytes());
        body[48..50].copy_from_slice(&(namespace.len() as u16).to_le_bytes());
        body[50..52].copy_from_slice(&88_u16.to_le_bytes());
        body[52..].copy_from_slice(namespace);
        let mut spcr = sdt(b"SPCR", b"TESTOS", b"UARTTEST", &body);
        let table = &mut bytes(&mut spcr)[..36 + body.len()];
        table[8] = 4;
        checksum(table, 9);
        let xsdt = sdt(
            b"XSDT",
            b"TESTOS",
            b"UARTTEST",
            &(spcr.as_ptr() as u64).to_le_bytes(),
        );
        let rsdp = rsdp_for(&xsdt);
        system.handler = AcpiHandler::new(root_with_spcr(&rsdp, &spcr), Vec::new());

        if let Some(address) = explicit {
            system.prefer_serial_console(address);
        }

        const UART_IDS: &[AcpiId] = &[
            AcpiId {
                hid: "PNP0501",
                cids: &[],
            },
            AcpiId {
                hid: "PNP0500",
                cids: &[],
            },
        ];
        let devices = system.device_infos_for_ids(UART_IDS).unwrap();
        let selected = devices
            .iter()
            .find(|device| device.path == r"\_SB_.COM1")
            .unwrap();
        let selected_info = super::AcpiInfo {
            root: &system,
            path: &selected.path,
            irq_route: None,
            device: Some(selected),
        };
        let actual_mmio_layout = match selected_info.spcr_16550().unwrap() {
            Some(super::Spcr16550::Mmio(settings)) => {
                Some((settings.register_stride, settings.access_width))
            }
            _ => None,
        };
        assert_eq!(actual_mmio_layout, expected_mmio_layout);

        let register = DriverRegister {
            name: "test ACPI SPCR UART",
            level: ProbeLevel::PostKernel,
            priority: ProbePriority::DEFAULT,
            probe_kinds: &[ProbeKind::Acpi {
                ids: UART_IDS,
                on_probe: |probe| {
                    let info = probe.info();
                    let explicit = info.preferred_serial_registers();
                    let binds = match (info.spcr_16550()?, explicit) {
                        (Some(super::Spcr16550::Mmio(settings)), _) => {
                            assert_eq!(settings.registers.base, 0x2000);
                            super::AcpiResourceAddress::memory(settings.registers.base)
                        }
                        (Some(super::Spcr16550::Io { registers, .. }), None) => {
                            assert_eq!(registers.base, 0x3f8);
                            assert_eq!(registers.size, 8);
                            super::AcpiResourceAddress::io(registers.base)
                        }
                        (None, Some((super::AcpiResourceAddressSpace::Memory, registers)))
                            if registers.size < 8 =>
                        {
                            return Err(OnProbeError::NotMatch);
                        }
                        (None, Some((space, registers))) => super::AcpiResourceAddress {
                            space,
                            base: registers.base,
                        },
                        other => panic!("unexpected console resources: {other:?}"),
                    };
                    probe.register_device_at(binds, crate::driver::Empty);
                    Ok(())
                },
            }],
        };
        let bound_address = if expected_mmio_layout.is_some() {
            super::AcpiResourceAddress::memory(uart_base)
        } else {
            explicit.unwrap_or_else(|| super::AcpiResourceAddress::io(uart_base))
        };
        let results = system.probe_register(&register).unwrap();
        if explicit.is_some_and(|address| address != bound_address) {
            assert!(
                matches!(results.as_slice(), [Err(OnProbeError::NotMatch), Ok(())]),
                "{results:?}"
            );
        } else {
            assert!(matches!(results.as_slice(), [Ok(())]), "{results:?}");
        }
        let selected = system
            .path_to_device_id(r"\_SB_.COM1")
            .expect("selected UART probed");
        assert_eq!(
            system.resource_address_to_device_id(bound_address),
            Some(selected)
        );
        if let Some(address) = explicit.filter(|&address| address != bound_address) {
            assert_eq!(system.resource_address_to_device_id(address), None);
        }
        if explicit_io {
            assert_eq!(
                system.spcr_console_device_id_result(),
                Err(super::SpcrConsoleNotFound)
            );
        } else if explicit.is_none() {
            assert_eq!(system.spcr_console_device_id_result(), Ok(Some(selected)));
            assert_eq!(
                system.resource_address_to_device_id(super::AcpiResourceAddress::io(0x2f8)),
                None
            );
        }
    }

    #[test]
    fn acpi_info_exposes_loongson_memory_resource() {
        let system = test_system();
        let info = system
            .device_infos_for_ids(&[AcpiId {
                hid: "LOON0001",
                cids: &[],
            }])
            .unwrap()
            .into_iter()
            .next()
            .expect("LOON0001 device should be discovered");

        assert_eq!(info.hid.as_deref(), Some("LOON0001"));
        assert_eq!(
            info.memory_ranges.as_slice(),
            &[AcpiResourceRange {
                base: 0x100d_0100,
                size: 0x100,
            }]
        );
    }

    #[test]
    fn acpi_resources_preserve_irq_lists_and_resource_flags() {
        let mut system = test_system();
        super::parse_loongarch_interrupt_routing(&loongarch_madt(), &mut system.routing).unwrap();
        let interpreter = system.interpreter.as_ref().unwrap();
        let path = AmlName::from_str("\\_SB.IRQ0").unwrap();
        // FixedMemory32, five Extended IRQ numbers, one UART IRQ, Small IRQ
        // {4, 10}, and EndTag. The list order is deliberately not sorted.
        let mut bytes = Vec::from([
            0x86, 0x09, 0x00, 0x01, 0x00, 0x40, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x89, 0x16,
            0x00, 0x01, 0x05,
        ]);
        for irq in [124u32, 125, 126, 127, 123] {
            bytes.extend_from_slice(&irq.to_le_bytes());
        }
        bytes.extend_from_slice(&[
            0x89, 0x06, 0x00, 0x09, 0x01, 26, 0, 0, 0, 0x23, 0x10, 0x04, 0x01, 0x79, 0x00,
        ]);
        {
            let mut namespace = interpreter.namespace.lock();
            namespace
                .add_level(path.clone(), NamespaceLevelKind::Device)
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str("_CRS").unwrap().resolve(&path).unwrap(),
                    Object::Buffer(bytes).wrap(),
                )
                .unwrap();
        }
        let resources = super::read_device_resources(interpreter, &path, &system.routing).unwrap();
        assert_eq!(
            resources.memory_ranges,
            [AcpiResourceRange {
                base: 0x4000,
                size: 8
            }]
        );
        assert_eq!(
            resources
                .irq_routes
                .iter()
                .map(|route| route.gsi)
                .collect::<Vec<_>>(),
            [124, 125, 126, 127, 123, 26, 4, 10]
        );
        for route in &resources.irq_routes[..6] {
            assert_eq!(route.trigger, AcpiIrqTrigger::Level);
            assert_eq!(route.polarity, AcpiIrqPolarity::ActiveHigh);
        }
        let uart = resources.irq_routes[5];
        assert_eq!(uart.controller, AcpiGsiController::LioPic);
        assert_eq!(uart.controller_address, 0x2000);
        assert_eq!(uart.controller_input, 10);
        assert_eq!(uart.vector, 2);
        for route in &resources.irq_routes[6..] {
            assert_eq!(route.trigger, AcpiIrqTrigger::Edge);
            assert_eq!(route.polarity, AcpiIrqPolarity::ActiveHigh);
        }
    }

    #[test]
    fn acpi_eisa_integer_ids_decode_to_hid_strings() {
        assert_eq!(
            super::decode_eisa_id(0x000b_d041).as_deref(),
            Some("PNP0B00")
        );
        assert_eq!(
            super::decode_eisa_id(0x030a_d041).as_deref(),
            Some("PNP0A03")
        );
        assert_eq!(
            super::decode_eisa_id(0x080a_d041).as_deref(),
            Some("PNP0A08")
        );
    }

    #[test]
    fn acpi_ecam_coherency_comes_from_matching_pci_root_cca() {
        let mut regions = [AcpiPciEcam {
            segment_group: 0,
            bus_start: 0,
            bus_end: 0xff,
            base_address: 0x3000_0000,
            layout: super::ConfigSpaceLayout::Ecam,
            dma_coherent: None,
        }];
        let pci = AcpiPciNamespace {
            link_allocator: RawSpinLock::new(PciLinkAllocator::default()),
            roots: vec![AcpiPciRoot {
                segment: 0,
                bus: 0,
                path: String::from("\\\\_SB.PCI0"),
                dma_coherent: Some(true),
                prt: None,
                link_prt: None,
            }],
        };

        apply_pci_root_dma_coherency(&mut regions, &pci).unwrap();

        assert_eq!(regions[0].dma_coherent, Some(true));
    }

    #[test]
    fn acpi_ecam_rejects_conflicting_root_cca_values() {
        let mut regions = [AcpiPciEcam {
            segment_group: 0,
            bus_start: 0,
            bus_end: 0xff,
            base_address: 0x3000_0000,
            layout: super::ConfigSpaceLayout::Ecam,
            dma_coherent: None,
        }];
        let root = |bus, dma_coherent| AcpiPciRoot {
            segment: 0,
            bus,
            path: format!("\\\\_SB.PC{bus:02x}"),
            dma_coherent: Some(dma_coherent),
            prt: None,
            link_prt: None,
        };
        let pci = AcpiPciNamespace {
            link_allocator: RawSpinLock::new(PciLinkAllocator::default()),
            roots: vec![root(0, true), root(0x80, false)],
        };

        assert!(apply_pci_root_dma_coherency(&mut regions, &pci).is_err());
    }

    #[test]
    fn acpi_cca_uses_first_declaring_device_ancestor() {
        let handler = AcpiHandler::new(AcpiRoot::identity(0x1000), Vec::new());
        let interpreter =
            Interpreter::new(handler.clone(), 2, test_fixed_registers(&handler), None);
        let parent = AmlName::from_str("\\_SB.DEV0").unwrap();
        let child = AmlName::from_str("\\_SB.DEV0.PCI0").unwrap();
        {
            let mut namespace = interpreter.namespace.lock();
            namespace
                .add_level(parent.clone(), NamespaceLevelKind::Device)
                .unwrap();
            namespace
                .add_level(child.clone(), NamespaceLevelKind::Device)
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str("_CCA").unwrap().resolve(&parent).unwrap(),
                    Object::Integer(0).wrap(),
                )
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str("_CCA").unwrap().resolve(&child).unwrap(),
                    Object::Integer(1).wrap(),
                )
                .unwrap();
        }

        assert_eq!(
            inherited_device_cca(&interpreter, &child, &[parent, child.clone()]),
            Some(false)
        );
    }

    #[test]
    fn acpi_cca_without_a_declaring_device_remains_unspecified() {
        let handler = AcpiHandler::new(AcpiRoot::identity(0x1000), Vec::new());
        let interpreter =
            Interpreter::new(handler.clone(), 2, test_fixed_registers(&handler), None);
        let device = AmlName::from_str("\\_SB.DEV0").unwrap();
        interpreter
            .namespace
            .lock()
            .add_level(device.clone(), NamespaceLevelKind::Device)
            .unwrap();

        assert_eq!(
            inherited_device_cca(&interpreter, &device, core::slice::from_ref(&device)),
            None
        );
    }

    #[test]
    fn ioapic_routes_map_gsi_to_stable_vector() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });

        let legacy_irq = routing
            .resolve_gsi(4)
            .expect("legacy ISA GSI 4 should be handled by the IOAPIC");
        assert_eq!(legacy_irq.vector, 0x34);
        assert_eq!(legacy_irq.controller_input, 4);
        assert_eq!(legacy_irq.trigger, AcpiIrqTrigger::Edge);
        assert_eq!(legacy_irq.polarity, AcpiIrqPolarity::ActiveHigh);

        let irq = routing
            .resolve_gsi(16)
            .expect("gsi 16 should be handled by the IOAPIC");
        assert_eq!(irq.gsi, 16);
        assert_eq!(irq.controller, AcpiGsiController::IoApic);
        assert_eq!(irq.controller_id, 0);
        assert_eq!(irq.controller_address, 0xfec0_0000);
        assert_eq!(irq.controller_input, 16);
        assert_eq!(irq.vector, 0x40);
        assert_eq!(irq.trigger, AcpiIrqTrigger::Level);
        assert_eq!(irq.polarity, AcpiIrqPolarity::ActiveLow);
        assert!(routing.resolve_gsi(24).is_none());
    }

    #[test]
    fn pci_link_irq_can_route_to_legacy_ioapic_gsi() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });

        let descriptor = IrqDescriptor {
            is_consumer: false,
            trigger: InterruptTrigger::Level,
            polarity: InterruptPolarity::ActiveLow,
            is_shared: true,
            is_wake_capable: false,
            irqs: Irqs::from_buf([10]),
        };
        let gsi = pci_irq_descriptor_gsi(&descriptor).unwrap();

        assert_eq!(gsi, 10);
        let route = routing
            .resolve_gsi(gsi)
            .expect("IOAPIC routes the legacy GSI from the ACPI PCI link");
        assert_eq!(route.gsi, 10);
        assert_eq!(route.controller_input, 10);
    }

    #[test]
    fn pci_link_descriptor_reports_selected_power_of_two_gsi_directly() {
        let resource = LinkIrqResource {
            kind: LinkIrqResourceKind::SmallIrq,
            descriptor: IrqDescriptor {
                is_consumer: false,
                trigger: InterruptTrigger::Level,
                polarity: InterruptPolarity::ActiveLow,
                is_shared: true,
                is_wake_capable: false,
                irqs: Irqs::from_buf([4]),
            },
        };

        let descriptor = resource.descriptor_for_irq(4);
        assert_eq!(descriptor.irqs.as_slice(), &[4]);
        assert_eq!(pci_irq_descriptor_gsi(&descriptor), Some(4));
    }

    fn loongarch_madt() -> Vec<u8> {
        // Minimal synthetic MADT: LIO + EIO + BIO. Addresses are arbitrary.
        let mut bytes = vec![0; super::RAW_MADT_HEADER_LEN];
        bytes.extend_from_slice(&[18, 23, 1]);
        bytes.extend_from_slice(&0x2000u64.to_le_bytes());
        bytes.extend_from_slice(&0x80u16.to_le_bytes());
        bytes.extend_from_slice(&[2, 0]);
        bytes.extend_from_slice(&(1u32 << 10).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&[20, 13, 1, 3, 0]);
        bytes.extend_from_slice(&3u64.to_le_bytes());
        bytes.extend_from_slice(&[22, 17, 1]);
        bytes.extend_from_slice(&0x3000u64.to_le_bytes());
        bytes.extend_from_slice(&0x1000u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&64u16.to_le_bytes());
        bytes[..4].copy_from_slice(b"APIC");
        let length = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        bytes[8] = 5;
        bytes[9] = 0u8.wrapping_sub(bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
        bytes
    }

    #[test]
    fn loongarch_madt_routes_only_explicit_lio_inputs_and_preserves_pch() {
        let mut routing = AcpiRouting::new();
        let mut bytes = loongarch_madt();
        super::parse_loongarch_interrupt_routing(&bytes, &mut routing).unwrap();
        assert_eq!(
            routing.resolve_gsi(26),
            Some(super::AcpiGsiRoute {
                gsi: 26,
                vector: 2,
                controller: AcpiGsiController::LioPic,
                controller_id: 0,
                controller_address: 0x2000,
                controller_input: 10,
                trigger: AcpiIrqTrigger::Level,
                polarity: AcpiIrqPolarity::ActiveHigh,
            })
        );
        for gsi in [16, 25, 27, 47, 48, 63] {
            assert!(routing.resolve_gsi(gsi).is_none(), "unwired GSI {gsi}");
        }
        assert!(routing.resolve_vector(2).is_none());
        assert!(
            routing
                .resolve_vector(super::PCI_INTX_VECTOR_BASE + 26)
                .is_none()
        );

        let route = routing
            .resolve_gsi(82)
            .expect("GSI 82 should be covered by the PCH-PIC");
        assert_eq!(route.controller, AcpiGsiController::PchPic);
        assert_eq!(route.controller_id, 1);
        assert_eq!(route.controller_address, 0x3000);
        assert_eq!(route.controller_input, 18);
        assert_eq!(route.vector, 0x30 + 82);
        assert_eq!(routing.resolve_vector(route.vector), Some(route));
        assert!(routing.resolve_gsi(320).is_none());
        assert_eq!(
            routing.eio_pics(),
            &[super::AcpiEioPic {
                version: 1,
                cascade: 3,
                node: 0,
                node_map: 3,
            }]
        );

        // A second active cascade routes the last real input, not GSI 63.
        let lio = super::RAW_MADT_HEADER_LEN;
        bytes[lio + 14] = 4;
        bytes[lio + 19..lio + 23].copy_from_slice(&(1u32 << 31).to_le_bytes());
        let mut routing = AcpiRouting::new();
        super::parse_loongarch_interrupt_routing(&bytes, &mut routing).unwrap();
        let route = routing.resolve_gsi(47).unwrap();
        assert_eq!(route.controller_input, 31);
        assert_eq!(route.vector, 4);
        assert!(routing.resolve_gsi(48).is_none());
        assert!(routing.resolve_gsi(63).is_none());

        // Firmware may list an input in both maps. The first parent owns it;
        // the second parent still routes its remaining, disjoint inputs.
        bytes[lio + 19..lio + 23].copy_from_slice(&((1u32 << 10) | (1u32 << 31)).to_le_bytes());
        let mut routing = AcpiRouting::new();
        super::parse_loongarch_interrupt_routing(&bytes, &mut routing).unwrap();
        assert_eq!(routing.resolve_gsi(26).unwrap().vector, 2);
        assert_eq!(routing.resolve_gsi(47).unwrap().vector, 4);
    }

    #[test]
    fn loongarch_madt_rejects_invalid_lio_wiring() {
        let original = loongarch_madt();
        let lio = super::RAW_MADT_HEADER_LEN;
        // Mutate complete wire entries, then enter the same bounded scanner.
        for (offset, replacement) in [
            (2, &[2][..]),                          // unsupported version
            (3, &[0; 8][..]),                       // zero address
            (3, &[0xff; 8][..]),                    // address overflow
            (11, &[0x43, 0][..]),                   // core 0 ISR is outside MMIO
            (13, &[1][..]),                         // active parent is not HW0..HW3
            (15, &[0; 8][..]),                      // no input wired
            (19, &[0, 8, 0, 0][..]),                // active second map has no parent
            (14, &[2, 0, 4, 0, 0, 0, 8, 0, 0][..]), // duplicate active parent
        ] {
            let mut bytes = original.clone();
            bytes[lio + offset..lio + offset + replacement.len()].copy_from_slice(replacement);
            assert!(
                super::parse_loongarch_interrupt_routing(&bytes, &mut AcpiRouting::new()).is_err(),
                "accepted invalid LIO field at offset {offset}"
            );
        }
        let mut duplicate = original.clone();
        duplicate.extend_from_slice(&original[lio..lio + 23]);
        assert!(
            super::parse_loongarch_interrupt_routing(&duplicate, &mut AcpiRouting::new()).is_err()
        );

        let mut short = original[..lio + 22].to_vec();
        short[lio + 1] = 22;
        assert!(super::parse_loongarch_interrupt_routing(&short, &mut AcpiRouting::new()).is_err());
        let truncated = &original[..lio + 22];
        assert!(
            super::parse_loongarch_interrupt_routing(truncated, &mut AcpiRouting::new()).is_err()
        );
    }

    #[test]
    fn ioapic_routes_apply_isa_interrupt_source_overrides() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });
        routing.add_isa_irq_override(AcpiIsaIrqOverride {
            source: 0,
            gsi: 2,
            trigger: AcpiIrqTrigger::Level,
            polarity: AcpiIrqPolarity::ActiveLow,
        });

        let route = routing
            .resolve_gsi(2)
            .expect("overridden ISA GSI should still route through the IOAPIC");
        assert_eq!(route.vector, 0x32);
        assert_eq!(route.controller_input, 2);
        assert_eq!(route.trigger, AcpiIrqTrigger::Level);
        assert_eq!(route.polarity, AcpiIrqPolarity::ActiveLow);
    }

    #[test]
    fn isa_irq_resolution_uses_legacy_gsi_without_an_override() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });

        let route = routing.resolve_isa_irq(4).unwrap();

        assert_eq!(route.gsi, 4);
        assert_eq!(route.controller_input, 4);
        assert_eq!(route.trigger, AcpiIrqTrigger::Edge);
        assert_eq!(route.polarity, AcpiIrqPolarity::ActiveHigh);
    }

    #[test]
    fn isa_irq_resolution_applies_source_override_before_selecting_ioapic_input() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });
        routing.add_isa_irq_override(AcpiIsaIrqOverride {
            source: 4,
            gsi: 18,
            trigger: AcpiIrqTrigger::Level,
            polarity: AcpiIrqPolarity::ActiveLow,
        });

        let route = routing.resolve_isa_irq(4).unwrap();

        assert_eq!(route.gsi, 18);
        assert_eq!(route.controller_input, 18);
        assert_eq!(route.trigger, AcpiIrqTrigger::Level);
        assert_eq!(route.polarity, AcpiIrqPolarity::ActiveLow);
    }

    #[test]
    fn pci_link_irq_selects_prs_when_crs_is_unassigned() {
        let current = LinkIrqResource {
            kind: LinkIrqResourceKind::ExtendedIrq,
            descriptor: IrqDescriptor {
                is_consumer: true,
                trigger: InterruptTrigger::Level,
                polarity: InterruptPolarity::ActiveHigh,
                is_shared: true,
                is_wake_capable: false,
                irqs: Irqs::from_buf([0]),
            },
        };
        let possible = LinkIrqResource {
            kind: LinkIrqResourceKind::ExtendedIrq,
            descriptor: IrqDescriptor {
                is_consumer: true,
                trigger: InterruptTrigger::Level,
                polarity: InterruptPolarity::ActiveHigh,
                is_shared: true,
                is_wake_capable: false,
                irqs: [5, 10, 11].into_iter().collect(),
            },
        };

        let link = "\\_SB.LNKA".parse().unwrap();
        let allocator = PciLinkAllocator::default();
        let selection = select_pci_link_irq(&link, Some(&current), Some(&possible), &allocator)
            .expect("possible IRQs should allocate a PCI link");

        assert_eq!(selection.irq, 10);
        assert!(selection.needs_programming);
        assert_eq!(selection.resource.descriptor.irqs.as_slice(), &[5, 10, 11]);
        // A candidate list in _CRS is not an already selected interrupt.
        assert!(select_pci_link_irq(&link, Some(&possible), None, &allocator).is_none());
        let selection =
            select_pci_link_irq(&link, Some(&possible), Some(&possible), &allocator).unwrap();
        assert!(selection.needs_programming);
        assert_eq!(selection.irq, 10);
        assert_eq!(
            selection
                .resource
                .descriptor_for_irq(selection.irq)
                .irqs
                .as_slice(),
            &[10]
        );
    }

    #[test]
    fn pci_link_irq_allocator_spreads_unassigned_links() {
        let possible = LinkIrqResource {
            kind: LinkIrqResourceKind::ExtendedIrq,
            descriptor: IrqDescriptor {
                is_consumer: true,
                trigger: InterruptTrigger::Level,
                polarity: InterruptPolarity::ActiveHigh,
                is_shared: true,
                is_wake_capable: false,
                irqs: [5, 10, 11].into_iter().collect(),
            },
        };
        let first = "\\_SB.LNKA".parse().unwrap();
        let second = "\\_SB.LNKB".parse().unwrap();
        let mut allocator = PciLinkAllocator::default();

        let first_selection = select_pci_link_irq(&first, None, Some(&possible), &allocator)
            .expect("first link should allocate");
        assert_eq!(first_selection.irq, 10);
        allocator.commit(&first, first_selection.irq);

        let second_selection = select_pci_link_irq(&second, None, Some(&possible), &allocator)
            .expect("second link should allocate");
        assert_eq!(second_selection.irq, 11);
    }

    #[test]
    fn pci_link_sources_resolve_package_names_and_preserve_irq_allocation() {
        let handler = AcpiHandler::new(AcpiRoot::identity(0x1000), Vec::new());
        let interpreter = interpreter_with_devices(handler.clone());
        let root = AmlName::from_str("\\_SB.PCI0").unwrap();
        let prt_path = AmlName::from_str("_PRT").unwrap().resolve(&root).unwrap();
        let link = AmlName::from_str("\\_SB.LNKA").unwrap();
        {
            let mut namespace = interpreter.namespace.lock();
            namespace
                .add_level(root.clone(), NamespaceLevelKind::Device)
                .unwrap();
            namespace
                .add_level(link.clone(), NamespaceLevelKind::Device)
                .unwrap();
            let shadow = AmlName::from_str("LNKA").unwrap().resolve(&root).unwrap();
            namespace
                .add_level(shadow.clone(), NamespaceLevelKind::Device)
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str("_CRS").unwrap().resolve(&shadow).unwrap(),
                    Object::Buffer(Vec::from([0x23, 0x00, 0x08, 0x18, 0x79, 0x00])).wrap(),
                )
                .unwrap();
            // Both NamePath's original scope and the legacy string form must
            // find the outer link despite a different link shadowing it in PCI0.
            let sources = [
                Object::NamePath {
                    name: AmlName::from_str("LNKA").unwrap(),
                    scope: AmlName::from_str("\\_SB").unwrap(),
                },
                Object::String(String::from("\\_SB.LNKA")),
            ];
            let entries = sources
                .into_iter()
                .enumerate()
                .map(|(device, source)| {
                    Object::Package(vec![
                        Object::Integer((device as u64) << 16 | 0xffff).wrap(),
                        Object::Integer(0).wrap(),
                        source.wrap(),
                        Object::Integer(0).wrap(),
                    ])
                    .wrap()
                })
                .collect();
            namespace
                .insert(prt_path.clone(), Object::Package(entries).wrap())
                .unwrap();
            // A selected Small IRQ {4} must stay the number 4 across routing.
            namespace
                .insert(
                    AmlName::from_str("_CRS").unwrap().resolve(&link).unwrap(),
                    Object::Buffer(Vec::from([0x23, 0x10, 0x00, 0x18, 0x79, 0x00])).wrap(),
                )
                .unwrap();
        }
        let table = super::PciLinkRoutingTable::from_prt_path(prt_path, &interpreter).unwrap();
        let mut allocator = PciLinkAllocator::default();
        for device in [0, 1] {
            let descriptor = table
                .route(
                    device,
                    0,
                    acpi::aml::pci_routing::Pin::IntA,
                    &interpreter,
                    &handler,
                    &mut allocator,
                )
                .unwrap()
                .unwrap();
            assert_eq!(pci_irq_descriptor_gsi(&descriptor), Some(4));
            assert_eq!(descriptor.trigger, InterruptTrigger::Level);
            assert_eq!(descriptor.polarity, InterruptPolarity::ActiveLow);
            assert!(descriptor.is_shared);
        }
    }

    #[test]
    fn pci_link_srs_gap_detection_is_narrow() {
        let gap = acpi::aml::AmlError::ObjectNotOfExpectedType {
            expected: acpi::aml::object::ObjectType::Integer,
            got: acpi::aml::object::ObjectType::BufferField,
        };
        let other = acpi::aml::AmlError::ObjectNotOfExpectedType {
            expected: acpi::aml::object::ObjectType::Buffer,
            got: acpi::aml::object::ObjectType::BufferField,
        };

        assert!(is_buffer_field_to_field_unit_store_gap(&gap));
        assert!(!is_buffer_field_to_field_unit_store_gap(&other));
    }

    #[test]
    fn qemu_pci_link_names_map_to_pirq_fields() {
        assert_eq!(
            pci_link_irq_field_candidates(&"\\_SB.LNKB".parse().unwrap()),
            ["\\_SB.PRQB", "\\_SB.PRQ1"]
        );
        assert_eq!(
            pci_link_irq_field_candidates(&"\\_SB.LNKG".parse().unwrap()),
            ["\\_SB.PRQG"]
        );
        assert!(pci_link_irq_field_candidates(&"\\_SB.LNKS".parse().unwrap()).is_empty());
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
            irqs: Irqs::from_buf([16]),
        };

        let route = route_with_irq_descriptor_flags(route, &descriptor);

        assert_eq!(route.trigger, AcpiIrqTrigger::Edge);
        assert_eq!(route.polarity, AcpiIrqPolarity::ActiveHigh);
    }
}
