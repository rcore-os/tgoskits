extern crate alloc;

use alloc::{format, sync::Arc};
use core::ptr::NonNull;

use ax_hal::mem::{mmio_ranges, phys_to_virt};
use mmio_api::{MapError, MmioAddr, MmioOp, MmioRaw};
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        pci::{Endpoint, EndpointRc, PciMem32, PciMem64, PcieController},
        static_::StaticInfo,
    },
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};
use spin::Mutex;
use virtio_drivers::transport::{
    DeviceType, Transport,
    pci::{
        PciTransport,
        bus::{ConfigurationAccess, DeviceFunction, DeviceFunctionInfo, HeaderType, PciRoot},
        virtio_device_type,
    },
};

use crate::static_devices::virtio::VirtIoHalImpl;

pub(super) const DEVICE_NAME: &str = "pci-ecam";

pub(super) const REGISTER: DriverRegister = DriverRegister {
    name: "Static PCIe ECAM",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe_pci_ecam,
    }],
};

struct StaticMmio;

impl MmioOp for StaticMmio {
    fn ioremap(&self, addr: MmioAddr, size: usize) -> Result<MmioRaw, MapError> {
        let virt = phys_to_virt(addr.as_usize().into()).as_mut_ptr();
        let virt = NonNull::new(virt).ok_or(MapError::Invalid)?;
        Ok(unsafe { MmioRaw::new(addr, virt, size) })
    }

    fn iounmap(&self, _mmio: &MmioRaw) {}
}

static STATIC_MMIO: StaticMmio = StaticMmio;

fn probe_pci_ecam(info: StaticInfo, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if info.name() != DEVICE_NAME || ax_config::devices::PCI_ECAM_BASE == 0 {
        return Err(OnProbeError::NotMatch);
    }

    let ecam_size = (ax_config::devices::PCI_BUS_END + 1) << 20;
    let mut controller = rdrive::probe::pci::new_driver_generic(
        ax_config::devices::PCI_ECAM_BASE,
        ecam_size,
        &STATIC_MMIO,
    )
    .map_err(|err| OnProbeError::other(format!("failed to create PCIe controller: {err:?}")))?;

    set_configured_mem_ranges(&mut controller);
    plat_dev.register_pcie(controller);
    info!("registered static PCIe ECAM controller");
    Ok(())
}

fn set_configured_mem_ranges(controller: &mut PcieController) {
    for (index, (address, size)) in ax_config::devices::PCI_RANGES.iter().copied().enumerate() {
        if size == 0 || !is_mapped_mmio_range(address, size) {
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

fn is_mapped_mmio_range(address: usize, size: usize) -> bool {
    let Some(end) = address.checked_add(size) else {
        return false;
    };

    mmio_ranges().iter().any(|&(mapped_start, mapped_size)| {
        let Some(mapped_end) = mapped_start.checked_add(mapped_size) else {
            return false;
        };
        mapped_start <= address && end <= mapped_end
    })
}

pub(super) fn take_virtio_transport(
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

fn as_device_function(address: rdrive::probe::pci::PciAddress) -> DeviceFunction {
    DeviceFunction {
        bus: address.bus(),
        device: address.device(),
        function: address.function(),
    }
}

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

struct EndpointConfigAccess {
    bdf: DeviceFunction,
    endpoint: Arc<Mutex<Endpoint>>,
}

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
