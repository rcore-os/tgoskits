use alloc::format;
#[cfg(virtio_dev)]
use alloc::sync::Arc;

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
    DeviceType, Transport,
    pci::{
        PciTransport,
        bus::{ConfigurationAccess, DeviceFunction, DeviceFunctionInfo, HeaderType, PciRoot},
        virtio_device_type,
    },
};

use crate::BindingIrq;
#[cfg(virtio_dev)]
use crate::virtio::VirtIoHalImpl;

mod acpi;
mod fdt;
pub(crate) use acpi::acpi_irq_for_endpoint;
pub(crate) use fdt::fdt_irq_for_endpoint;

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

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use core::cell::Cell;

    use axklib::{
        AxError, AxResult, BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle,
        IrqId, Klib, PhysAddr, VirtAddr, impl_trait,
    };
    use rdrive::probe::{
        OnProbeError,
        pci::{PciAddress, PciInfo, PciIntxRoute},
    };

    use super::{
        DynamicPciIrqSource, LegacyIrqRoute, legacy_line_to_irq_for_platform,
        prepare_intx_passthrough_command, resolve_intx_binding_with_resolvers,
        resolve_intx_irq_with_resolvers, select_dynamic_pci_irq_source,
        unmask_intx_passthrough_command,
    };
    use crate::{BindingIrq, BindingIrqSource};
    struct KlibImpl;
    impl_trait! {
        impl Klib for KlibImpl {
            fn mem_iomap(_addr: PhysAddr, _size: usize) -> AxResult<VirtAddr> {
                Err(AxError::Unsupported)
            }

            fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
                PhysAddr::from_usize(addr.as_usize())
            }

            fn mem_make_dma_coherent_uncached(_addr: VirtAddr, _size: usize) -> AxResult {
                Err(AxError::Unsupported)
            }

            fn mem_restore_dma_cached(_addr: VirtAddr, _size: usize) -> AxResult {
                Err(AxError::Unsupported)
            }

            fn dma_alloc_pages(
                _dma_mask: u64,
                _num_pages: usize,
                _align: usize,
            ) -> AxResult<VirtAddr> {
                Err(AxError::Unsupported)
            }

            fn dma_dealloc_pages(_addr: VirtAddr, _num_pages: usize) {}

            fn time_busy_wait(_dur: core::time::Duration) {}

            fn time_monotonic_nanos() -> u64 {
                0
            }

            fn time_try_init_epoch_offset(_epoch_time_nanos: u64) -> bool {
                false
            }

            fn irq_set_enable(_irq: IrqId, _enabled: bool) -> axklib::AxResult {
                Ok(())
            }

            fn irq_request_shared(
                _irq: IrqId,
                _handler: BoxedIrqHandler,
            ) -> AxResult<IrqHandle> {
                Err(AxError::Unsupported)
            }

            fn irq_request_shared_disabled(
                _irq: IrqId,
                _handler: BoxedIrqHandler,
            ) -> AxResult<IrqHandle> {
                Err(AxError::Unsupported)
            }

            fn irq_request_percpu(
                _irq: IrqId,
                _cpus: IrqCpuMask,
                _handler: ConcurrentBoxedIrqHandler,
            ) -> AxResult<IrqHandle> {
                Err(AxError::Unsupported)
            }

            fn irq_free(_handle: IrqHandle) -> AxResult {
                Err(AxError::Unsupported)
            }

            fn irq_enable(_handle: IrqHandle) -> AxResult {
                Err(AxError::Unsupported)
            }

            fn irq_disable(_handle: IrqHandle) -> AxResult {
                Err(AxError::Unsupported)
            }
        }
    }

    #[test]
    fn x86_64_legacy_line_uses_dynamic_ioapic_base() {
        assert_eq!(legacy_line_to_irq_for_platform(9, true), 0x39);
    }

    #[test]
    fn non_x86_64_legacy_line_remains_raw_irq() {
        assert_eq!(legacy_line_to_irq_for_platform(9, false), 9);
    }

    #[test]
    fn legacy_route_uses_swizzled_root_device_and_pin() {
        let route = LegacyIrqRoute::from_irqs(0, 8, &[40, 41, 42, 43]).unwrap();
        let info = PciInfo {
            address: PciAddress::new(0, 2, 7, 0),
            interrupt_pin: 1,
            interrupt_line: 0,
            intx_route: Some(PciIntxRoute {
                root_device: 2,
                root_function: 0,
                root_pin: 4,
            }),
        };

        assert_eq!(route.irq_for(info), Some(41));
    }

    #[test]
    fn legacy_route_ignores_endpoints_without_intx_route() {
        let route = LegacyIrqRoute::from_irqs(0, 8, &[40, 41, 42, 43]).unwrap();
        let info = PciInfo {
            address: PciAddress::new(0, 2, 7, 0),
            interrupt_pin: 1,
            interrupt_line: 0,
            intx_route: None,
        };

        assert_eq!(route.irq_for(info), None);
    }

    #[test]
    fn resolve_intx_irq_source_prefers_acpi_when_both_backends_exist() {
        assert_eq!(
            select_dynamic_pci_irq_source(true, true),
            Some(DynamicPciIrqSource::Acpi)
        );
    }

    #[test]
    fn resolve_intx_irq_acpi_error_does_not_fallback_to_fdt_or_legacy() {
        let info = endpoint_with_intx_route();
        let fdt_called = Cell::new(false);
        let legacy_called = Cell::new(false);
        let line_called = Cell::new(false);

        let err = resolve_intx_irq_with_resolvers(
            info,
            Some(DynamicPciIrqSource::Acpi),
            |_| Err(OnProbeError::other("acpi irq failed")),
            |_| {
                fdt_called.set(true);
                Ok(Some(55))
            },
            |_| None,
            |_| {
                legacy_called.set(true);
                Some(66)
            },
            |_| {
                line_called.set(true);
                Some(77)
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("acpi irq failed"));
        assert!(!fdt_called.get());
        assert!(!legacy_called.get());
        assert!(!line_called.get());
    }

    #[test]
    fn resolve_intx_binding_acpi_keeps_gsi_source_native() {
        let info = endpoint_with_intx_route();
        let irq = resolve_intx_binding_with_resolvers(
            info,
            Some(DynamicPciIrqSource::Acpi),
            |_| Ok(Some(BindingIrq::acpi_gsi(18))),
            |_| Ok(Some(BindingIrq::acpi_gsi(19))),
            |_| None,
            |_| Some(66),
            |_| Some(77),
        )
        .unwrap()
        .unwrap();

        assert_eq!(irq.legacy_num(), None);
        assert_eq!(
            irq.as_irq_source(),
            Some(irq_framework::IrqSource::AcpiGsi(18))
        );
    }

    #[test]
    fn resolve_intx_binding_acpi_keeps_route_metadata_native() {
        let info = endpoint_with_intx_route();
        let controller = rdrive::DeviceId::new();
        let route = irq_framework::AcpiGsiRoute {
            gsi: 10,
            vector: 0x3a,
            controller: irq_framework::AcpiGsiController::IoApic,
            controller_id: 0,
            controller_address: 0xfec0_0000,
            controller_input: 10,
            trigger: irq_framework::AcpiIrqTrigger::Level,
            polarity: irq_framework::AcpiIrqPolarity::ActiveLow,
        };
        let irq = resolve_intx_binding_with_resolvers(
            info,
            Some(DynamicPciIrqSource::Acpi),
            |_| Ok(Some(BindingIrq::acpi_gsi_route(route))),
            |_| {
                Ok(Some(BindingIrq::fdt_interrupt_with_controller(
                    controller,
                    [0, 42, 4],
                )))
            },
            |_| None,
            |_| Some(66),
            |_| Some(77),
        )
        .unwrap()
        .unwrap();

        assert_eq!(irq.legacy_num(), None);
        assert_eq!(
            irq.as_irq_source(),
            Some(irq_framework::IrqSource::AcpiGsiRoute(route))
        );
    }

    #[test]
    fn resolve_intx_binding_fdt_keeps_interrupt_cells_native() {
        let info = endpoint_with_intx_route();
        let controller = rdrive::DeviceId::new();
        let irq = resolve_intx_binding_with_resolvers(
            info,
            Some(DynamicPciIrqSource::Fdt),
            |_| Ok(Some(BindingIrq::acpi_gsi(18))),
            |_| {
                Ok(Some(BindingIrq::fdt_interrupt_with_controller(
                    controller,
                    [0, 42, 4],
                )))
            },
            |_| None,
            |_| Some(66),
            |_| Some(77),
        )
        .unwrap()
        .unwrap();

        assert_eq!(irq.legacy_num(), None);
        let BindingIrq::Source(BindingIrqSource::FdtInterrupt(spec)) = irq else {
            panic!("expected native FDT interrupt binding");
        };
        assert_eq!(spec.controller, controller);
        assert_eq!(spec.cells, [0, 42, 4]);
    }

    #[test]
    fn resolve_intx_binding_fdt_prefers_registered_native_legacy_route() {
        let info = endpoint_with_intx_route();
        let fdt_called = Cell::new(false);
        let legacy_called = Cell::new(false);
        let line_called = Cell::new(false);
        let controller = rdrive::DeviceId::new();
        let irq = resolve_intx_binding_with_resolvers(
            info,
            Some(DynamicPciIrqSource::Fdt),
            |_| Ok(Some(BindingIrq::acpi_gsi(18))),
            |_| {
                fdt_called.set(true);
                Ok(Some(BindingIrq::fdt_interrupt_with_controller(
                    controller,
                    [0, 0, 4],
                )))
            },
            |_| {
                Some(BindingIrq::fdt_interrupt_with_controller(
                    controller,
                    [0, 245, 4],
                ))
            },
            |_| {
                legacy_called.set(true);
                Some(66)
            },
            |_| {
                line_called.set(true);
                Some(77)
            },
        )
        .unwrap()
        .unwrap();

        assert!(!fdt_called.get());
        assert!(!legacy_called.get());
        assert!(!line_called.get());
        assert_eq!(irq.legacy_num(), None);
        let BindingIrq::Source(BindingIrqSource::FdtInterrupt(spec)) = irq else {
            panic!("expected native FDT interrupt binding");
        };
        assert_eq!(spec.controller, controller);
        assert_eq!(spec.cells, [0, 245, 4]);
    }

    #[test]
    fn resolve_intx_binding_without_dynamic_firmware_uses_legacy_irq_line() {
        let info = endpoint_with_intx_route();
        let controller = rdrive::DeviceId::new();
        let irq = resolve_intx_binding_with_resolvers(
            info,
            None,
            |_| Ok(Some(BindingIrq::acpi_gsi(18))),
            |_| {
                Ok(Some(BindingIrq::fdt_interrupt_with_controller(
                    controller,
                    [0, 42, 4],
                )))
            },
            |_| None,
            |_| None,
            |_| Some(77),
        )
        .unwrap()
        .unwrap();

        assert_eq!(irq.legacy_num(), Some(77));
        assert_eq!(irq.as_irq_source(), None);
    }

    #[test]
    fn resolve_intx_irq_fdt_none_does_not_fallback_to_legacy_or_interrupt_line() {
        let info = endpoint_with_intx_route();
        let acpi_called = Cell::new(false);
        let legacy_called = Cell::new(false);
        let line_called = Cell::new(false);

        let irq = resolve_intx_irq_with_resolvers(
            info,
            Some(DynamicPciIrqSource::Fdt),
            |_| {
                acpi_called.set(true);
                Ok(Some(44))
            },
            |_| Ok(None),
            |_| None,
            |_| {
                legacy_called.set(true);
                Some(66)
            },
            |_| {
                line_called.set(true);
                Some(77)
            },
        )
        .unwrap();

        assert_eq!(irq, None);
        assert!(!acpi_called.get());
        assert!(!legacy_called.get());
        assert!(!line_called.get());
    }

    #[test]
    fn resolve_intx_irq_acpi_none_does_not_fallback_to_legacy_or_interrupt_line() {
        let info = endpoint_with_intx_route();
        let fdt_called = Cell::new(false);
        let legacy_called = Cell::new(false);
        let line_called = Cell::new(false);

        let irq = resolve_intx_irq_with_resolvers(
            info,
            Some(DynamicPciIrqSource::Acpi),
            |_| Ok(None),
            |_| {
                fdt_called.set(true);
                Ok(Some(55))
            },
            |_| None,
            |_| {
                legacy_called.set(true);
                Some(66)
            },
            |_| {
                line_called.set(true);
                Some(77)
            },
        )
        .unwrap();

        assert_eq!(irq, None);
        assert!(!fdt_called.get());
        assert!(!legacy_called.get());
        assert!(!line_called.get());
    }

    #[test]
    fn resolve_intx_irq_dynamic_source_without_intx_route_does_not_use_interrupt_line() {
        let info = PciInfo {
            intx_route: None,
            ..endpoint_with_intx_route()
        };
        let acpi_called = Cell::new(false);
        let fdt_called = Cell::new(false);
        let legacy_called = Cell::new(false);
        let line_called = Cell::new(false);

        let irq = resolve_intx_irq_with_resolvers(
            info,
            Some(DynamicPciIrqSource::Acpi),
            |_| {
                acpi_called.set(true);
                Ok(Some(44))
            },
            |_| {
                fdt_called.set(true);
                Ok(Some(55))
            },
            |_| None,
            |_| {
                legacy_called.set(true);
                Some(66)
            },
            |_| {
                line_called.set(true);
                Some(77)
            },
        )
        .unwrap();

        assert_eq!(irq, None);
        assert!(!acpi_called.get());
        assert!(!fdt_called.get());
        assert!(!legacy_called.get());
        assert!(!line_called.get());
    }

    #[test]
    fn resolve_intx_irq_static_source_keeps_legacy_and_interrupt_line_fallback() {
        let info = endpoint_with_intx_route();
        let acpi_called = Cell::new(false);
        let fdt_called = Cell::new(false);
        let line_called = Cell::new(false);

        let irq = resolve_intx_irq_with_resolvers(
            info,
            None,
            |_| {
                acpi_called.set(true);
                Ok(Some(44))
            },
            |_| {
                fdt_called.set(true);
                Ok(Some(55))
            },
            |_| None,
            |_| None,
            |line| {
                line_called.set(true);
                assert_eq!(line, 9);
                Some(77)
            },
        )
        .unwrap();

        assert_eq!(irq, Some(77));
        assert!(!acpi_called.get());
        assert!(!fdt_called.get());
        assert!(line_called.get());
    }

    #[test]
    fn prepare_intx_passthrough_command_masks_native_intx_until_guest_route_ready() {
        let mut command = pcie::CommandRegister::INTERRUPT_DISABLE;

        command = prepare_intx_passthrough_command(command);

        assert!(command.contains(pcie::CommandRegister::IO_ENABLE));
        assert!(command.contains(pcie::CommandRegister::MEMORY_ENABLE));
        assert!(command.contains(pcie::CommandRegister::BUS_MASTER_ENABLE));
        assert!(command.contains(pcie::CommandRegister::INTERRUPT_DISABLE));

        command = prepare_intx_passthrough_command(pcie::CommandRegister::empty());

        assert!(command.contains(pcie::CommandRegister::IO_ENABLE));
        assert!(command.contains(pcie::CommandRegister::MEMORY_ENABLE));
        assert!(command.contains(pcie::CommandRegister::BUS_MASTER_ENABLE));
        assert!(command.contains(pcie::CommandRegister::INTERRUPT_DISABLE));
    }

    #[test]
    fn unmask_intx_passthrough_command_clears_native_intx_mask() {
        let mut command = pcie::CommandRegister::INTERRUPT_DISABLE
            | pcie::CommandRegister::IO_ENABLE
            | pcie::CommandRegister::MEMORY_ENABLE
            | pcie::CommandRegister::BUS_MASTER_ENABLE;

        command = unmask_intx_passthrough_command(command);

        assert!(command.contains(pcie::CommandRegister::IO_ENABLE));
        assert!(command.contains(pcie::CommandRegister::MEMORY_ENABLE));
        assert!(command.contains(pcie::CommandRegister::BUS_MASTER_ENABLE));
        assert!(!command.contains(pcie::CommandRegister::INTERRUPT_DISABLE));
    }

    fn endpoint_with_intx_route() -> PciInfo {
        PciInfo {
            address: PciAddress::new(0, 2, 7, 0),
            interrupt_pin: 1,
            interrupt_line: 9,
            intx_route: Some(PciIntxRoute {
                root_device: 2,
                root_function: 0,
                root_pin: 1,
            }),
        }
    }
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
) -> Result<impl Transport + 'static, OnProbeError> {
    take_virtio_transport_with_intx_policy(endpoint, expected, false)
}

#[cfg(virtio_dev)]
pub fn take_virtio_transport_masked(
    endpoint: &mut EndpointRc,
    expected: DeviceType,
) -> Result<impl Transport + 'static, OnProbeError> {
    take_virtio_transport_with_intx_policy(endpoint, expected, true)
}

#[cfg(virtio_dev)]
fn take_virtio_transport_with_intx_policy(
    endpoint: &mut EndpointRc,
    expected: DeviceType,
    mask_intx_after_match: bool,
) -> Result<impl Transport + 'static, OnProbeError> {
    match (endpoint.vendor_id(), endpoint.device_id()) {
        (0x1af4, 0x1000..=0x107f) => {}
        _ => return Err(OnProbeError::NotMatch),
    }

    let bdf = as_device_function(endpoint.address());
    let dev_info = as_device_function_info(endpoint);
    let ty = virtio_device_type(&dev_info).ok_or(OnProbeError::NotMatch)?;
    if ty != expected {
        return Err(OnProbeError::NotMatch);
    }

    if mask_intx_after_match {
        mask_intx(endpoint);
    }
    enable_virtio_pci_command(endpoint);

    let config_access = EndpointConfigAccess::new(bdf, endpoint.take());
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
    endpoint: Arc<Mutex<Endpoint>>,
}

#[cfg(virtio_dev)]
impl EndpointConfigAccess {
    fn new(bdf: DeviceFunction, endpoint: Endpoint) -> Self {
        Self {
            bdf,
            endpoint: Arc::new(Mutex::new(endpoint)),
        }
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
        self.endpoint.lock().update_command(f);
    }
}

#[cfg(virtio_dev)]
impl ConfigurationAccess for EndpointConfigAccess {
    fn read_word(&self, device_function: DeviceFunction, register_offset: u8) -> u32 {
        self.assert_same_function(device_function);
        self.endpoint.lock().read(register_offset.into())
    }

    fn write_word(&mut self, device_function: DeviceFunction, register_offset: u8, data: u32) {
        self.assert_same_function(device_function);
        self.endpoint.lock().write(register_offset.into(), data);
    }

    unsafe fn unsafe_clone(&self) -> Self {
        Self {
            bdf: self.bdf,
            endpoint: Arc::clone(&self.endpoint),
        }
    }
}
