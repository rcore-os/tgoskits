//! VM-local device graphs created by architecture-owned initialization.

mod passthrough;
mod pools;

use axdevice::{
    DeviceFactoryRegistry, DeviceFirmwareBinding, DeviceGraphBuilder, DeviceManagerError,
    DeviceNodeId, DeviceNodeSpec, ResolvedDeviceGraph, ResourcePools,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::{AxVmResult, config::AxVMConfig};

/// Creates factories for architecture-independent and machine-owned nodes.
pub(crate) fn machine_factory_registry(config: &AxVMConfig) -> AxVmResult<DeviceFactoryRegistry> {
    let mut factories = DeviceFactoryRegistry::new();
    axdevice::register_builtin_factories(&mut factories)?;
    crate::machine::register_machine_device_factories_from_config(config, &mut factories)?;
    Ok(factories)
}

/// One immutable graph and its one-shot resource claims.
pub(crate) struct VmDevicePlan {
    graph: ResolvedDeviceGraph,
}

impl VmDevicePlan {
    #[cfg(test)]
    pub(crate) fn fixed(
        configs: &[EmulatedDeviceConfig],
        factories: DeviceFactoryRegistry,
        controller_type: Option<EmulatedDeviceType>,
        host_replacements: &[EmulatedDeviceType],
    ) -> AxVmResult<Self> {
        Self::with_pools(
            configs,
            factories,
            controller_type,
            host_replacements,
            ResourcePools::new(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_pools(
        configs: &[EmulatedDeviceConfig],
        factories: DeviceFactoryRegistry,
        controller_type: Option<EmulatedDeviceType>,
        host_replacements: &[EmulatedDeviceType],
        mut pools: ResourcePools,
    ) -> AxVmResult<Self> {
        Self::build(
            None,
            configs,
            factories,
            controller_type,
            host_replacements,
            &mut pools,
        )
    }

    pub(crate) fn with_pools_for_vm(
        config: &AxVMConfig,
        configs: &[EmulatedDeviceConfig],
        factories: DeviceFactoryRegistry,
        controller_type: Option<EmulatedDeviceType>,
        host_replacements: &[EmulatedDeviceType],
        mut pools: ResourcePools,
    ) -> AxVmResult<Self> {
        pools::reserve_guest_memory(config, &mut pools)?;
        Self::build(
            Some(config),
            configs,
            factories,
            controller_type,
            host_replacements,
            &mut pools,
        )
    }

    fn build(
        vm_config: Option<&AxVMConfig>,
        configs: &[EmulatedDeviceConfig],
        factories: DeviceFactoryRegistry,
        controller_type: Option<EmulatedDeviceType>,
        host_replacements: &[EmulatedDeviceType],
        pools: &mut ResourcePools,
    ) -> AxVmResult<Self> {
        let mut builder = DeviceGraphBuilder::new();
        let controller_id = controller_type
            .map(|device_type| unique_device_id(configs, device_type))
            .transpose()?;
        for config in configs {
            let factory = factories.get_arc(config.emu_type).ok_or_else(|| {
                DeviceManagerError::Unsupported {
                    operation: "declare VM device graph",
                    detail: alloc::format!(
                        "no factory is registered for device '{}' of type {}",
                        config.name,
                        config.emu_type
                    ),
                }
            })?;
            let id = DeviceNodeId::new(config.name.clone())?;
            let mut node = if host_replacements.contains(&config.emu_type) {
                DeviceNodeSpec::host_replacement(id, config.clone(), factory)
            } else {
                DeviceNodeSpec::virtual_device(id, config.clone(), factory)
            };
            if controller_type != Some(config.emu_type)
                && let Some(controller_id) = controller_id.as_ref()
            {
                node = node.with_dependency(controller_id.clone());
            }
            if let Some(binding) = firmware_binding(vm_config, config.emu_type) {
                node = node.with_firmware_binding(binding);
            }
            builder.add(node).map_err(DeviceManagerError::from)?;
        }

        if let Some(config) = vm_config {
            passthrough::add_host_nodes(config, configs, host_replacements, &mut builder)?;
        }

        let declared = builder.declare().map_err(DeviceManagerError::from)?;
        let requests = declared.requests()?;
        pools::allow_fixed_requirements(&requests, pools)?;
        let graph = declared.resolve(core::mem::take(pools))?;
        Ok(Self { graph })
    }

    pub(crate) const fn graph(&self) -> &ResolvedDeviceGraph {
        &self.graph
    }
}

fn firmware_binding(
    vm: Option<&AxVMConfig>,
    device_type: EmulatedDeviceType,
) -> Option<DeviceFirmwareBinding> {
    let vm = vm?;
    let path = match device_type {
        EmulatedDeviceType::Console => vm
            .serial_fdt_identity()
            .map(|identity| identity.node_path.clone()),
        EmulatedDeviceType::InterruptController | EmulatedDeviceType::GicCpuRegion => vm
            .gic_profile()
            .filter(|profile| !profile.node_path.is_empty())
            .map(|profile| profile.node_path.clone()),
        EmulatedDeviceType::PPPTGlobal => vm
            .plic_profile()
            .filter(|profile| !profile.node_path.is_empty())
            .map(|profile| profile.node_path.clone()),
        _ => None,
    }?;
    Some(DeviceFirmwareBinding::FdtNode(path))
}

fn unique_device_id(
    configs: &[EmulatedDeviceConfig],
    device_type: EmulatedDeviceType,
) -> AxVmResult<DeviceNodeId> {
    let mut matches = configs
        .iter()
        .filter(|config| config.emu_type == device_type);
    let config = matches.next().ok_or_else(|| {
        crate::AxVmError::invalid_config(alloc::format!(
            "device graph has no controller of type {device_type}"
        ))
    })?;
    if matches.next().is_some() {
        return Err(crate::AxVmError::invalid_config(alloc::format!(
            "device graph has more than one controller of type {device_type}"
        )));
    }
    Ok(DeviceNodeId::new(config.name.clone())?)
}

/// Small common capability exposed by every architecture-specific VM plan.
pub(crate) trait ArchitectureVmPlan {
    fn devices(&self) -> &VmDevicePlan;
}

/// Plan used by architectures with no extra immutable controller metadata.
pub(crate) struct SimpleVmPlan(VmDevicePlan);

impl SimpleVmPlan {
    pub(crate) const fn new(devices: VmDevicePlan) -> Self {
        Self(devices)
    }
}

impl ArchitectureVmPlan for SimpleVmPlan {
    fn devices(&self) -> &VmDevicePlan {
        &self.0
    }
}
