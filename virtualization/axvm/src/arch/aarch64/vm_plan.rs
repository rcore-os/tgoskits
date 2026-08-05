//! Immutable AArch64 device, VGIC, and firmware construction plan.

use alloc::sync::Arc;

use axdevice::DeviceModelRegistry;
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use super::{
    firmware_plan::Aarch64FirmwarePlan, shared_provider::SharedProviderBootstrap,
    vgic::VgicConstructionPlan,
};
use crate::{
    AxVmResult,
    config::AxVMConfig,
    machine::GuestGicProfile,
    vm::prepare::device_plan::{
        ArchitectureVmPlan, FixedAddressKind, FixedDeviceModel, VmDevicePlan,
        machine_model_registry,
    },
};

/// Complete AArch64 plan created once before firmware and devices are finalized.
pub(crate) struct Aarch64VmPlan {
    devices: VmDevicePlan,
    vgic: Arc<VgicConstructionPlan>,
    firmware: Aarch64FirmwarePlan,
    shared_providers: SharedProviderBootstrap,
}

impl Aarch64VmPlan {
    pub(crate) fn new(config: &AxVMConfig) -> AxVmResult<Self> {
        let vgic = VgicConstructionPlan::new(config)?;
        let shared_providers = SharedProviderBootstrap::from_config(config)?;
        let configs = planned_device_configs(config, &shared_providers)?;
        let models = device_models(config, &vgic)?;
        let devices = VmDevicePlan::fixed(&configs, models)?;
        let firmware = Aarch64FirmwarePlan::new(config, vgic.config())?;
        Ok(Self {
            devices,
            vgic,
            firmware,
            shared_providers,
        })
    }

    pub(super) const fn vgic(&self) -> &Arc<VgicConstructionPlan> {
        &self.vgic
    }

    pub(super) const fn shared_providers(&self) -> &SharedProviderBootstrap {
        &self.shared_providers
    }

    pub(crate) const fn gic_profile(&self) -> &GuestGicProfile {
        self.firmware.gic()
    }
}

impl ArchitectureVmPlan for Aarch64VmPlan {
    fn devices(&self) -> &VmDevicePlan {
        &self.devices
    }
}

fn planned_device_configs(
    config: &AxVMConfig,
    shared_providers: &SharedProviderBootstrap,
) -> AxVmResult<alloc::vec::Vec<EmulatedDeviceConfig>> {
    let controller_type = EmulatedDeviceType::InterruptController;
    let mut controllers = config
        .emu_devices()
        .iter()
        .filter(|device| device.emu_type == controller_type);
    let controller = controllers
        .next()
        .cloned()
        .ok_or_else(|| crate::AxVmError::invalid_config("AArch64 machine profile has no VGIC"))?;
    if controllers.next().is_some() {
        return Err(crate::AxVmError::invalid_config(
            "AArch64 machine profile has more than one VGIC",
        ));
    }

    let mut configs = alloc::vec![controller];
    configs.extend(
        config
            .emu_devices()
            .iter()
            .filter(|device| device.emu_type != controller_type)
            .cloned(),
    );
    configs.extend_from_slice(shared_providers.configs());
    Ok(configs)
}

fn device_models(
    config: &AxVMConfig,
    vgic: &Arc<VgicConstructionPlan>,
) -> AxVmResult<DeviceModelRegistry> {
    let mut models = machine_model_registry(config)?;
    vgic.register_model(&mut models)?;
    for device_type in [
        EmulatedDeviceType::GicCpuRegion,
        EmulatedDeviceType::SharedMmio,
    ] {
        models.register(Arc::new(FixedDeviceModel::new(
            device_type,
            FixedAddressKind::Mmio,
        )))?;
    }
    Ok(models)
}
