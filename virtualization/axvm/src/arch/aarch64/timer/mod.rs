//! Per-vCPU emulated EL1 physical timers connected to a Redistributor PPI.

use alloc::sync::Arc;

use arm_vgic::PpiId;
use axdevice::{
    DeviceBundle, DeviceRegistration, GicV3DeviceSet, InterruptPlanAuthority, InterruptTopology,
    InterruptTriggerMode, VcpuInterruptId,
};

use self::{device::VirtualTimerBank, state::counter_frequency};
use crate::{
    AxVmError, AxVmResult,
    vm::prepare::{devices::PreparedDevices, vcpus::VcpuPlacement},
};

mod device;
mod state;

const PHYSICAL_TIMER_PPI: u8 = 30;

pub(crate) fn register_emulated_timers(
    devices: &mut PreparedDevices,
    gic: &GicV3DeviceSet,
    placements: &[VcpuPlacement],
    topology: &InterruptTopology,
    authority: &InterruptPlanAuthority,
    discovered_ppi: Option<PpiId>,
) -> AxVmResult {
    let ppi = discovered_ppi
        .map(Ok)
        .unwrap_or_else(|| PpiId::new(PHYSICAL_TIMER_PPI))
        .map_err(|error| AxVmError::interrupt("validate physical timer PPI", error))?;
    let frequency = counter_frequency();
    if frequency == 0 {
        return Err(AxVmError::unsupported(
            "create AArch64 physical timers",
            "CNTFRQ_EL0 reports zero",
        ));
    }
    let mut lines = alloc::vec::Vec::with_capacity(placements.len());
    let mut endpoint_registrations = alloc::vec::Vec::with_capacity(placements.len());
    for placement in placements {
        let request = gic.ppi_request(
            VcpuInterruptId::new(placement.id),
            ppi,
            InterruptTriggerMode::LevelTriggered,
        )?;
        let claim = authority.claim_wired(topology, request)?;
        let (line, registration) = topology.connect_irq(claim)?.into_parts();
        lines.push(line);
        endpoint_registrations.push(registration);
    }
    let timer_bank = Arc::new(VirtualTimerBank::new(lines, frequency));
    let mut bundle = DeviceBundle::from_registration(DeviceRegistration::Device(timer_bank));
    for registration in endpoint_registrations {
        bundle.push(DeviceRegistration::InterruptEndpoint(registration));
    }
    devices
        .devices
        .register_bundle_with_topology(bundle, topology)
        .map_err(Into::into)
}
