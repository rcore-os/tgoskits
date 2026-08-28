//! x86 machine-owned automatic device resource windows.

use axdevice::ResourcePools;
use axdevice_base::*;

use crate::{AxVmResult, config::AxVMConfig};

// Keep synthetic MMIO below the PCI root bridge window described by ACPI.
const AUTO_MMIO: core::ops::Range<u64> = 0x8000_0000..0xc000_0000;
const AUTO_PIO: core::ops::Range<u16> = 0x1000..0x5000;
const AUTO_GSI: core::ops::Range<ControllerInputId> =
    ControllerInputId::new(5)..ControllerInputId::new(16);

pub(super) fn create(config: &AxVMConfig) -> AxVmResult<ResourcePools> {
    let controller = InterruptControllerId::new(0);
    let mut pools = ResourcePools::new();
    pools.add_auto_mmio(AUTO_MMIO)?;
    pools.allow_fixed_mmio(
        super::pci_config::PCI_MEMORY_BASE
            ..super::pci_config::PCI_MEMORY_BASE + super::pci_config::PCI_MEMORY_SIZE,
    )?;
    pools.add_auto_pio(AUTO_PIO)?;
    pools.add_auto_controller_inputs(controller, AUTO_GSI)?;

    for route in config.pass_through_irqs() {
        let input = ControllerInputId::new(route.source as usize);
        let owner = std::format!("x86-physical-irq-{}", route.source);
        pools.reserve_wired_host_irq(
            owner,
            controller,
            input,
            HostIrqId::new(route.source as usize),
            route.trigger,
        )?;
    }
    Ok(pools)
}
