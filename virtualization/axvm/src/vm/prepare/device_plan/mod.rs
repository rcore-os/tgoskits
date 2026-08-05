//! VM-local resource plans created by architecture-owned initialization.

mod model;
mod pools;

use alloc::vec::Vec;

use axdevice::{
    DeviceModelRegistry, ResourcePools, VmResourcePlan, VmResourcePlanner, register_builtin_models,
};
use axdevice_base::{InterruptControllerId, InterruptSharing, InterruptTrigger};
use axvm_types::EmulatedDeviceConfig;
pub(crate) use model::{FixedAddressKind, FixedDeviceModel};

use crate::{AxVmResult, config::AxVMConfig, machine::GuestSerialTransport};

pub(crate) fn machine_model_registry(config: &AxVMConfig) -> AxVmResult<DeviceModelRegistry> {
    let mut models = DeviceModelRegistry::new();
    register_builtin_models(&mut models)?;
    let address = match config.serial_profile().transport {
        GuestSerialTransport::Port { .. } => FixedAddressKind::Pio,
        GuestSerialTransport::Mmio { .. } => FixedAddressKind::Mmio,
    };
    models.register(alloc::sync::Arc::new(
        FixedDeviceModel::new(axvm_types::EmulatedDeviceType::Console, address).with_wired_irq(
            InterruptControllerId::new(0),
            InterruptTrigger::LevelTriggered,
            InterruptSharing::Exclusive,
        ),
    ))?;
    Ok(models)
}

/// Pure models and one-shot claims retained for a VM's whole lifetime.
pub(crate) struct VmDevicePlan {
    configs: Vec<EmulatedDeviceConfig>,
    resources: VmResourcePlan,
    models: DeviceModelRegistry,
}

impl VmDevicePlan {
    pub(crate) fn fixed(
        configs: &[EmulatedDeviceConfig],
        models: DeviceModelRegistry,
    ) -> AxVmResult<Self> {
        Self::with_pools(configs, models, ResourcePools::new())
    }

    pub(crate) fn with_pools(
        configs: &[EmulatedDeviceConfig],
        models: DeviceModelRegistry,
        mut pools: ResourcePools,
    ) -> AxVmResult<Self> {
        let requests = configs
            .iter()
            .map(|config| models.plan_request(&config.name, config))
            .collect::<Result<Vec<_>, _>>()?;
        pools::allow_fixed_requirements(&requests, &mut pools)?;
        let resources = VmResourcePlanner::new(pools).plan(requests)?;
        Ok(Self {
            configs: configs.to_vec(),
            resources,
            models,
        })
    }

    pub(crate) fn configs(&self) -> &[EmulatedDeviceConfig] {
        &self.configs
    }

    pub(crate) const fn resources(&self) -> &VmResourcePlan {
        &self.resources
    }

    pub(crate) const fn models(&self) -> &DeviceModelRegistry {
        &self.models
    }
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
