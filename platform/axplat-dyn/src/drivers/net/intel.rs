use eth_intel::E1000;
use pcie::CommandRegister;
use rdrive::{
    PlatformDevice, module_driver,
    probe::{
        OnProbeError,
        pci::{EndpointRc, FnOnProbe},
    },
};

use super::PlatformDeviceNet;
use crate::{boot::Kernel, drivers::DmaImpl};

const DRIVER_NAME: &str = "eth-intel-e1000";

module_driver!(
    name: "Intel E1000 PCI Network",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe as FnOnProbe
    }],
);

fn probe(endpoint: &mut EndpointRc, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if !E1000::check_vid_did(endpoint.vendor_id(), endpoint.device_id()) {
        return Err(OnProbeError::NotMatch);
    }

    let address = endpoint.address();
    let irq = super::pci_legacy_irq_for_address(address);
    let Some(bar) = endpoint.bar_mmio(0) else {
        return Err(OnProbeError::other("E1000 BAR0 MMIO region missing"));
    };

    endpoint.update_command(|mut cmd| {
        cmd.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        cmd
    });

    let dev = E1000::new(bar.start as u64, bar.count(), u64::MAX, &DmaImpl, &Kernel)
        .map_err(|err| OnProbeError::other(alloc::format!("failed to create e1000: {err:?}")))?;

    plat_dev.register_net(DRIVER_NAME, dev, Some(irq));
    debug!(
        "intel e1000 PCI device registered successfully at {} with irq {:#x}",
        address, irq
    );
    Ok(())
}
