//! Device construction for VM preparation.

use alloc::vec::Vec;

use axdevice::{DeviceBuildContext, DeviceFactoryRegistry, DeviceRuntime};
use axvm_types::EmulatedDeviceConfig;

use super::super::AxVMResources;
use crate::{AxVmResult, irq::InterruptFabric};

pub(crate) struct PreparedDevices {
    pub(crate) devices: DeviceRuntime,
}

impl PreparedDevices {
    #[allow(dead_code)]
    pub(crate) fn build_common(
        resources: &AxVMResources,
        factories: &DeviceFactoryRegistry,
        interrupt_fabric: &InterruptFabric,
    ) -> AxVmResult<Self> {
        Self::build_common_with_extra(resources, factories, interrupt_fabric, &[])
    }

    pub(crate) fn build_common_with_extra(
        resources: &AxVMResources,
        factories: &DeviceFactoryRegistry,
        interrupt_fabric: &InterruptFabric,
        extra_configs: &[EmulatedDeviceConfig],
    ) -> AxVmResult<Self> {
        let build_context = DeviceBuildContext::new(interrupt_fabric);
        let mut configs: Vec<EmulatedDeviceConfig> = resources.config.emu_devices().to_vec();
        configs.extend_from_slice(extra_configs);
        let devices = DeviceRuntime::build_with_factories(&configs, factories, &build_context)?;

        Ok(Self { devices })
    }

    pub(crate) const fn devices(&self) -> &DeviceRuntime {
        &self.devices
    }

    pub(crate) fn into_inner(self) -> DeviceRuntime {
        self.devices
    }
}
