//! x86 Q35 PCI host provider and runtime adapters.

use std::sync::Arc;

use axdevice::*;

pub(super) const PCI_HOST_NODE: &str = "pci-host";
pub(super) const PCI_MEMORY_BASE: u64 = 0xc000_0000;
pub(super) const PCI_MEMORY_SIZE: u64 = 0x1000_0000;
const CONFIG_SLOT: &str = "config-ports";
const MEMORY_SLOT: &str = "memory-aperture";

pub(super) fn host_key() -> PciHostKey {
    PciHostKey::new("x86-q35").expect("static x86 PCI host key is valid")
}

pub(super) fn provider() -> DeviceManagerResult<PciHostProvider> {
    let host_id = DeviceNodeId::new(PCI_HOST_NODE)?;
    let model: Arc<dyn DeviceModel> = Arc::new(X86PciHostModel {
        host_id: host_id.clone(),
    });
    let node = DeviceNodeSpec::virtual_device(host_id.clone(), model);
    let provider = PciHostProvider::new(host_key(), node, ResourceSlot::new(MEMORY_SLOT)?)
        .with_platform_function(q35_host_function(host_id.clone())?)?
        .with_platform_function(lpc_function()?)?;
    Ok(provider)
}

struct X86PciHostModel {
    host_id: DeviceNodeId,
}

impl DeviceModel for X86PciHostModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new()
            .with_pio(
                ResourceSlot::new(CONFIG_SLOT)?,
                X86PciConfigFrontend::PORT_SIZE,
                1,
                ResourceRequest::Fixed(X86PciConfigFrontend::PORT_BASE),
            )?
            .with_mmio(
                ResourceSlot::new(MEMORY_SLOT)?,
                PCI_MEMORY_SIZE,
                PCI_MEMORY_SIZE,
                ResourceRequest::Fixed(PCI_MEMORY_BASE),
            )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::interfaces(
            None,
            Some(std::vec![AcpiContributionSpec::PciHostBridge(
                AcpiDeviceSpec::new("PCI0", "PNP0A03")
                    .with_register(ResourceSlot::new(CONFIG_SLOT).expect("static slot is valid"))
                    .with_register(ResourceSlot::new(MEMORY_SLOT).expect("static slot is valid")),
            )]),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let config = context.pio(CONFIG_SLOT)?;
        let memory = context.mmio(MEMORY_SLOT)?;
        if config
            != (
                X86PciConfigFrontend::PORT_BASE,
                X86PciConfigFrontend::PORT_SIZE,
            )
            || memory != (PCI_MEMORY_BASE, PCI_MEMORY_SIZE)
        {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build x86 PCI host",
                detail: "resolved PCI host resources differ from the Q35 provider".into(),
            });
        }
        let topology = context
            .pci_host_topology()
            .ok_or_else(|| DeviceManagerError::InvalidState {
                operation: "build x86 PCI host",
                detail: "resolved graph did not attach PCI topology metadata".into(),
            })?
            .clone();
        let root = Arc::new(PciRootState::new(topology));
        let binding = Arc::new(PciRootBinding::new(self.host_id.clone(), root.clone()));
        let mut bundle = DeviceBundle::new();
        bundle.add_device(Arc::new(X86PciConfigFrontend::new(root.clone())));
        bundle.add_device(Arc::new(PciMemoryApertureDevice::new(
            memory.0,
            memory.1,
            binding.clone(),
        )));
        bundle.add_lifecycle(Arc::new(PciRootLifecycle::new(root)));
        bundle.provide_service::<PciRootBindingKey>(binding)?;
        Ok(bundle)
    }
}

fn q35_host_function(id: DeviceNodeId) -> PciResult<PciFunctionSpec> {
    PciFunctionSpec::new(
        id,
        PciEndpointIdentity::new(0x8086, 0x29c0, PciClass::new(0x06, 0x00, 0x00)),
    )
    .with_bdf(ResourceRequest::Fixed(PciBdf::new(
        PciSegment::new(0),
        0,
        0,
        0,
    )?))
    .with_platform_config_byte(ConfigOffset::new(4)?, 0, 0x07)
}

fn lpc_function() -> PciResult<PciFunctionSpec> {
    let id = DeviceNodeId::new("q35-lpc").expect("static Q35 LPC identity is valid");
    PciFunctionSpec::new(
        id,
        PciEndpointIdentity::new(0x8086, 0x2918, PciClass::new(0x06, 0x01, 0x00)),
    )
    .with_bdf(ResourceRequest::Fixed(PciBdf::new(
        PciSegment::new(0),
        0,
        0x1f,
        0,
    )?))
    .with_platform_config_byte(ConfigOffset::new(4)?, 0, 0x07)?
    .with_platform_config_byte(ConfigOffset::new(0x0e)?, 0x80, 0)?
    .with_platform_config_byte(ConfigOffset::new(0x40)?, 0x01, 0x80)?
    .with_platform_config_byte(ConfigOffset::new(0x41)?, 0, 0xff)?
    .with_platform_config_byte(ConfigOffset::new(0x44)?, 0, 0x87)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_builds_the_host_and_platform_functions_without_an_endpoint() {
        let mut graph = DeviceGraphBuilder::new();
        graph.register_pci_host(provider().unwrap()).unwrap();
        let mut pools = ResourcePools::new();
        pools
            .allow_fixed_pio(
                X86PciConfigFrontend::PORT_BASE
                    ..X86PciConfigFrontend::PORT_BASE + X86PciConfigFrontend::PORT_SIZE,
            )
            .unwrap();
        pools
            .allow_fixed_mmio(PCI_MEMORY_BASE..PCI_MEMORY_BASE + PCI_MEMORY_SIZE)
            .unwrap();
        let graph = graph.declare().unwrap().resolve(pools).unwrap();
        let topology = graph.pci_topology(&host_key()).unwrap();
        assert_eq!(topology.functions().count(), 2);
        assert_eq!(
            topology.memory_aperture(),
            &(PCI_MEMORY_BASE..PCI_MEMORY_BASE + PCI_MEMORY_SIZE)
        );

        let mut runtime = DeviceRuntimeBuilder::new(RuntimeAccessPorts::new());
        for node in graph.nodes() {
            runtime
                .build_graph_node(node, graph.resource_plan())
                .unwrap();
        }
        runtime.finish(graph.resource_plan()).unwrap();
    }
}
