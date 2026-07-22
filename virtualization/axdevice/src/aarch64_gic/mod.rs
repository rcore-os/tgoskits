//! AxDevice boundary for one VM-local GICv3 controller.

use alloc::sync::Arc;

use arm_vgic::{GicV3Controller, GicVcpuId, PpiId};
use axdevice_base::{InterruptControllerId, InterruptSharing, InterruptTriggerMode};

use crate::{
    ControllerRegistration, ControllerRole, DeviceBundle, DeviceManagerResult, DeviceRegistration,
    VcpuInterruptId, WiredIrqRequest,
};

mod error;
mod mmio;
mod topology;

use mmio::configured_mmio_devices;
use topology::GicV3TopologyAdapter;

/// Builds the MMIO and interrupt-topology capabilities for one GICv3 controller.
pub struct GicV3DeviceSet {
    controller: Arc<GicV3Controller>,
    topology: Arc<GicV3TopologyAdapter>,
}

impl GicV3DeviceSet {
    /// Creates AxDevice adapters around an architecture-domain controller.
    pub fn new(controller: Arc<GicV3Controller>, id: InterruptControllerId) -> Self {
        Self {
            topology: Arc::new(GicV3TopologyAdapter::new(id, controller.clone())),
            controller,
        }
    }

    /// Returns one atomic bundle containing the controller and all configured MMIO frames.
    pub fn bundle(&self, role: ControllerRole) -> DeviceBundle {
        let mut bundle = DeviceBundle::new();
        let registration = ControllerRegistration::new(self.topology.id(), role)
            .with_wired_inputs(self.topology.clone())
            .with_message_inputs(self.topology.clone())
            .with_vcpu_controller(self.topology.clone())
            .with_vcpu_deactivation(self.topology.clone());
        bundle.push(DeviceRegistration::InterruptController(registration));
        for device in configured_mmio_devices(self.controller.clone()) {
            bundle.push(DeviceRegistration::Device(device));
        }
        bundle
    }

    /// Describes one exclusive planner-owned vCPU Redistributor PPI input.
    pub fn ppi_request(
        &self,
        vcpu: VcpuInterruptId,
        ppi: PpiId,
        trigger: InterruptTriggerMode,
    ) -> DeviceManagerResult<WiredIrqRequest> {
        let input =
            topology::private_input_id(self.topology.id(), GicVcpuId::new(vcpu.value()), ppi)?;
        Ok(WiredIrqRequest::for_controller(
            self.topology.id(),
            input,
            trigger,
            InterruptSharing::Exclusive,
        ))
    }
}
