//! RISC-V machine-owned automatic device resource windows.

use axdevice::ResourcePools;
use axdevice_base::{ControllerInputId, HostIrqId, InterruptControllerId, InterruptSharing};

use crate::{AxVmResult, config::AxVMConfig};

const AUTO_MMIO: core::ops::Range<u64> = 0x1100_0000..0x2000_0000;
const AUTO_SOURCE: core::ops::Range<ControllerInputId> =
    ControllerInputId::new(1)..ControllerInputId::new(1024);

pub(super) fn create(config: &AxVMConfig) -> AxVmResult<ResourcePools> {
    let controller = InterruptControllerId::new(0);
    let mut pools = ResourcePools::new();
    pools.add_auto_mmio(AUTO_MMIO)?;
    pools.add_auto_controller_inputs(controller, AUTO_SOURCE)?;

    for route in config.pass_through_irqs() {
        let input = ControllerInputId::new(route.source as usize);
        let owner = alloc::format!("riscv-physical-irq-{}", route.source);
        pools.reserve_controller_input(
            owner.clone(),
            controller,
            input,
            route.trigger,
            InterruptSharing::Exclusive,
        )?;
        pools.reserve_host_irq(owner, HostIrqId::new(route.source as usize))?;
    }
    Ok(pools)
}
