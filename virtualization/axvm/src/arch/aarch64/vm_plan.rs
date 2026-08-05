//! Immutable AArch64 device, VGIC, and firmware construction plan.

use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use super::{
    firmware_plan::Aarch64FirmwarePlan, shared_provider::SharedProviderBootstrap,
    vgic::VgicConstructionPlan,
};
use crate::{
    AxVmResult,
    config::AxVMConfig,
    machine::{GuestGicProfile, GuestSerialFdtIdentity, GuestSerialProfile, GuestTimerProfile},
    vm::prepare::device_plan::{ArchitectureVmPlan, VmDevicePlan, machine_factory_registry},
};

/// Complete AArch64 plan created once before firmware and devices are finalized.
pub(crate) struct Aarch64VmPlan {
    devices: VmDevicePlan,
    firmware: Aarch64FirmwarePlan,
}

impl Aarch64VmPlan {
    pub(crate) fn new(config: &AxVMConfig) -> AxVmResult<Self> {
        let vgic = VgicConstructionPlan::new(config)?;
        let shared_providers = SharedProviderBootstrap::from_config(config)?;
        let configs = planned_device_configs(config, &shared_providers)?;
        let mut factories = machine_factory_registry(config)?;
        shared_providers.register_factory(&mut factories)?;
        super::vgic::register_device_factories(config.id(), &vgic, &mut factories)?;
        let mut host_replacements = alloc::vec![
            EmulatedDeviceType::InterruptController,
            EmulatedDeviceType::GicCpuRegion,
            EmulatedDeviceType::SharedMmio,
        ];
        if config.serial_fdt_identity().is_some() {
            host_replacements.push(EmulatedDeviceType::Console);
        }
        let devices = VmDevicePlan::with_pools_for_vm(
            config,
            &configs,
            factories,
            Some(EmulatedDeviceType::InterruptController),
            &host_replacements,
            super::resource_pools::create(vgic.config())?,
        )?;
        let firmware = Aarch64FirmwarePlan::new(config, vgic.config())?;
        Ok(Self { devices, firmware })
    }

    pub(crate) const fn gic_profile(&self) -> &GuestGicProfile {
        self.firmware.gic()
    }

    pub(crate) const fn serial_profile(&self) -> GuestSerialProfile {
        self.firmware.serial()
    }

    pub(crate) const fn serial_fdt_identity(&self) -> Option<&GuestSerialFdtIdentity> {
        self.firmware.serial_identity()
    }

    pub(crate) const fn timer_profile(&self) -> &GuestTimerProfile {
        self.firmware.timer()
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
