//! Device construction for VM preparation.

use axdevice::{
    AxVmDevices, DeviceBuildContext, InterruptPlanAuthority, InterruptTopology,
    VirtualDeviceModelRegistry,
};

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

    pub(crate) fn register_planned(
        &mut self,
        plan: &crate::machine::VmMachinePlan,
        models: &VirtualDeviceModelRegistry,
        interrupt_topology: &InterruptTopology,
        interrupt_authority: &InterruptPlanAuthority,
    ) -> AxVmResult {
        for device in plan.virtual_devices() {
            let context = DeviceBuildContext::with_backend(
                interrupt_topology,
                interrupt_authority,
                device.resources(),
                device.backend(),
            );
            let bundle = models.build(device.model_id(), device.resources(), context)?;
            self.devices
                .register_bundle_with_topology(bundle, interrupt_topology)?;
        }
        Ok(())
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
