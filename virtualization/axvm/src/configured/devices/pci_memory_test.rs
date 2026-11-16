//! Configured private PCI memory test endpoint with one memory BAR.
//!
//! This model intentionally implements only a private, zero-initialized BAR2
//! aperture. Shared backing, control registers, peer notification, and MSI-X
//! remain outside this test-only vPCI registration path.

use std::{
    sync::{Arc, Mutex},
    vec::Vec,
};

use axdevice::*;
use axdevice_base::{Device, DeviceAccess, DeviceContext, DeviceError, DeviceResult, Resource};
use axvmconfig::VirtualDeviceRequest;

use crate::{ConfiguredDeviceError, ConfiguredModelRegistration, DeviceInstantiationContext};

const MODEL: &str = "pci-memory-test";
const HOST_KEY: &str = "aarch64-ecam";
// These are repository-internal PCI test placeholder IDs, not a PCI-SIG
// assignment. This endpoint is not ivshmem or any other production device.
const TEST_VENDOR_ID: u16 = 0x1234;
const TEST_DEVICE_ID: u16 = 0x5678;
const PRIVATE_MEMORY_BAR_INDEX: u8 = 2;
const PRIVATE_MEMORY_SIZE: usize = 0x1_0000;

fn host_key() -> PciHostKey {
    // This module is architecture-neutral while the provider is AArch64-only.
    // A mismatch fails during typed graph declaration rather than silently
    // attaching the endpoint to another host.
    PciHostKey::new(HOST_KEY).expect("static AArch64 PCI host key is valid")
}

const REGISTRATION: ConfiguredModelRegistration = ConfiguredModelRegistration {
    model: MODEL,
    create: create_device_node,
};

pub(super) fn register(
    catalog: &mut crate::ConfiguredDeviceCatalog,
) -> Result<(), ConfiguredDeviceError> {
    catalog.register(module_path!(), REGISTRATION)
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PciMemoryTestOptions {}

fn create_device_node(
    id: DeviceNodeId,
    request: &VirtualDeviceRequest,
    _context: &DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
    request
        .deserialize_options::<PciMemoryTestOptions>()
        .map_err(|error| ConfiguredDeviceError::InvalidOptions {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: error.to_string(),
        })?;
    Ok(DeviceNodeSpec::virtual_device(
        id,
        Arc::new(PciMemoryTestModel),
    ))
}

struct PciMemoryTestModel;

impl DeviceModel for PciMemoryTestModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        let bar = PciMemoryBar::new(
            PciBarIndex::new(PRIVATE_MEMORY_BAR_INDEX)?,
            PRIVATE_MEMORY_SIZE as u64,
        )?;
        let function = PciFunctionRequirement::new(
            host_key(),
            PciEndpointIdentity::new(
                TEST_VENDOR_ID,
                TEST_DEVICE_ID,
                PciClass::new(0x05, 0x00, 0x00),
            ),
        )
        .with_bar(bar)?;
        DeviceRequirements::new().with_pci_function(function)
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::None
    }

    fn build(&self, _context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let function = Arc::new(PciMemoryTestFunction::new()?);
        let mut bundle = DeviceBundle::new();
        bundle.add_pci_function(function)?;
        Ok(bundle)
    }
}

struct PciMemoryTestFunction {
    bytes: Mutex<Box<[u8]>>,
}

impl PciMemoryTestFunction {
    fn new() -> DeviceManagerResult<Self> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(PRIVATE_MEMORY_SIZE).map_err(|_| {
            DeviceManagerError::OutOfMemory {
                operation: "allocate private PCI memory test BAR2 backing",
            }
        })?;
        bytes.resize(PRIVATE_MEMORY_SIZE, 0);
        Ok(Self {
            bytes: Mutex::new(bytes.into_boxed_slice()),
        })
    }

    fn access_range(access: PciBarAccess) -> DeviceResult<core::ops::Range<usize>> {
        if access.bar().value() != PRIVATE_MEMORY_BAR_INDEX {
            return Err(DeviceError::OutOfRange {
                addr: access.offset(),
            });
        }
        let start = usize::try_from(access.offset()).map_err(|_| DeviceError::OutOfRange {
            addr: access.offset(),
        })?;
        let end = start
            .checked_add(access.width().size())
            .filter(|end| *end <= PRIVATE_MEMORY_SIZE)
            .ok_or(DeviceError::OutOfRange {
                addr: access.offset(),
            })?;
        Ok(start..end)
    }
}

impl Device for PciMemoryTestFunction {
    fn name(&self) -> &str {
        MODEL
    }

    fn resources(&self) -> &[Resource] {
        &[]
    }

    fn read(&self, _access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        Err(DeviceError::Unsupported {
            operation: "access private PCI memory test endpoint",
            detail: "direct access is routed through BAR2".into(),
        })
    }

    fn write(
        &self,
        _access: &DeviceAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        Err(DeviceError::Unsupported {
            operation: "access private PCI memory test endpoint",
            detail: "direct access is routed through BAR2".into(),
        })
    }
}

impl PciFunction for PciMemoryTestFunction {
    fn read_bar(
        &self,
        access: PciBarAccess,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult<u64> {
        let range = Self::access_range(access)?;
        let bytes = self.bytes.lock().map_err(|_| DeviceError::InvalidState {
            operation: "read private PCI memory test BAR2",
            detail: "BAR backing lock is poisoned".into(),
        })?;
        let mut value = [0u8; 8];
        value[..range.len()].copy_from_slice(&bytes[range]);
        Ok(u64::from_le_bytes(value))
    }

    fn write_bar(
        &self,
        access: PciBarAccess,
        value: u64,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        let range = Self::access_range(access)?;
        let width = range.len();
        let mut bytes = self.bytes.lock().map_err(|_| DeviceError::InvalidState {
            operation: "write private PCI memory test BAR2",
            detail: "BAR backing lock is poisoned".into(),
        })?;
        bytes[range].copy_from_slice(&value.to_le_bytes()[..width]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axdevice_base::AccessWidth;

    use super::*;

    const APERTURE_BASE: u64 = 0x0c00_0000;
    const APERTURE_SIZE: u64 = 0x0400_0000;

    fn id(value: &str) -> DeviceNodeId {
        DeviceNodeId::new(value).unwrap()
    }

    fn slot(value: &str) -> ResourceSlot {
        ResourceSlot::new(value).unwrap()
    }

    #[test]
    fn requirements_declare_aarch64_bar2_function() {
        let requirements = PciMemoryTestModel.requirements().unwrap();
        let function = requirements.pci_function().unwrap();
        let expected = PciFunctionRequirement::new(
            host_key(),
            PciEndpointIdentity::new(TEST_VENDOR_ID, TEST_DEVICE_ID, PciClass::new(0x05, 0, 0)),
        )
        .with_bar(
            PciMemoryBar::new(
                PciBarIndex::new(PRIVATE_MEMORY_BAR_INDEX).unwrap(),
                PRIVATE_MEMORY_SIZE as u64,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(function, &expected);
    }

    #[test]
    fn backing_is_zeroed_per_endpoint() {
        let first = PciMemoryTestFunction::new().unwrap();
        let second = PciMemoryTestFunction::new().unwrap();
        first.bytes.lock().unwrap()[0] = 0xa5;
        assert_eq!(second.bytes.lock().unwrap()[0], 0);
    }

    struct HostModel {
        root: Arc<Mutex<Option<Arc<PciRootState>>>>,
    }

    impl DeviceModel for HostModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            DeviceRequirements::new().with_mmio(
                slot("pci-memory"),
                APERTURE_SIZE,
                APERTURE_SIZE,
                ResourceRequest::Auto,
            )
        }

        fn firmware(&self) -> DeviceFirmwareSpec {
            DeviceFirmwareSpec::None
        }

        fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
            let _ = context.mmio("pci-memory")?;
            let topology =
                context
                    .pci_host_topology()
                    .cloned()
                    .ok_or(DeviceManagerError::InvalidState {
                        operation: "build private PCI memory test host",
                        detail: "test host topology was not resolved".into(),
                    })?;
            let root = Arc::new(PciRootState::new(topology));
            *self.root.lock().unwrap() = Some(root.clone());
            let binding = Arc::new(PciRootBinding::new(id("pci-host"), root));
            DeviceBundle::new().with_service::<PciRootBindingKey>(binding)
        }
    }

    #[test]
    fn graph_build_routes_private_bar_backing() {
        let root_slot = Arc::new(Mutex::new(None));
        let provider = PciHostProvider::new(
            host_key(),
            DeviceNodeSpec::virtual_device(
                id("pci-host"),
                Arc::new(HostModel {
                    root: root_slot.clone(),
                }),
            ),
            slot("pci-memory"),
        );
        let mut builder = DeviceGraphBuilder::new();
        builder.register_pci_host(provider).unwrap();
        builder
            .add(DeviceNodeSpec::virtual_device(
                id("pci-memory"),
                Arc::new(PciMemoryTestModel),
            ))
            .unwrap();
        let mut pools = ResourcePools::new();
        pools
            .add_auto_mmio(APERTURE_BASE..APERTURE_BASE + APERTURE_SIZE)
            .unwrap();
        let graph = builder.declare().unwrap().resolve(pools).unwrap();
        let mut runtime_builder = DeviceRuntimeBuilder::new(RuntimeAccessPorts::new());
        for node in graph.nodes() {
            runtime_builder
                .build_graph_node(node, graph.resource_plan())
                .unwrap();
        }
        let runtime = runtime_builder.finish(graph.resource_plan()).unwrap();
        let root = root_slot.lock().unwrap().clone().unwrap();
        let binding = runtime
            .services()
            .all::<PciRootBindingKey>()
            .into_iter()
            .next()
            .unwrap();
        let function = graph
            .pci_topology(&host_key())
            .unwrap()
            .function(&id("pci-memory"))
            .unwrap();
        let bar = function
            .bar(PciBarIndex::new(PRIVATE_MEMORY_BAR_INDEX).unwrap())
            .unwrap();
        root.write_config(
            function.bdf(),
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
            2,
        )
        .unwrap();

        binding
            .write_bar(
                bar.address() + 0x120,
                AccessWidth::Qword,
                0x3154_5345_544d_5043,
            )
            .unwrap();
        assert_eq!(
            binding
                .read_bar(bar.address() + 0x120, AccessWidth::Qword)
                .unwrap(),
            0x3154_5345_544d_5043
        );
        assert_eq!(
            binding.read_bar(
                bar.address() + PRIVATE_MEMORY_SIZE as u64,
                AccessWidth::Byte
            ),
            Err(DeviceError::NotFound)
        );
    }
}
