//! LoongArch QEMU virt PCI host provider and runtime adapters.

use std::sync::Arc;

use axdevice::*;
use axdevice_base::{ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger};

pub(super) const PCI_HOST_NODE: &str = "pci-host";
pub(super) const PCI_CONFIG_BASE: u64 = 0x2000_0000;
pub(super) const PCI_CONFIG_SIZE: u64 = 0x0800_0000;
pub(super) const PCI_MEMORY_BASE: u64 = 0x4000_0000;
pub(super) const PCI_MEMORY_SIZE: u64 = 0x4000_0000;
pub(super) const PCI_IO_BASE: u64 = 0x1800_0000;
pub(super) const PCI_IO_SIZE: u64 = 0x0001_0000;
const IO_SLOT: &str = "io-aperture";
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
            )?
            .with_mmio(
                ResourceSlot::new(IO_SLOT)?,
                PCI_IO_SIZE,
                PCI_IO_SIZE,
                ResourceRequest::Fixed(PCI_IO_BASE),
            )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        let config = ResourceSlot::new(CONFIG_SLOT).expect("static slot is valid");
        let memory = ResourceSlot::new(MEMORY_SLOT).expect("static slot is valid");
        let io = ResourceSlot::new(IO_SLOT).expect("static slot is valid");
        DeviceFirmwareSpec::interfaces(
            Some(std::vec![FdtContributionSpec::PciHostBridge(
                FdtNodeSpec::new("pcie")
                    .with_compatible("pci-host-ecam-generic")
                    .with_register(config.clone())
                    .with_register(memory.clone())
                    .with_register(io.clone())
                    .with_empty_property("dma-coherent"),
            )]),
            Some(std::vec![AcpiContributionSpec::PciHostBridge(
                AcpiDeviceSpec::new("PCI0", "PNP0A08")
                    .with_register(config)
                    .with_register(memory)
                    .with_register(io),
            )]),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let config = context.mmio(CONFIG_SLOT)?;
        let memory = context.mmio(MEMORY_SLOT)?;
        let io = context.mmio(IO_SLOT)?;
        if config != (PCI_CONFIG_BASE, PCI_CONFIG_SIZE)
            || memory != (PCI_MEMORY_BASE, PCI_MEMORY_SIZE)
            || io != (PCI_IO_BASE, PCI_IO_SIZE)
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
        bundle.add_device(Arc::new(PciEcamConfigFrontend::try_new(
            config.0,
            config.1,
            binding.clone(),
        )?));
        bundle.add_device(Arc::new(PciMemoryApertureDevice::new(
            memory.0,
            memory.1,
            binding.clone(),
        )));
        bundle.add_device(Arc::new(EmptyPciIoAperture {
            resource: [axdevice_base::Resource::MmioRange {
                base: io.0,
                size: io.1,
            }],
        }));
        bundle.add_lifecycle(Arc::new(PciRootLifecycle::new(binding.clone())));
        bundle.provide_service::<PciRootBindingKey>(binding)?;
        Ok(bundle)
    }
}

/// This root supports memory BARs only. Its advertised I/O aperture therefore
/// owns accesses to absent legacy devices: reads return an open bus and writes
/// have no effect. It never forwards I/O addresses to memory-BAR endpoints.
struct EmptyPciIoAperture {
    resource: [axdevice_base::Resource; 1],
}

impl axdevice_base::Device for EmptyPciIoAperture {
    fn name(&self) -> &str {
        "pci-io-aperture"
    }
    fn resources(&self) -> &[axdevice_base::Resource] {
        &self.resource
    }
    fn read(
        &self,
        access: &axdevice_base::DeviceAccess,
        _context: &mut dyn axdevice_base::DeviceContext,
    ) -> axdevice_base::DeviceResult<u64> {
        Ok(match access.width() {
            axdevice_base::AccessWidth::Byte => u8::MAX as u64,
            axdevice_base::AccessWidth::Word => u16::MAX as u64,
            axdevice_base::AccessWidth::Dword => u32::MAX as u64,
            axdevice_base::AccessWidth::Qword => u64::MAX,
        })
    }
    fn write(
        &self,
        _access: &axdevice_base::DeviceAccess,
        _value: u64,
        _context: &mut dyn axdevice_base::DeviceContext,
    ) -> axdevice_base::DeviceResult {
        Ok(())
    }
}
