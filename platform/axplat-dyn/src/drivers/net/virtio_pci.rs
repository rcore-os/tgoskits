extern crate alloc;

use alloc::{format, sync::Arc};

use ax_driver_base::DeviceType;
use ax_driver_virtio::pci::{
    ConfigurationAccess, DeviceFunction, DeviceFunctionInfo, HeaderType, PciRoot,
};
use rdrive::{
    PlatformDevice, module_driver,
    probe::{
        OnProbeError,
        pci::{Endpoint, EndpointRc},
    },
};
use spin::Mutex;

use super::PlatformDeviceNetDriver;
use crate::drivers::virtio::VirtIoHalImpl;

const DRIVER_NAME: &str = "virtio-net-pci";
type VirtIoNetDevice<T> = ax_driver_virtio::VirtIoNetDev<VirtIoHalImpl, T, 64>;

module_driver!(
    name: "Virtio PCI Network",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci { on_probe: probe }],
);

fn probe(endpoint: &mut EndpointRc, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    match (endpoint.vendor_id(), endpoint.device_id()) {
        (0x1af4, 0x1000 | 0x1041) => {}
        _ => return Err(OnProbeError::NotMatch),
    }

    let address = endpoint.address();
    let bdf = as_device_function(address);
    let dev_info = as_device_function_info(endpoint);
    let mut root = PciRoot::new(EndpointConfigAccess::new(bdf, endpoint.take()));

    let (ty, transport, irq) =
        ax_driver_virtio::probe_pci_device::<VirtIoHalImpl, _>(&mut root, bdf, &dev_info)
            .ok_or(OnProbeError::NotMatch)?;
    debug_assert_eq!(irq, super::pci_legacy_irq_for_address(address));

    if ty != DeviceType::Net {
        return Err(OnProbeError::NotMatch);
    }

    let dev = VirtIoNetDevice::try_new(transport, Some(irq)).map_err(|err| {
        OnProbeError::other(format!(
            "failed to initialize Virtio PCI network device at {bdf}: {err:?}"
        ))
    })?;

    plat_dev.register_net_driver(DRIVER_NAME, dev);
    debug!("virtio PCI network device registered successfully at {bdf} with irq {irq:#x}");
    Ok(())
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
