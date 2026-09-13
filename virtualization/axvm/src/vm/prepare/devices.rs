//! Device construction for VM preparation.

use axdevice::{DeviceRuntime, DeviceRuntimeBuilder, RuntimeAccessPorts};

use super::super::AxVMResources;
use crate::AxVmResult;

pub(crate) struct PreparedDevices {
    pub(crate) devices: DeviceRuntime,
}

impl PreparedDevices {
    pub(crate) fn build_planned(
        resources: &AxVMResources,
        access_ports: RuntimeAccessPorts,
    ) -> AxVmResult<Self> {
        let planned = resources.planned_devices();
        let mut builder = DeviceRuntimeBuilder::new(access_ports);
        for node in planned.graph().nodes() {
            builder.build_graph_node(node, planned.graph().resource_plan())?;
        }
        let devices = builder.finish(planned.graph().resource_plan())?;

        Ok(Self { devices })
    }

    pub(crate) const fn devices(&self) -> &DeviceRuntime {
        &self.devices
    }

    pub(crate) fn into_inner(self) -> DeviceRuntime {
        self.devices
    }
}
