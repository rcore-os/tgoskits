extern crate alloc;

use alloc::format;

use rdrive::{
    PlatformDevice, module_driver,
    probe::{OnProbeError, pci::EndpointRc},
};
use virtio_drivers::transport::DeviceType;

use super::virtio::{VirtIoBlkDevice, register_virtio_block};

module_driver!(
    name: "Virtio PCI Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci { on_probe: probe }],
);

fn probe(endpoint: &mut EndpointRc, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let address = endpoint.address();
    let transport = ax_drivers::pci::take_virtio_transport(endpoint, DeviceType::Block)?;
    let dev = VirtIoBlkDevice::new(transport).map_err(|err| {
        OnProbeError::other(format!(
            "failed to initialize Virtio PCI block device at {address:?}: {err:?}"
        ))
    })?;

    register_virtio_block(plat_dev, dev);
    debug!("virtio PCI block device registered successfully at {address:?}");
    Ok(())
}
