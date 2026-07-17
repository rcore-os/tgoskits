//! PCI/MMIO discovery and unresolved controller registration.

use alloc::{string::ToString, sync::Arc};

use rdrive::{PlatformDevice, probe::OnProbeError};
use virtio_drivers::transport::DeviceType;

use super::{controller::BlockDevice, device::VirtIoBlkDevice};
use crate::{
    BindingInfo, binding_info_from_fdt,
    block::{PlatformDeviceBlock, validate_block_interface_irq_bindings},
    virtio::VirtIoTransport,
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci_endpoint};

#[cfg(feature = "pci")]
crate::model_register!(
    name: "VirtIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

#[cfg(feature = "pci")]
fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    let info = binding_info_from_pci_endpoint(
        probe.info(),
        probe.endpoint(),
        PciIrqRequirement::Required,
    )?;
    let (transport, irq_lease) =
        crate::pci::take_virtio_block_transport(probe.endpoint_mut(), DeviceType::Block, info)?;
    register_transport_with_irq_lease(probe.into_platform_device(), transport, irq_lease)
}

crate::model_register!(
    name: "VirtIO MMIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["virtio,mmio"],
        on_probe: probe_fdt,
    }],
);

fn probe_fdt(probe: rdrive::register::ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let binding_info = binding_info_from_fdt(&info)?;
    let (ty, transport) = crate::virtio::probe_fdt_mmio_device(&info)?;
    if ty != DeviceType::Block {
        return Err(OnProbeError::NotMatch);
    }
    register_transport_with_info(plat_dev, transport, binding_info)
}

/// Registers a command-free VirtIO block discovery object.
pub fn register_transport<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    register_transport_with_info(plat_dev, transport, BindingInfo::empty())
}

fn register_transport_with_info<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    info: BindingInfo,
) -> Result<(), OnProbeError> {
    let dev = Arc::new(VirtIoBlkDevice::discovered(transport));
    let mut block = BlockDevice::discovered(dev);
    validate_block_interface_irq_bindings(&mut block, &info)
        .map_err(|error| OnProbeError::other(error.to_string()))?;
    plat_dev.register_block_with_info(block, info);
    log::info!("discovered virtio block controller");
    Ok(())
}

#[cfg(feature = "pci")]
fn register_transport_with_irq_lease<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    irq_lease: crate::pci::PciIntxIrqLease,
) -> Result<(), OnProbeError> {
    let dev = Arc::new(VirtIoBlkDevice::discovered(transport));
    plat_dev.register_irq_bound_block(BlockDevice::discovered(dev), irq_lease);
    log::info!("discovered PCI virtio block controller with retained INTx lease");
    Ok(())
}
