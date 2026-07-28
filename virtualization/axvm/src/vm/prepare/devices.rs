//! Device construction for VM preparation.

use axdevice::{DeviceBuildContext, DeviceFactoryRegistry, DeviceRuntime};

use super::super::AxVMResources;
use crate::{AxVmResult, irq::InterruptFabric};

pub(crate) struct PreparedDevices {
    pub(crate) devices: DeviceRuntime,
}

impl PreparedDevices {
    pub(crate) fn build_common(
        resources: &AxVMResources,
        factories: &DeviceFactoryRegistry,
        interrupt_fabric: &InterruptFabric,
    ) -> AxVmResult<Self> {
        let build_context = DeviceBuildContext::new(interrupt_fabric);
        let devices = DeviceRuntime::build_with_factories(
            resources.config.emu_devices(),
            factories,
            &build_context,
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
