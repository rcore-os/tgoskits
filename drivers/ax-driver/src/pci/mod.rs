use alloc::format;

use ax_kspin::SpinRaw as Mutex;
use heapless::Vec as ArrayVec;
use mmio_api::MmioOp;
#[cfg(any(test, virtio_dev))]
use pcie::CommandRegister;
#[cfg(virtio_dev)]
use rdrive::probe::pci::{Endpoint, EndpointRc};
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        pci::{PciAddress, PciInfo, PciMem32, PciMem64},
    },
};
#[cfg(virtio_dev)]
use virtio_drivers::transport::{
    DeviceType,
    pci::{
        PciTransport,
        bus::{ConfigurationAccess, DeviceFunction, DeviceFunctionInfo, HeaderType, PciRoot},
        virtio_device_type,
    },
};

use crate::BindingIrq;
#[cfg(virtio_dev)]
use crate::virtio::{VirtIoHalImpl, VirtIoTransport};

mod acpi;
mod fdt;
mod intx;
pub mod msi;
pub(crate) use acpi::acpi_irq_for_endpoint;
pub(crate) use fdt::fdt_irq_for_endpoint;
#[cfg(virtio_dev)]
use intx::SharedPciEndpoint;
pub use intx::{PciIntxIrqLease, PciIntxSourceMask};
pub use msi::PciMsiTarget;
#[cfg(feature = "nvme")]
pub use msi::{PciIrqLease, PciMsixAllocation};
#[cfg(feature = "nvme")]
pub(crate) use msi::{PciMsixActivationFailure, PciMsixPreflight};

const MAX_PCIE_LEGACY_IRQS: usize = 8;
#[cfg(virtio_dev)]
const MAX_TAKEN_ENDPOINT_CONFIGS: usize = 16;
const PCI_INTX_LINES: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
struct LegacyIrq {
    binding: BindingIrq,
    raw: Option<usize>,
}

impl LegacyIrq {
    fn try_legacy(raw: usize) -> Option<Self> {
        let binding = BindingIrq::try_legacy(raw).ok()?;
        Some(Self {
            binding,
            raw: Some(raw),
        })
    }
    fn native(binding: BindingIrq, raw: Option<usize>) -> Self {
        Self { binding, raw }
    }

    fn legacy_num(&self) -> Option<usize> {
        self.raw.or_else(|| self.binding.legacy_num())
    }
    fn native_binding(&self) -> Option<BindingIrq> {
        self.binding
            .legacy_num()
            .is_none()
            .then(|| self.binding.clone())
    }
}

#[derive(Clone)]
struct LegacyIrqRoute {
    bus_start: u8,
    bus_end: u8,
    irqs: ArrayVec<LegacyIrq, PCI_INTX_LINES>,
}

impl LegacyIrqRoute {
    fn from_irqs(bus_start: u8, bus_end: u8, irq_list: &[usize]) -> Option<Self> {
        let mut irqs: ArrayVec<LegacyIrq, PCI_INTX_LINES> = ArrayVec::new();
        for irq in irq_list.iter().take(PCI_INTX_LINES) {
            irqs.push(LegacyIrq::try_legacy(*irq)?).ok()?;
        }
        Self::from_legacy_irqs(bus_start, bus_end, &irqs)
    }

    fn from_legacy_irqs(bus_start: u8, bus_end: u8, irq_list: &[LegacyIrq]) -> Option<Self> {
        let irq_count = irq_list.len().min(PCI_INTX_LINES);
        if irq_count == 0 {
            return None;
        }

        let mut irqs: ArrayVec<LegacyIrq, PCI_INTX_LINES> = ArrayVec::new();
        for irq in &irq_list[..irq_count] {
            irqs.push(irq.clone()).ok()?;
        }
        Some(Self {
            bus_start,
            bus_end,
            irqs,
        })
    }

    fn matches_irqs(&self, bus_start: u8, bus_end: u8, irq_list: &[usize]) -> bool {
        let mut irqs: ArrayVec<LegacyIrq, PCI_INTX_LINES> = ArrayVec::new();
        for irq in irq_list.iter().take(PCI_INTX_LINES) {
            let Some(irq) = LegacyIrq::try_legacy(*irq) else {
                return false;
            };
            if irqs.push(irq).is_err() {
                return false;
            }
        }
        self.matches_legacy_irqs(bus_start, bus_end, &irqs)
    }

    fn matches_legacy_irqs(&self, bus_start: u8, bus_end: u8, irq_list: &[LegacyIrq]) -> bool {
        let irq_count = irq_list.len().min(PCI_INTX_LINES);
        self.bus_start == bus_start
            && self.bus_end == bus_end
            && self.irqs.len() == irq_count
            && self.irqs.iter().eq(irq_list[..irq_count].iter())
    }
    fn native_binding_for(&self, info: PciInfo) -> Option<BindingIrq> {
        let route = info.intx_route?;
        if info.address.bus() < self.bus_start
            || info.address.bus() > self.bus_end
            || !(1..=PCI_INTX_LINES as u8).contains(&route.root_pin)
        {
            return None;
        }

        let irq_count = self.irqs.len();
        let route_index = if irq_count == 1 {
            0
        } else {
            (usize::from(route.root_device) + usize::from(route.root_pin) - 1) % irq_count
        };
        self.irqs
            .get(route_index)
            .and_then(LegacyIrq::native_binding)
    }

    fn irq_for(&self, info: PciInfo) -> Option<usize> {
        let route = info.intx_route?;
        if info.address.bus() < self.bus_start
            || info.address.bus() > self.bus_end
            || !(1..=PCI_INTX_LINES as u8).contains(&route.root_pin)
        {
            return None;
        }

        let irq_count = self.irqs.len();
        let route_index = if irq_count == 1 {
            0
        } else {
            (usize::from(route.root_device) + usize::from(route.root_pin) - 1) % irq_count
        };
        self.irqs.get(route_index).and_then(LegacyIrq::legacy_num)
    }
}

static LEGACY_IRQ_ROUTES: Mutex<ArrayVec<LegacyIrqRoute, MAX_PCIE_LEGACY_IRQS>> =
    Mutex::new(ArrayVec::new());
#[cfg(virtio_dev)]
static TAKEN_ENDPOINT_CONFIGS: Mutex<ArrayVec<TakenEndpointConfig, MAX_TAKEN_ENDPOINT_CONFIGS>> =
    Mutex::new(ArrayVec::new());

pub const DEVICE_NAME: &str = "pci-ecam";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DynamicPciIrqSource {
    Acpi,
    Fdt,
}

pub const fn has_pci_endpoint_drivers() -> bool {
    cfg!(any(
        feature = "ahci",
        feature = "intel-net",
        feature = "realtek-rtl8125",
        feature = "nvme",
        feature = "xhci-pci",
        feature = "virtio-blk",
        feature = "virtio-net",
        feature = "virtio-gpu",
        feature = "virtio-input",
        feature = "virtio-socket",
        feature = "list-pci-devices",
    ))
}

pub fn register_ecam_legacy_irq_routes(irqs: &[usize], ecam_size: usize) {
    if irqs.is_empty() {
        return;
    }

    let bus_count = ecam_size >> 20;
    let bus_end = bus_count.saturating_sub(1).min(usize::from(u8::MAX)) as u8;
    register_legacy_irq_routes(0, bus_end, irqs);
}

pub fn pci_mem32_from_ranges(ranges: &[(usize, usize)]) -> Option<PciMem32> {
    let (address, size) = ranges.get(1).copied()?;
    if size == 0 {
        return None;
    }
    Some(PciMem32 {
        address: u32::try_from(address).ok()?,
        size: u32::try_from(size).ok()?,
    })
}

pub fn pci_mem64_from_ranges(ranges: &[(usize, usize)]) -> Option<PciMem64> {
    let (address, size) = ranges.get(2).copied()?;
    if size == 0 || usize::BITS <= 32 {
        return None;
    }
    Some(PciMem64 {
        address: address as u64,
        size: size as u64,
    })
}

pub fn register_ecam_controller(
    plat_dev: PlatformDevice,
    ecam_base: usize,
    ecam_size: usize,
    mem32: Option<PciMem32>,
    mem64: Option<PciMem64>,
) -> Result<(), OnProbeError> {
    register_ecam_controller_with_mmio_op(
        plat_dev,
        ecam_base,
        ecam_size,
        mem32,
        mem64,
        axklib::mmio::op(),
    )
}

pub fn register_ecam_controller_with_mmio_op(
    plat_dev: PlatformDevice,
    ecam_base: usize,
    ecam_size: usize,
    mem32: Option<PciMem32>,
    mem64: Option<PciMem64>,
    mmio_op: &'static dyn MmioOp,
) -> Result<(), OnProbeError> {
    if !has_pci_endpoint_drivers() {
        return Err(OnProbeError::NotMatch);
    }

    if ecam_base == 0 || ecam_size == 0 {
        return Err(OnProbeError::NotMatch);
    }

    let mut controller = rdrive::probe::pci::new_driver_generic(ecam_base, ecam_size, mmio_op)
        .map_err(|err| OnProbeError::other(format!("failed to create PCIe controller: {err:?}")))?;

    if let Some(mem32) = mem32 {
        controller.set_mem32(mem32, false);
    }
    if let Some(mem64) = mem64 {
        controller.set_mem64(mem64, true);
    }
    plat_dev.register_pcie(controller);
    log::info!("registered PCIe ECAM controller");
    Ok(())
}

pub fn resolve_intx_binding(info: PciInfo) -> Result<Option<BindingIrq>, OnProbeError> {
    resolve_intx_binding_with_resolvers(
        info,
        dynamic_pci_irq_source(),
        crate::pci::acpi_irq_for_endpoint,
        crate::pci::fdt_irq_for_endpoint,
        native_legacy_binding_for_endpoint,
        legacy_irq_for_endpoint,
        interrupt_line_irq,
    )
}

pub fn resolve_intx_irq(info: PciInfo) -> Result<Option<usize>, OnProbeError> {
    resolve_intx_binding(info).map(|irq| irq.and_then(|irq| irq.legacy_num()))
}

pub fn prepare_intx_passthrough(info: PciInfo) -> Result<(), OnProbeError> {
    #[cfg(virtio_dev)]
    {
        if info.interrupt_pin == 0 {
            return Err(OnProbeError::NotMatch);
        }

        let bdf = as_device_function(info.address);
        let configs = TAKEN_ENDPOINT_CONFIGS.lock();
        let config = configs
            .iter()
            .find(|config| config.bdf == bdf)
            .ok_or(OnProbeError::NotMatch)?;

        config
            .access
            .update_command(prepare_intx_passthrough_command);
        log::info!(
            "prepared PCI INTx passthrough endpoint {} with native config handoff",
            info.address
        );
        Ok(())
    }

    #[cfg(not(virtio_dev))]
    {
        let _ = info;
        Err(OnProbeError::Unsupported(
            "PCI INTx passthrough handoff requires captured endpoint config access",
        ))
    }
}

pub fn unmask_intx_passthrough(info: PciInfo) -> Result<(), OnProbeError> {
    #[cfg(virtio_dev)]
    {
        if info.interrupt_pin == 0 {
            return Err(OnProbeError::NotMatch);
        }

        let bdf = as_device_function(info.address);
        let configs = TAKEN_ENDPOINT_CONFIGS.lock();
        let config = configs
            .iter()
            .find(|config| config.bdf == bdf)
            .ok_or(OnProbeError::NotMatch)?;

        config
            .access
            .update_command(unmask_intx_passthrough_command);
        log::info!(
            "unmasked PCI INTx passthrough endpoint {} after guest route became ready",
            info.address
        );
        Ok(())
    }

    #[cfg(not(virtio_dev))]
    {
        let _ = info;
        Err(OnProbeError::Unsupported(
            "PCI INTx passthrough handoff requires captured endpoint config access",
        ))
    }
}

#[cfg(any(test, virtio_dev))]
fn prepare_intx_passthrough_command(mut command: CommandRegister) -> CommandRegister {
    command.insert(
        CommandRegister::IO_ENABLE
            | CommandRegister::MEMORY_ENABLE
            | CommandRegister::BUS_MASTER_ENABLE
            | CommandRegister::INTERRUPT_DISABLE,
    );
    command
}

#[cfg(any(test, virtio_dev))]
fn unmask_intx_passthrough_command(mut command: CommandRegister) -> CommandRegister {
    command.remove(CommandRegister::INTERRUPT_DISABLE);
    command
}

#[cfg(test)]
fn resolve_intx_irq_with_resolvers(
    info: PciInfo,
    dynamic_source: Option<DynamicPciIrqSource>,
    acpi_irq: impl FnOnce(PciInfo) -> Result<Option<usize>, OnProbeError>,
    fdt_irq: impl FnOnce(PciInfo) -> Result<Option<usize>, OnProbeError>,
    native_legacy_irq: impl FnOnce(PciInfo) -> Option<BindingIrq>,
    legacy_irq: impl FnOnce(PciInfo) -> Option<usize>,
    interrupt_line: impl FnOnce(u8) -> Option<usize>,
) -> Result<Option<usize>, OnProbeError> {
    resolve_intx_binding_with_resolvers(
        info,
        dynamic_source,
        |info| legacy_irq_result(acpi_irq(info)?),
        |info| legacy_irq_result(fdt_irq(info)?),
        native_legacy_irq,
        legacy_irq,
        interrupt_line,
    )
    .map(|irq| irq.and_then(|irq| irq.legacy_num()))
}

fn resolve_intx_binding_with_resolvers(
    info: PciInfo,
    dynamic_source: Option<DynamicPciIrqSource>,
    acpi_irq: impl FnOnce(PciInfo) -> Result<Option<BindingIrq>, OnProbeError>,
    fdt_irq: impl FnOnce(PciInfo) -> Result<Option<BindingIrq>, OnProbeError>,
    native_legacy_irq: impl FnOnce(PciInfo) -> Option<BindingIrq>,
    legacy_irq: impl FnOnce(PciInfo) -> Option<usize>,
    interrupt_line: impl FnOnce(u8) -> Option<usize>,
) -> Result<Option<BindingIrq>, OnProbeError> {
    match dynamic_source {
        Some(DynamicPciIrqSource::Acpi) => {
            if info.intx_route.is_none() {
                return Ok(None);
            }
            return acpi_irq(info);
        }
        Some(DynamicPciIrqSource::Fdt) => {
            if info.intx_route.is_none() {
                return Ok(None);
            }
            if let Some(irq) = native_legacy_irq(info) {
                return Ok(Some(irq));
            }
            return fdt_irq(info);
        }
        None => {}
    }

    if let Some(irq) = legacy_irq(info) {
        return legacy_irq_result(Some(irq));
    }

    legacy_irq_result(interrupt_line(info.interrupt_line))
}

fn legacy_irq_result(raw: Option<usize>) -> Result<Option<BindingIrq>, OnProbeError> {
    raw.map(legacy_binding).transpose()
}

fn legacy_binding(raw: usize) -> Result<BindingIrq, OnProbeError> {
    BindingIrq::try_legacy(raw).map_err(|_| {
        OnProbeError::other(format!(
            "legacy PCI INTx IRQ {raw} exceeds IRQ framework hardware line width"
        ))
    })
}

fn dynamic_pci_irq_source() -> Option<DynamicPciIrqSource> {
    select_dynamic_pci_irq_source(
        rdrive::probe::acpi::with_acpi(|_| ()).is_some(),
        rdrive::with_fdt(|_| ()).is_some(),
    )
}

fn select_dynamic_pci_irq_source(has_acpi: bool, has_fdt: bool) -> Option<DynamicPciIrqSource> {
    if has_acpi {
        Some(DynamicPciIrqSource::Acpi)
    } else if has_fdt {
        Some(DynamicPciIrqSource::Fdt)
    } else {
        None
    }
}

pub fn legacy_irq_for_endpoint(info: PciInfo) -> Option<usize> {
    LEGACY_IRQ_ROUTES
        .lock()
        .iter()
        .find_map(|route| route.irq_for(info))
}

fn native_legacy_binding_for_endpoint(info: PciInfo) -> Option<BindingIrq> {
    LEGACY_IRQ_ROUTES
        .lock()
        .iter()
        .find_map(|route| route.native_binding_for(info))
}

pub fn legacy_irq_for_address(address: PciAddress) -> Option<usize> {
    legacy_irq_for_endpoint(PciInfo {
        address,
        interrupt_pin: 1,
        interrupt_line: 0,
        intx_route: Some(rdrive::probe::pci::PciIntxRoute {
            root_device: address.device(),
            root_function: address.function(),
            root_pin: 1,
        }),
    })
}

pub(crate) const fn legacy_line_to_irq(line: u8) -> usize {
    legacy_line_to_irq_for_platform(line, cfg!(target_arch = "x86_64"))
}

fn interrupt_line_irq(line: u8) -> Option<usize> {
    if line == 0 || line == u8::MAX {
        return None;
    }
    Some(legacy_line_to_irq(line))
}

const fn legacy_line_to_irq_for_platform(line: u8, is_x86_64: bool) -> usize {
    let base = if is_x86_64 { 0x30 } else { 0 };

    base + line as usize
}

pub fn register_legacy_irq_route(bus_start: u8, bus_end: u8, irq: usize) {
    register_legacy_irq_routes(bus_start, bus_end, &[irq]);
}

pub fn register_legacy_irq_routes(bus_start: u8, bus_end: u8, irqs: &[usize]) {
    let Some(route) = LegacyIrqRoute::from_irqs(bus_start, bus_end, irqs) else {
        return;
    };

    let mut routes = LEGACY_IRQ_ROUTES.lock();
    if routes
        .iter()
        .any(|route| route.matches_irqs(bus_start, bus_end, irqs))
    {
        return;
    }
    if routes.push(route).is_err() {
        log::warn!("too many PCIe legacy IRQ routes; dropping IRQs {irqs:?}");
    } else {
        log::info!("PCIe legacy IRQ route: logical bus {bus_start}..={bus_end} -> IRQs {irqs:?}");
    }
}

pub fn register_native_legacy_irq_route(
    bus_start: u8,
    bus_end: u8,
    irq: BindingIrq,
    raw_irq: Option<usize>,
) {
    let irq = LegacyIrq::native(irq, raw_irq);
    let Some(route) =
        LegacyIrqRoute::from_legacy_irqs(bus_start, bus_end, core::slice::from_ref(&irq))
    else {
        return;
    };

    let mut routes = LEGACY_IRQ_ROUTES.lock();
    if routes
        .iter()
        .any(|route| route.matches_legacy_irqs(bus_start, bus_end, core::slice::from_ref(&irq)))
    {
        return;
    }
    if routes.push(route).is_err() {
        log::warn!("too many PCIe native legacy IRQ routes; dropping IRQ {irq:?}");
    } else {
        log::info!("PCIe native legacy IRQ route: logical bus {bus_start}..={bus_end} -> {irq:?}");
    }
}

#[cfg(virtio_dev)]
pub fn take_virtio_transport(
    endpoint: &mut EndpointRc,
    expected: DeviceType,
) -> Result<impl VirtIoTransport, OnProbeError> {
    take_virtio_transport_with_intx_policy(endpoint, expected, false)
}

/// Verifies that a PCI endpoint belongs to the requested VirtIO device class.
///
/// Probe adapters call this before resolving IRQ routes, mapping ISR
/// capabilities, or mutating endpoint command state. The transport takeover
/// repeats the check so its ownership transition remains self-contained.
#[cfg(virtio_dev)]
pub(crate) fn ensure_virtio_pci_endpoint(
    endpoint: &Endpoint,
    expected: DeviceType,
) -> Result<(), OnProbeError> {
    match (endpoint.vendor_id(), endpoint.device_id()) {
        (0x1af4, 0x1000..=0x107f) => {}
        _ => return Err(OnProbeError::NotMatch),
    }

    let device = as_device_function_info(endpoint);
    match virtio_device_type(&device) {
        Some(actual) if actual == expected => Ok(()),
        _ => Err(OnProbeError::NotMatch),
    }
}

#[cfg(virtio_dev)]
pub fn take_virtio_transport_masked(
    endpoint: &mut EndpointRc,
    expected: DeviceType,
) -> Result<impl VirtIoTransport, OnProbeError> {
    take_virtio_transport_with_intx_policy(endpoint, expected, true)
}

/// Takes a VirtIO PCI transport together with its move-only INTx gate.
///
/// This block-specific path leaves the endpoint masked until the block runtime
/// has installed every IRQ action and enables the returned binding lease.
#[cfg(virtio_dev)]
pub fn take_virtio_block_transport(
    endpoint: &mut EndpointRc,
    expected: DeviceType,
    binding: crate::BindingInfo,
) -> Result<(impl VirtIoTransport, PciIntxIrqLease), OnProbeError> {
    let (bdf, endpoint) = take_matched_virtio_endpoint(endpoint, expected, true)?;
    let irq_lease = PciIntxIrqLease::from_shared(endpoint.clone(), binding);
    let transport = virtio_transport_from_endpoint(bdf, endpoint)?;
    Ok((transport, irq_lease))
}

/// Takes a VirtIO display transport together with its move-only INTx gate.
///
/// The endpoint remains masked until the display maintenance owner has
/// registered its fixed-affinity hard-IRQ action and completed initialization.
#[cfg(virtio_dev)]
pub fn take_virtio_display_transport(
    endpoint: &mut EndpointRc,
    expected: DeviceType,
    binding: crate::BindingInfo,
) -> Result<(impl VirtIoTransport, PciIntxIrqLease), OnProbeError> {
    let (bdf, endpoint) = take_matched_virtio_endpoint(endpoint, expected, true)?;
    let irq_lease = PciIntxIrqLease::from_shared(endpoint.clone(), binding);
    let transport = virtio_transport_from_endpoint(bdf, endpoint)?;
    Ok((transport, irq_lease))
}

/// Takes a VirtIO input transport together with its move-only INTx gate.
///
/// The endpoint remains masked until the input maintenance owner has
/// registered its detached ISR endpoint and published a live session.
#[cfg(virtio_dev)]
pub fn take_virtio_input_transport(
    endpoint: &mut EndpointRc,
    expected: DeviceType,
    binding: crate::BindingInfo,
) -> Result<(impl VirtIoTransport, PciIntxIrqLease), OnProbeError> {
    let (bdf, endpoint) = take_matched_virtio_endpoint(endpoint, expected, true)?;
    let irq_lease = PciIntxIrqLease::from_shared(endpoint.clone(), binding);
    let transport = virtio_transport_from_endpoint(bdf, endpoint)?;
    Ok((transport, irq_lease))
}

#[cfg(virtio_dev)]
fn take_virtio_transport_with_intx_policy(
    endpoint: &mut EndpointRc,
    expected: DeviceType,
    mask_intx_after_match: bool,
) -> Result<impl VirtIoTransport, OnProbeError> {
    let (bdf, endpoint) = take_matched_virtio_endpoint(endpoint, expected, mask_intx_after_match)?;
    virtio_transport_from_endpoint(bdf, endpoint)
}

#[cfg(virtio_dev)]
fn take_matched_virtio_endpoint(
    endpoint: &mut EndpointRc,
    expected: DeviceType,
    mask_intx_after_match: bool,
) -> Result<(DeviceFunction, SharedPciEndpoint), OnProbeError> {
    ensure_virtio_pci_endpoint(endpoint, expected)?;

    let bdf = as_device_function(endpoint.address());

    if mask_intx_after_match {
        mask_intx(endpoint);
    }
    enable_virtio_pci_command(endpoint);

    Ok((bdf, SharedPciEndpoint::new(endpoint.take())))
}

#[cfg(virtio_dev)]
fn virtio_transport_from_endpoint(
    bdf: DeviceFunction,
    endpoint: SharedPciEndpoint,
) -> Result<impl VirtIoTransport, OnProbeError> {
    let config_access = EndpointConfigAccess::new(bdf, endpoint);
    remember_taken_endpoint_config(&config_access);

    let mut root = PciRoot::new(config_access);
    PciTransport::new::<VirtIoHalImpl, _>(&mut root, bdf).map_err(|err| {
        OnProbeError::other(format!(
            "failed to create VirtIO PCI transport at {bdf}: {err:?}"
        ))
    })
}

#[cfg(virtio_dev)]
fn remember_taken_endpoint_config(access: &EndpointConfigAccess) {
    let mut configs = TAKEN_ENDPOINT_CONFIGS.lock();
    if let Some(config) = configs.iter_mut().find(|config| config.bdf == access.bdf) {
        config.access = access.clone_for_handoff();
        return;
    }

    if configs
        .push(TakenEndpointConfig {
            bdf: access.bdf,
            access: access.clone_for_handoff(),
        })
        .is_err()
    {
        log::warn!(
            "too many taken PCI endpoint configs; dropping passthrough handoff access for {}",
            access.bdf
        );
    }
}

#[cfg(virtio_dev)]
fn mask_intx(endpoint: &mut EndpointRc) {
    endpoint.update_command(|mut command| {
        command.insert(CommandRegister::INTERRUPT_DISABLE);
        command
    });
}

#[cfg(virtio_dev)]
fn enable_virtio_pci_command(endpoint: &mut EndpointRc) {
    endpoint.update_command(|mut command| {
        command.insert(
            CommandRegister::IO_ENABLE
                | CommandRegister::MEMORY_ENABLE
                | CommandRegister::BUS_MASTER_ENABLE,
        );
        command
    });
}

#[cfg(virtio_dev)]
fn as_device_function(address: rdrive::probe::pci::PciAddress) -> DeviceFunction {
    DeviceFunction {
        bus: address.bus(),
        device: address.device(),
        function: address.function(),
    }
}

#[cfg(virtio_dev)]
fn as_device_function_info(endpoint: &Endpoint) -> DeviceFunctionInfo {
    let class_info = endpoint.revision_and_class();
    let header_type = HeaderType::from(((endpoint.read(0x0c) >> 16) as u8) & 0x7f);
    DeviceFunctionInfo {
        vendor_id: endpoint.vendor_id(),
        device_id: endpoint.device_id(),
        class: class_info.base_class,
        subclass: class_info.sub_class,
        prog_if: class_info.interface,
        revision: class_info.revision_id,
        header_type,
    }
}

#[cfg(virtio_dev)]
struct TakenEndpointConfig {
    bdf: DeviceFunction,
    access: EndpointConfigAccess,
}

#[cfg(virtio_dev)]
struct EndpointConfigAccess {
    bdf: DeviceFunction,
    endpoint: SharedPciEndpoint,
}

#[cfg(virtio_dev)]
impl EndpointConfigAccess {
    fn new(bdf: DeviceFunction, endpoint: SharedPciEndpoint) -> Self {
        Self { bdf, endpoint }
    }

    fn assert_same_function(&self, device_function: DeviceFunction) {
        assert_eq!(device_function, self.bdf);
    }

    fn clone_for_handoff(&self) -> Self {
        // SAFETY: EndpointConfigAccess serializes all shared config-space
        // accesses through the same internal mutex.
        unsafe { self.unsafe_clone() }
    }

    fn update_command<F>(&self, f: F)
    where
        F: FnOnce(CommandRegister) -> CommandRegister,
    {
        self.endpoint.update_command(f);
    }
}

#[cfg(virtio_dev)]
impl ConfigurationAccess for EndpointConfigAccess {
    fn read_word(&self, device_function: DeviceFunction, register_offset: u8) -> u32 {
        self.assert_same_function(device_function);
        self.endpoint.read(register_offset.into())
    }

    fn write_word(&mut self, device_function: DeviceFunction, register_offset: u8, data: u32) {
        self.assert_same_function(device_function);
        self.endpoint.write(register_offset.into(), data);
    }

    unsafe fn unsafe_clone(&self) -> Self {
        Self {
            bdf: self.bdf,
            endpoint: self.endpoint.clone(),
        }
    }
}

#[cfg(test)]
mod tests;
