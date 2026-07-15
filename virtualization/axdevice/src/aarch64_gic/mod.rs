//! AxDevice boundary for one VM-local GICv3 controller.

use alloc::sync::Arc;

use arm_vgic::{GicV3Controller, PpiId};
use axdevice_base::{InterruptControllerId, InterruptTriggerMode, IrqLine};

use crate::{
    ControllerRegistration, ControllerRole, DeviceBundle, DeviceManagerResult, DeviceRegistration,
    VcpuInterruptId,
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
            .with_vcpu_controller(self.topology.clone());
        bundle.push(DeviceRegistration::InterruptController(registration));
        for device in configured_mmio_devices(self.controller.clone()) {
            bundle.push(DeviceRegistration::Device(device));
        }
        bundle
    }

    /// Creates a device-owned line connected to one vCPU's Redistributor PPI input.
    ///
    /// The topology must have attached the target vCPU before this method is called.
    pub fn connect_ppi(
        &self,
        vcpu: VcpuInterruptId,
        ppi: PpiId,
        trigger: InterruptTriggerMode,
    ) -> DeviceManagerResult<IrqLine> {
        self.topology.connect_ppi(vcpu, ppi, trigger)
    }
}
