//! Device construction for VM preparation.

use axdevice::{
    AxVmDeviceConfig, AxVmDevices, DeviceBuildContext, DeviceFactoryRegistry, InterruptTopology,
};
use axvm_types::EmulatedDeviceConfig;

use super::super::AxVM;
use crate::AxVmResult;

pub(crate) struct PreparedDevices {
    pub(crate) devices: AxVmDevices,
}

impl PreparedDevices {
    pub(crate) fn empty() -> Self {
        Self {
            devices: AxVmDevices::empty(),
        }
    }

    pub(crate) fn register_configured(
        &mut self,
        configs: &[EmulatedDeviceConfig],
        factories: &DeviceFactoryRegistry,
        interrupt_topology: &InterruptTopology,
    ) -> AxVmResult {
        self.devices
            .register_configured_devices(
                &AxVmDeviceConfig {
                    emu_configs: configs.to_vec(),
                },
                factories,
                &DeviceBuildContext::new(interrupt_topology),
            )
            .map_err(Into::into)
    }

    pub(crate) fn register_special_devices(&mut self, vm: &AxVM) -> AxVmResult {
        vm.add_special_emulated_devices(&mut self.devices)
    }

    pub(crate) const fn devices(&self) -> &AxVmDevices {
        &self.devices
    }

    pub(crate) fn into_inner(self) -> AxVmDevices {
        self.devices
    }
}
