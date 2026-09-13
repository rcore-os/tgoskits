//! LoongArch machine-owned automatic device resource windows.

use axdevice::ResourcePools;
use axdevice_base::{ControllerInputId, InterruptControllerId};

use crate::AxVmResult;

// QEMU virt reserves 0x2000_0000..0x3000_0000 for ECAM and PCH MSI.
const AUTO_MMIO: core::ops::Range<u64> = 0x3000_0000..0x4000_0000;
const AUTO_PCH_INPUT: core::ops::Range<ControllerInputId> =
    ControllerInputId::new(20)..ControllerInputId::new(32);

pub(super) fn create() -> AxVmResult<ResourcePools> {
    let mut pools = ResourcePools::new();
    pools.add_auto_mmio(AUTO_MMIO)?;
    pools.add_auto_controller_inputs(InterruptControllerId::new(0), AUTO_PCH_INPUT)?;
    Ok(pools)
}
