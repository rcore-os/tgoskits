//! Device construction for VM preparation.

use axdevice::{DeviceFactoryRegistry, DeviceRuntime, DeviceRuntimeBuilder, RuntimeAccessPorts};

use super::super::AxVMResources;
use crate::AxVmResult;

pub(crate) struct PreparedDevices {
    pub(crate) devices: DeviceRuntime,
}

impl PreparedDevices {
    pub(crate) fn build_planned(
        resources: &AxVMResources,
        factories: &DeviceFactoryRegistry,
        access_ports: RuntimeAccessPorts,
    ) -> AxVmResult<Self> {
        let planned = resources.planned_devices();
        let mut builder = DeviceRuntimeBuilder::new(access_ports);
        for config in planned.configs() {
            builder.build_planned_device(
                &config.name,
                config,
                planned.models(),
                factories,
                planned.resources(),
            )?;
        }
        let devices = builder.finish(planned.resources())?;

        Ok(Self { devices })
    }

    pub(crate) const fn devices(&self) -> &DeviceRuntime {
        &self.devices
    }

    pub(crate) fn into_inner(self) -> DeviceRuntime {
        self.devices
    }
}
