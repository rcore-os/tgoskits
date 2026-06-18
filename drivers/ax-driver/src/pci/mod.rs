use alloc::format;
#[cfg(virtio_dev)]
use alloc::sync::Arc;

use heapless::Vec as ArrayVec;
use mmio_api::MmioOp;
#[cfg(virtio_dev)]
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
use spin::Mutex;
use spin::Mutex as SpinMutex;
#[cfg(virtio_dev)]
use virtio_drivers::transport::{
    DeviceType, Transport,
    pci::{
        PciTransport,
        bus::{ConfigurationAccess, DeviceFunction, DeviceFunctionInfo, HeaderType, PciRoot},
        virtio_device_type,
    },
};

#[cfg(virtio_dev)]
use crate::virtio::VirtIoHalImpl;

#[cfg(plat_dyn)]
mod acpi;
#[cfg(plat_dyn)]
mod fdt;
#[cfg(plat_dyn)]
pub(crate) use acpi::acpi_irq_for_endpoint;
#[cfg(plat_dyn)]
pub(crate) use fdt::fdt_irq_for_endpoint;

const MAX_PCIE_LEGACY_IRQS: usize = 8;
const PCI_INTX_LINES: usize = 4;

#[derive(Clone, Copy)]
struct LegacyIrqRoute {
    bus_start: u8,
    bus_end: u8,
    irqs: [usize; PCI_INTX_LINES],
    irq_count: u8,
}

impl LegacyIrqRoute {
    fn from_irqs(bus_start: u8, bus_end: u8, irq_list: &[usize]) -> Option<Self> {
        let irq_count = irq_list.len().min(PCI_INTX_LINES);
        if irq_count == 0 {
            return None;
        }

        let mut irqs = [0; PCI_INTX_LINES];
        irqs[..irq_count].copy_from_slice(&irq_list[..irq_count]);
        Some(Self {
            bus_start,
            bus_end,
            irqs,
            irq_count: irq_count as u8,
        })
    }

    fn matches(&self, bus_start: u8, bus_end: u8, irq_list: &[usize]) -> bool {
        let irq_count = irq_list.len().min(PCI_INTX_LINES);
        self.bus_start == bus_start
            && self.bus_end == bus_end
            && usize::from(self.irq_count) == irq_count
            && self.irqs[..irq_count] == irq_list[..irq_count]
    }

    fn irq_for(&self, info: PciInfo) -> Option<usize> {
        let route = info.intx_route?;
        if info.address.bus() < self.bus_start
            || info.address.bus() > self.bus_end
            || !(1..=PCI_INTX_LINES as u8).contains(&route.root_pin)
        {
            return None;
        }

        let irq_count = usize::from(self.irq_count);
        let route_index = if irq_count == 1 {
            0
        } else {
            (usize::from(route.root_device) + usize::from(route.root_pin) - 1) % irq_count
        };
        Some(self.irqs[route_index])
    }
}

static LEGACY_IRQ_ROUTES: SpinMutex<ArrayVec<LegacyIrqRoute, MAX_PCIE_LEGACY_IRQS>> =
    SpinMutex::new(ArrayVec::new());

pub const DEVICE_NAME: &str = "pci-ecam";

#[cfg(any(plat_dyn, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DynamicPciIrqSource {
    Acpi,
    Fdt,
}

pub const fn has_static_endpoint_drivers() -> bool {
    cfg!(any(
        feature = "ahci",
        feature = "ixgbe",
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

pub fn register_static_legacy_irq_routes(irqs: &[usize], ecam_size: usize) {
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
    if !has_static_endpoint_drivers() {
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

pub fn resolve_intx_irq(info: PciInfo) -> Result<Option<usize>, OnProbeError> {
    #[cfg(plat_dyn)]
    {
        resolve_intx_irq_with_resolvers(
            info,
            dynamic_pci_irq_source(),
            crate::pci::acpi_irq_for_endpoint,
            crate::pci::fdt_irq_for_endpoint,
            legacy_irq_for_endpoint,
            interrupt_line_irq,
        )
    }

    #[cfg(not(plat_dyn))]
    {
        if let Some(irq) = legacy_irq_for_endpoint(info) {
            return Ok(Some(irq));
        }

        Ok(interrupt_line_irq(info.interrupt_line))
    }
}

#[cfg(any(plat_dyn, test))]
fn resolve_intx_irq_with_resolvers(
    info: PciInfo,
    dynamic_source: Option<DynamicPciIrqSource>,
    acpi_irq: impl FnOnce(PciInfo) -> Result<Option<usize>, OnProbeError>,
    fdt_irq: impl FnOnce(PciInfo) -> Result<Option<usize>, OnProbeError>,
    legacy_irq: impl FnOnce(PciInfo) -> Option<usize>,
    interrupt_line: impl FnOnce(u8) -> Option<usize>,
) -> Result<Option<usize>, OnProbeError> {
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
            return fdt_irq(info);
        }
        None => {}
    }

    if let Some(irq) = legacy_irq(info) {
        return Ok(Some(irq));
    }

    Ok(interrupt_line(info.interrupt_line))
}

#[cfg(plat_dyn)]
fn dynamic_pci_irq_source() -> Option<DynamicPciIrqSource> {
    select_dynamic_pci_irq_source(
        rdrive::probe::acpi::with_acpi(|_| ()).is_some(),
        rdrive::with_fdt(|_| ()).is_some(),
    )
}

#[cfg(any(plat_dyn, test))]
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
    legacy_line_to_irq_for_platform(line, cfg!(target_arch = "x86_64"), cfg!(plat_dyn))
}

fn interrupt_line_irq(line: u8) -> Option<usize> {
    if line == 0 || line == u8::MAX {
        return None;
    }
    Some(legacy_line_to_irq(line))
}

const fn legacy_line_to_irq_for_platform(line: u8, is_x86_64: bool, is_plat_dyn: bool) -> usize {
    let base = if is_x86_64 {
        if is_plat_dyn { 0x30 } else { 0x20 }
    } else {
        0
    };

    base + line as usize
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use core::cell::Cell;

    #[cfg(plat_dyn)]
    use axklib::{
        AxError, AxResult, IrqCpuMask, IrqHandle, Klib, PhysAddr, RawIrqHandler, VirtAddr,
        impl_trait,
    };
    use rdrive::probe::{
        OnProbeError,
        pci::{PciAddress, PciInfo, PciIntxRoute},
    };

    use super::{
        DynamicPciIrqSource, LegacyIrqRoute, legacy_line_to_irq_for_platform,
        resolve_intx_irq_with_resolvers, select_dynamic_pci_irq_source,
    };

    #[cfg(plat_dyn)]
    struct KlibImpl;

    #[cfg(plat_dyn)]
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

            fn irq_set_enable(_irq: usize, _enabled: bool) {}

            fn irq_request_shared(
                _irq: usize,
                _handler: RawIrqHandler,
                _data: core::ptr::NonNull<()>,
            ) -> AxResult<IrqHandle> {
                Err(AxError::Unsupported)
            }

            fn irq_request_percpu(
                _irq: usize,
                _cpus: IrqCpuMask,
                _handler: RawIrqHandler,
                _data: core::ptr::NonNull<()>,
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
    fn x86_64_legacy_line_uses_dynamic_ioapic_base_on_plat_dyn() {
        assert_eq!(legacy_line_to_irq_for_platform(9, true, false), 0x29);
        assert_eq!(legacy_line_to_irq_for_platform(9, true, true), 0x39);
    }

    #[test]
    fn non_x86_64_legacy_line_remains_raw_irq() {
        assert_eq!(legacy_line_to_irq_for_platform(9, false, false), 9);
        assert_eq!(legacy_line_to_irq_for_platform(9, false, true), 9);
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
        .any(|route| route.matches(bus_start, bus_end, irqs))
    {
        return;
    }
    if routes.push(route).is_err() {
        log::warn!("too many PCIe legacy IRQ routes; dropping IRQs {irqs:?}");
    } else {
        log::info!("PCIe legacy IRQ route: logical bus {bus_start}..={bus_end} -> IRQs {irqs:?}");
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

    let mut root = PciRoot::new(EndpointConfigAccess::new(bdf, endpoint.take()));
    PciTransport::new::<VirtIoHalImpl, _>(&mut root, bdf).map_err(|err| {
        OnProbeError::other(format!(
            "failed to create VirtIO PCI transport at {bdf}: {err:?}"
        ))
    })
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
