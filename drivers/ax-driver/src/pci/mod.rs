use alloc::format;
#[cfg(virtio_dev)]
use alloc::sync::Arc;

use heapless::Vec as ArrayVec;
use mmio_api::MmioOp;
#[cfg(virtio_dev)]
use rdrive::probe::pci::{Endpoint, EndpointRc};
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        pci::{PciAddress, PciMem32, PciMem64},
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
mod fdt;
#[cfg(all(
    plat_dyn,
    target_os = "none",
    any(
        feature = "intel-net",
        feature = "ixgbe",
        feature = "realtek-rtl8125",
        feature = "virtio-net",
        feature = "xhci-pci",
    )
))]
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

    fn irq_for(&self, address: PciAddress, interrupt_pin: u8) -> Option<usize> {
        if address.bus() < self.bus_start
            || address.bus() > self.bus_end
            || !(1..=PCI_INTX_LINES as u8).contains(&interrupt_pin)
        {
            return None;
        }

        let irq_count = usize::from(self.irq_count);
        let route_index = if irq_count == 1 {
            0
        } else {
            (usize::from(address.device()) + usize::from(interrupt_pin) - 1) % irq_count
        };
        Some(self.irqs[route_index])
    }
}

static LEGACY_IRQ_ROUTES: SpinMutex<ArrayVec<LegacyIrqRoute, MAX_PCIE_LEGACY_IRQS>> =
    SpinMutex::new(ArrayVec::new());

pub const DEVICE_NAME: &str = "pci-ecam";

pub const fn has_static_endpoint_drivers() -> bool {
    cfg!(any(
        feature = "ahci",
        feature = "ixgbe",
        feature = "intel-net",
        feature = "realtek-rtl8125",
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

pub fn legacy_irq_for_endpoint(address: PciAddress, interrupt_pin: u8) -> Option<usize> {
    LEGACY_IRQ_ROUTES
        .lock()
        .iter()
        .find_map(|route| route.irq_for(address, interrupt_pin))
}

pub fn legacy_irq_for_address(address: PciAddress) -> Option<usize> {
    legacy_irq_for_endpoint(address, 1)
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

    let mut root = PciRoot::new(EndpointConfigAccess::new(bdf, endpoint.take()));
    PciTransport::new::<VirtIoHalImpl, _>(&mut root, bdf).map_err(|err| {
        OnProbeError::other(format!(
            "failed to create VirtIO PCI transport at {bdf}: {err:?}"
        ))
    })
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
