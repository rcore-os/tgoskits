//! Device construction for VM preparation.

use axdevice::{AxVmDeviceConfig, AxVmDevices, DeviceBuildContext, DeviceFactoryRegistry};

use super::super::{AxVM, AxVMResources};
use crate::{AxVmResult, irq::InterruptFabric};

pub(crate) struct PreparedDevices {
    pub(crate) devices: AxVmDevices,
}

impl PreparedDevices {
    pub(crate) fn build_common(
        resources: &AxVMResources,
        factories: &DeviceFactoryRegistry,
        interrupt_fabric: &InterruptFabric,
    ) -> AxVmResult<Self> {
        let build_context = DeviceBuildContext::new(interrupt_fabric);
        let devices = AxVmDevices::build_with_factories(
            AxVmDeviceConfig {
                emu_configs: resources.config.emu_devices().to_vec(),
            },
            factories,
            &build_context,
        )?;

        Ok(Self { devices })
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
