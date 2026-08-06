//! Device construction for VM preparation.

use std::vec::Vec;

use axdevice::{DeviceBuildContext, DeviceFactoryRegistry, DeviceRuntime, RuntimeAccessPorts};
use axdevice_base::VirtualInterruptController;
use axvm_types::EmulatedDeviceConfig;

use super::super::AxVMResources;
use crate::AxVmResult;

pub(crate) struct PreparedDevices {
    pub(crate) devices: DeviceRuntime,
}

impl PreparedDevices {
    #[allow(dead_code)]
    pub(crate) fn build_common(
        resources: &AxVMResources,
        factories: &DeviceFactoryRegistry,
        interrupt_controller: &dyn VirtualInterruptController,
        access_ports: RuntimeAccessPorts,
    ) -> AxVmResult<Self> {
        Self::build_common_with_extra(
            resources,
            factories,
            interrupt_controller,
            &[],
            access_ports,
        )
    }

    pub(crate) fn build_common_with_extra(
        resources: &AxVMResources,
        factories: &DeviceFactoryRegistry,
        interrupt_controller: &dyn VirtualInterruptController,
        extra_configs: &[EmulatedDeviceConfig],
        access_ports: RuntimeAccessPorts,
    ) -> AxVmResult<Self> {
        let build_context = DeviceBuildContext::new(interrupt_controller);
        let mut configs: Vec<EmulatedDeviceConfig> = resources.config.emu_devices().to_vec();
        configs.extend_from_slice(extra_configs);
        let devices = DeviceRuntime::build_with_factories_and_ports(
            &configs,
            factories,
            &build_context,
            access_ports,
        )?;

        Ok(Self { devices })
    }

    pub(crate) const fn devices(&self) -> &DeviceRuntime {
        &self.devices
    }

    pub(crate) fn into_inner(self) -> DeviceRuntime {
        self.devices
    }
}
