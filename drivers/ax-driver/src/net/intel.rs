use eth_intel::E1000;
use log::debug;
use pcie::CommandRegister;
use rdrive::probe::{
    OnProbeError,
    pci::{FnOnProbe, ProbePci},
};

use crate::{PciIrqRequirement, net::ProbePciNet};

const DRIVER_NAME: &str = "eth-intel-e1000";

crate::model_register!(
    name: "Intel E1000 PCI Network",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe as FnOnProbe
    }],
);

fn probe(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    let endpoint = probe.endpoint_mut();
    if !E1000::check_vid_did(endpoint.vendor_id(), endpoint.device_id()) {
        return Err(OnProbeError::NotMatch);
    }

    let address = endpoint.address();
    let Some(bar) = endpoint.bar_mmio(0) else {
        return Err(OnProbeError::other("E1000 BAR0 MMIO region missing"));
    };

    endpoint.update_command(|mut cmd| {
        cmd.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        cmd
    });

    let dev = E1000::new(
        bar.start as u64,
        bar.count(),
        u64::MAX,
        axklib::dma::op(),
        axklib::mmio::op(),
    )
    .map_err(|err| OnProbeError::other(alloc::format!("failed to create e1000: {err:?}")))?;

    let irq = probe.register_net(DRIVER_NAME, dev, PciIrqRequirement::Required)?;
    debug!(
        "intel e1000 PCI device registered successfully at {} with irq {:?}",
        address, irq
    );
    Ok(())
}
