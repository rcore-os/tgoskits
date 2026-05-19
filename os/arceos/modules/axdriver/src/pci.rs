use alloc::format;
#[cfg(virtio_dev)]
use alloc::sync::Arc;

#[cfg(virtio_dev)]
use rdrive::probe::pci::{Endpoint, EndpointRc};
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        pci::{PciMem32, PciMem64, PcieController},
        static_::StaticInfo,
    },
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};
#[cfg(virtio_dev)]
use spin::Mutex;
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

pub const DEVICE_NAME: &str = "pci-ecam";

pub const REGISTER: DriverRegister = DriverRegister {
    name: "Static PCIe ECAM",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe_pci_ecam,
    }],
};

fn probe_pci_ecam(info: StaticInfo, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if info.name() != DEVICE_NAME || ax_config::devices::PCI_ECAM_BASE == 0 {
        return Err(OnProbeError::NotMatch);
    }

    let ecam_size = (ax_config::devices::PCI_BUS_END + 1) << 20;
    let mut controller = rdrive::probe::pci::new_driver_generic(
        ax_config::devices::PCI_ECAM_BASE,
        ecam_size,
        axklib::mmio::op(),
    )
    .map_err(|err| OnProbeError::other(format!("failed to create PCIe controller: {err:?}")))?;

    set_configured_mem_ranges(&mut controller);
    plat_dev.register_pcie(controller);
    log::info!("registered static PCIe ECAM controller");
    Ok(())
}

fn set_configured_mem_ranges(controller: &mut PcieController) {
    for (index, (address, size)) in ax_config::devices::PCI_RANGES.iter().copied().enumerate() {
        if size == 0 {
            continue;
        }
        match index {
            1 => {
                if let (Ok(address), Ok(size)) = (u32::try_from(address), u32::try_from(size)) {
                    controller.set_mem32(PciMem32 { address, size }, false);
                }
            }
            2 if usize::BITS > 32 => {
                controller.set_mem64(
                    PciMem64 {
                        address: address as u64,
                        size: size as u64,
                    },
                    true,
                );
            }
            _ => {}
        }
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
