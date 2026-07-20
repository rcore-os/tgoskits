use log::{debug, info, warn};
use pcie::CommandRegister;
use rdrive::probe::{
    OnProbeError,
    pci::{EndpointRc, FnOnProbe, ProbePci},
};
use realtek_rtl8125::Rtl8125;

use crate::{PciIrqRequirement, net::ProbePciNet};

const DRIVER_NAME: &str = "realtek-rtl8125";
const RTL8125_DMA_MASK: u64 = u32::MAX as u64;

crate::model_register!(
    name: "Realtek RTL8125 PCI Network",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe as FnOnProbe
    }],
);

fn probe(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    let endpoint = probe.endpoint_mut();
    if !Rtl8125::check_vid_did(endpoint.vendor_id(), endpoint.device_id()) {
        return Err(OnProbeError::NotMatch);
    }

    let address = endpoint.address();
    let Some((bar_index, bar)) = first_mmio_bar(endpoint) else {
        warn!("RTL8125 at {address} left unused: no PCI MMIO BAR found");
        return Err(OnProbeError::NotMatch);
    };
    info!(
        "RTL8125 PCI endpoint {address}: BAR{bar_index}={:#x}..{:#x}, int_pin={}, int_line={}, \
         command={:?}, status={:?}",
        bar.start,
        bar.end,
        endpoint.interrupt_pin(),
        endpoint.interrupt_line(),
        endpoint.command(),
        endpoint.status()
    );

    endpoint.update_command(|mut cmd| {
        cmd.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        cmd.remove(CommandRegister::INTERRUPT_DISABLE);
        cmd
    });

    let dev = Rtl8125::new(
        bar.start as u64,
        bar.count(),
        RTL8125_DMA_MASK,
        axklib::dma::op(),
        axklib::mmio::op(),
    )
    .map_err(|err| OnProbeError::other(alloc::format!("failed to create RTL8125: {err:?}")))?;

    let irq = probe.register_net(DRIVER_NAME, dev, PciIrqRequirement::Required)?;
    debug!(
        "RTL8125 PCI network device registered pending owner initialization at {address} with irq \
         {irq:?}"
    );
    Ok(())
}

fn first_mmio_bar(endpoint: &EndpointRc) -> Option<(u8, core::ops::Range<usize>)> {
    (0..6).find_map(|bar| endpoint.bar_mmio(bar).map(|range| (bar, range)))
}
