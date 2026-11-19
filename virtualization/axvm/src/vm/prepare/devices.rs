//! Device construction for VM preparation.

use std::sync::Arc;

use axdevice::{DeviceRuntime, DeviceRuntimeBuilder, RuntimeAccessPorts, Stage2Remap};

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
        let stage2_remap: Arc<dyn Stage2Remap> = resources.stage2_remap.clone();
        let mut builder = DeviceRuntimeBuilder::new(access_ports).with_stage2_remap(stage2_remap);
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
