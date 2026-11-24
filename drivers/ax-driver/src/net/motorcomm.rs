use alloc::format;

use motorcomm_yt6801::Yt6801;
use pcie::CommandRegister;
use rdrive::probe::{OnProbeError, pci::ProbePci};

use crate::{PciIrqRequirement, binding_info_from_pci, net::PlatformDeviceNet};

crate::model_register!(
    name: "Motorcomm YT6801 PCI Network",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci { on_probe: probe }],
);

fn probe(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    let endpoint = probe.endpoint_mut();
    if endpoint.vendor_id() != 0x1f0a || endpoint.device_id() != 0x6801 {
        return Err(OnProbeError::NotMatch);
    }
    let address = endpoint.address();
    let had_msi = endpoint.disable_message_signaled_interrupts();
    let bar = endpoint
        .bar_mmio(0)
        .ok_or_else(|| OnProbeError::other("YT6801 BAR0 is absent"))?;
    log::info!(
        "YT6801 PCI endpoint {address}: BAR0={bar:#x?}, pin={}, line={}, command={:?}, previous \
         MSI={had_msi}",
        endpoint.interrupt_pin(),
        endpoint.interrupt_line(),
        endpoint.command(),
    );
    let dma = crate::pci::device_dma(probe.info(), u64::MAX)?;
    let binding = binding_info_from_pci(probe.info(), PciIrqRequirement::Required)?;
    probe.endpoint_mut().update_command(|mut command| {
        command.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        command.remove(CommandRegister::INTERRUPT_DISABLE);
        command
    });
    let device = Yt6801::new(
        bar.start,
        bar.len(),
        dma,
        axklib::mmio::op(),
        axklib::time::busy_wait,
    )
    .map_err(|error| OnProbeError::other(format!("YT6801 initialization failed: {error}")))?;
    let dma = device.dma();
    probe
        .into_platform_device()
        .register_net_with_info("motorcomm-yt6801", device, dma, binding)
}
