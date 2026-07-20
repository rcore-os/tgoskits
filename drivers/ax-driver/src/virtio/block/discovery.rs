//! PCI/MMIO discovery and unresolved controller registration.

use alloc::{format, string::ToString, sync::Arc};

use rdrive::{PlatformDevice, probe::OnProbeError};
use virtio_drivers::transport::DeviceType;

use super::{controller::BlockDevice, device::VirtIoBlkDevice, irq::VirtioInterruptPort};
use crate::{
    BindingInfo, binding_info_from_fdt,
    block::{PlatformDeviceBlock, validate_block_interface_irq_bindings},
    virtio::VirtIoTransport,
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci_endpoint};

#[cfg(feature = "pci")]
const PCI_VIRTIO_ISR_CONFIG_TYPE: u8 = 3;
#[cfg(feature = "pci")]
const PCI_VIRTIO_ISR_CAP_MIN_LENGTH: u8 = 16;
#[cfg(feature = "pci")]
const PCI_CAP_BAR_OFFSET: u16 = 4;
#[cfg(feature = "pci")]
const PCI_CAP_REGION_OFFSET: u16 = 8;
#[cfg(feature = "pci")]
const PCI_CAP_REGION_LENGTH: u16 = 12;

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
    crate::pci::ensure_virtio_pci_endpoint(probe.endpoint(), DeviceType::Block)?;
    let info = binding_info_from_pci_endpoint(
        probe.info(),
        probe.endpoint(),
        PciIrqRequirement::Required,
    )?;
    let interrupt_port = pci_interrupt_port(probe.endpoint())?;
    let (transport, irq_lease) =
        crate::pci::take_virtio_block_transport(probe.endpoint_mut(), DeviceType::Block, info)?;
    register_transport_with_irq_lease(
        probe.into_platform_device(),
        transport,
        interrupt_port,
        irq_lease,
    )
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
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let mapped_size = reg.size.unwrap_or(0x1000) as usize;
    let mapping = axklib::mmio::ioremap((reg.address as usize).into(), mapped_size)
        .map_err(|error| OnProbeError::other(format!("{error:?}")))?;
    let (ty, transport) = crate::virtio::probe_mmio_device(mapping.as_ptr(), mapped_size)
        .ok_or(OnProbeError::NotMatch)?;
    if ty != DeviceType::Block {
        return Err(OnProbeError::NotMatch);
    }
    let interrupt_port = VirtioInterruptPort::from_mmio(mapping)
        .map_err(|error| OnProbeError::other(error.to_string()))?;
    register_transport_with_info(plat_dev, transport, interrupt_port, binding_info)
}

/// Rejects a transport whose destructive IRQ capability was already erased.
///
/// Callers that still own the MMIO/PCI interrupt-status capability must use
/// [`register_transport_with_interrupt_port`]. This fail-closed compatibility
/// entry prevents a fallback to `Transport::ack_interrupt(&mut self)`.
pub fn register_transport<T: VirtIoTransport>(
    _plat_dev: PlatformDevice,
    _transport: T,
) -> Result<(), OnProbeError> {
    Err(OnProbeError::other(
        "virtio block registration requires an independent interrupt port",
    ))
}

/// Registers a command-free transport with its split IRQ capability intact.
pub fn register_transport_with_interrupt_port<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    interrupt_port: VirtioInterruptPort,
) -> Result<(), OnProbeError> {
    register_transport_with_info(plat_dev, transport, interrupt_port, BindingInfo::empty())
}

fn register_transport_with_info<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    interrupt_port: VirtioInterruptPort,
    info: BindingInfo,
) -> Result<(), OnProbeError> {
    let dev = Arc::new(VirtIoBlkDevice::discovered(transport));
    let mut block = BlockDevice::discovered(dev, interrupt_port);
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
    interrupt_port: VirtioInterruptPort,
    irq_lease: crate::pci::PciIntxIrqLease,
) -> Result<(), OnProbeError> {
    let dev = Arc::new(VirtIoBlkDevice::discovered(transport));
    plat_dev.register_irq_bound_block(BlockDevice::discovered(dev, interrupt_port), irq_lease);
    log::info!("discovered PCI virtio block controller with retained INTx lease");
    Ok(())
}

#[cfg(feature = "pci")]
fn pci_interrupt_port(
    endpoint: &rdrive::probe::pci::Endpoint,
) -> Result<VirtioInterruptPort, OnProbeError> {
    use rdrive::probe::pci::PciCapability;

    for capability in endpoint.capabilities() {
        let PciCapability::Vendor(address) = capability else {
            continue;
        };
        let header = endpoint.read(address.offset);
        if (header >> 24) as u8 != PCI_VIRTIO_ISR_CONFIG_TYPE {
            continue;
        }
        let capability_length = (header >> 16) as u8;
        if capability_length < PCI_VIRTIO_ISR_CAP_MIN_LENGTH {
            return Err(OnProbeError::other(
                "virtio PCI ISR capability is shorter than its fixed fields",
            ));
        }
        let bar = endpoint.read(address.offset + PCI_CAP_BAR_OFFSET) as u8;
        if bar >= 6 {
            return Err(OnProbeError::other(format!(
                "virtio PCI ISR capability names invalid BAR {bar}"
            )));
        }
        let region_offset = endpoint.read(address.offset + PCI_CAP_REGION_OFFSET) as usize;
        let region_length = endpoint.read(address.offset + PCI_CAP_REGION_LENGTH) as usize;
        if region_length == 0 {
            return Err(OnProbeError::other(
                "virtio PCI ISR capability has zero length",
            ));
        }
        let bar_range = endpoint.bar_mmio(bar).ok_or_else(|| {
            OnProbeError::other(format!("virtio PCI ISR capability names invalid BAR {bar}"))
        })?;
        let isr_phys = bar_range
            .start
            .checked_add(region_offset)
            .filter(|start| {
                start
                    .checked_add(region_length)
                    .is_some_and(|end| end <= bar_range.end)
            })
            .ok_or_else(|| OnProbeError::other("virtio PCI ISR capability exceeds its BAR"))?;
        let isr_mapping = axklib::mmio::ioremap(isr_phys.into(), region_length)
            .map_err(|error| OnProbeError::other(format!("{error:?}")))?;
        return VirtioInterruptPort::from_pci_isr(isr_mapping)
            .map_err(|error| OnProbeError::other(error.to_string()));
    }
    Err(OnProbeError::other(
        "virtio PCI transport has no ISR capability",
    ))
}

/// Builds an interrupt port for a statically mapped VirtIO MMIO transport.
pub fn register_mmio_transport<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    mapping: mmio_api::Mmio,
) -> Result<(), OnProbeError> {
    let interrupt_port = VirtioInterruptPort::from_mmio(mapping)
        .map_err(|error| OnProbeError::other(error.to_string()))?;
    register_transport_with_interrupt_port(plat_dev, transport, interrupt_port)
}
