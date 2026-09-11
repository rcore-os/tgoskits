//! LoongArch QEMU virt PCI host provider and runtime adapters.

use std::sync::Arc;

use axdevice::*;
use axdevice_base::{ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger};

pub(super) const PCI_HOST_NODE: &str = "pci-host";
pub(super) const PCI_CONFIG_BASE: u64 = 0x2000_0000;
pub(super) const PCI_CONFIG_SIZE: u64 = 0x0800_0000;
pub(super) const PCI_MEMORY_BASE: u64 = 0x4000_0000;
pub(super) const PCI_MEMORY_SIZE: u64 = 0x4000_0000;
const CONFIG_SLOT: &str = "config-space";
const MEMORY_SLOT: &str = "memory-aperture";

pub(super) fn host_key() -> PciHostKey {
    PciHostKey::new("loongarch-qemu-virt").expect("static LoongArch PCI host key is valid")
}

pub(super) fn provider() -> DeviceManagerResult<PciHostProvider> {
    let host_id = DeviceNodeId::new(PCI_HOST_NODE)?;
    let model: Arc<dyn DeviceModel> = Arc::new(LoongArchPciHostModel {
        host_id: host_id.clone(),
    });
    let node = DeviceNodeSpec::virtual_device(host_id, model);
    Ok(
        PciHostProvider::new(host_key(), node, ResourceSlot::new(MEMORY_SLOT)?).with_intx_router(
            PciIntxRouter::new(
                InterruptControllerId::new(0),
                [
                    ControllerInputId::new(16),
                    ControllerInputId::new(17),
                    ControllerInputId::new(18),
                    ControllerInputId::new(19),
                ],
                [80, 81, 82, 83],
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Shared,
            )
            .with_controller_dependency(DeviceNodeId::new("pch-pic")?),
        ),
    )
}

struct LoongArchPciHostModel {
    host_id: DeviceNodeId,
}

impl DeviceModel for LoongArchPciHostModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new()
            .with_mmio(
                ResourceSlot::new(CONFIG_SLOT)?,
                PCI_CONFIG_SIZE,
                PCI_CONFIG_SIZE,
                ResourceRequest::Fixed(PCI_CONFIG_BASE),
            )?
            .with_mmio(
                ResourceSlot::new(MEMORY_SLOT)?,
                PCI_MEMORY_SIZE,
                PCI_MEMORY_SIZE,
                ResourceRequest::Fixed(PCI_MEMORY_BASE),
            )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        let config = ResourceSlot::new(CONFIG_SLOT).expect("static slot is valid");
        let memory = ResourceSlot::new(MEMORY_SLOT).expect("static slot is valid");
        DeviceFirmwareSpec::interfaces(
            Some(std::vec![FdtContributionSpec::PciHostBridge(
                FdtNodeSpec::new("pcie")
                    .with_compatible("pci-host-ecam-generic")
                    .with_register(config.clone())
                    .with_register(memory.clone())
                    .with_empty_property("dma-coherent"),
            )]),
            Some(std::vec![AcpiContributionSpec::PciHostBridge(
                AcpiDeviceSpec::new("PCI0", "PNP0A08")
                    .with_register(config)
                    .with_register(memory),
            )]),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let config = context.mmio(CONFIG_SLOT)?;
        let memory = context.mmio(MEMORY_SLOT)?;
        if config != (PCI_CONFIG_BASE, PCI_CONFIG_SIZE)
            || memory != (PCI_MEMORY_BASE, PCI_MEMORY_SIZE)
        {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build LoongArch PCI host",
                detail: "resolved PCI host resources differ from the QEMU virt provider".into(),
            });
        }
        let topology = context
            .pci_host_topology()
            .ok_or_else(|| DeviceManagerError::InvalidState {
                operation: "build LoongArch PCI host",
                detail: "resolved graph did not attach PCI topology metadata".into(),
            })?
            .clone();
        let root = Arc::new(PciRootState::new(topology));
        let binding = Arc::new(PciRootBinding::new(self.host_id.clone(), root));
        let mut bundle = DeviceBundle::new();
        bundle.add_device(Arc::new(PciEcamConfigFrontend::new(
            config.0,
            config.1,
            binding.clone(),
        )));
        bundle.add_device(Arc::new(PciMemoryApertureDevice::new(
            memory.0,
            memory.1,
            binding.clone(),
        )));
        bundle.add_lifecycle(Arc::new(PciRootLifecycle::new(binding.clone())));
        bundle.provide_service::<PciRootBindingKey>(binding)?;
        Ok(bundle)
    }
}
