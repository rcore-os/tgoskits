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

use super::virtio::{VirtIoBlkDevice, VirtIoHalImpl, register_virtio_block};

module_driver!(
    name: "Virtio PCI Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci { on_probe: probe }],
);

fn probe(endpoint: &mut EndpointRc, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    match (endpoint.vendor_id(), endpoint.device_id()) {
        (0x1af4, 0x1001 | 0x1042) => {}
        _ => return Err(OnProbeError::NotMatch),
    }

    let bdf = as_device_function(endpoint.address());
    let dev_info = as_device_function_info(endpoint);
    let mut root = PciRoot::new(EndpointConfigAccess::new(bdf, endpoint.take()));

    let (ty, transport, _irq) =
        ax_driver_virtio::probe_pci_device::<VirtIoHalImpl, _>(&mut root, bdf, &dev_info)
            .ok_or(OnProbeError::NotMatch)?;

    if ty != DeviceType::Block {
        return Err(OnProbeError::NotMatch);
    }

    let dev = VirtIoBlkDevice::try_new(transport).map_err(|err| {
        OnProbeError::other(format!(
            "failed to initialize Virtio PCI block device at {bdf}: {err:?}"
        ))
    })?;

    register_virtio_block(plat_dev, dev);
    debug!("virtio PCI block device registered successfully at {bdf}");
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
