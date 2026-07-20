//! Per-vCPU emulated EL1 physical timers connected to a Redistributor PPI.

use alloc::sync::Arc;

use arm_vgic::PpiId;
use axdevice::{
    DeviceBundle, DeviceRegistration, GicV3DeviceSet, InterruptTopology, InterruptTriggerMode,
    VcpuInterruptId,
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
    for placement in placements {
        lines.push(gic.connect_ppi(
            VcpuInterruptId::new(placement.id),
            ppi,
            InterruptTriggerMode::LevelTriggered,
        )?);
    }
    let timer_bank = Arc::new(VirtualTimerBank::new(lines, frequency));
    devices
        .devices
        .register_bundle_with_topology(
            DeviceBundle::from_registration(DeviceRegistration::Device(timer_bank)),
            topology,
        )
        .map_err(Into::into)
}
