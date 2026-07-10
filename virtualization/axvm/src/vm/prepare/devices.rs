//! Device construction for VM preparation.

use ax_errno::AxResult;
use axdevice::{AxVmDeviceConfig, AxVmDevices, DeviceBuildContext, DeviceFactoryRegistry};

use super::super::{AxVM, AxVMResources};
use crate::irq::InterruptFabric;

pub(super) struct PreparedDevices {
    devices: AxVmDevices,
}

impl PreparedDevices {
    pub(super) fn build(
        vm: &AxVM,
        resources: &AxVMResources,
        factories: &DeviceFactoryRegistry,
        interrupt_fabric: &InterruptFabric,
    ) -> AxResult<Self> {
        let build_context = DeviceBuildContext::new(interrupt_fabric);
        let mut devices = AxVmDevices::build_with_factories(
            AxVmDeviceConfig {
                emu_configs: resources.config.emu_devices().to_vec(),
            },
            factories,
            &build_context,
        )?;

        crate::arch::register_arch_devices(vm, &resources.config, &mut devices)?;
        vm.add_special_emulated_devices(&mut devices)?;
        Ok(Self { devices })
    }

    pub(super) const fn devices(&self) -> &AxVmDevices {
        &self.devices
    }

    pub(super) fn into_inner(self) -> AxVmDevices {
        self.devices
    }
}
