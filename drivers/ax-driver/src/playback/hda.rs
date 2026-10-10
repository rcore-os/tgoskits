//! PCI HDA registration using the platform's existing MMIO and DMA capabilities.
use alloc::format;
use core::time::Duration;

use intel_hda::{Clock, Controller, MappedHdaIo, identify};
use pcie::CommandRegister;
use rdrive::probe::{OnProbeError, pci::ProbePci};

use super::PlatformDevicePlayback;

struct PlatformClock;
impl Clock for PlatformClock {
    fn delay_us(&mut self, micros: u32) {
        axklib::time::busy_wait(Duration::from_micros(u64::from(micros)));
    }
    fn now_ns(&self) -> u64 {
        axklib::time::monotonic_nanos()
    }
}

crate::model_register!(
    name: "Intel HDA analog playback",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci { on_probe: probe_pci }],
);

fn probe_pci(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    let endpoint = probe.endpoint();
    let class = endpoint.revision_and_class();
    let Some(device) = identify(
        endpoint.vendor_id(),
        endpoint.device_id(),
        class.base_class,
        class.sub_class,
        class.interface,
    ) else {
        return Err(OnProbeError::NotMatch);
    };
    let bar = endpoint
        .bar_mmio(0)
        .ok_or_else(|| OnProbeError::other("HDA BAR0 missing"))?;
    let size = bar
        .end
        .checked_sub(bar.start)
        .filter(|&size| size != 0)
        .ok_or_else(|| OnProbeError::other("HDA BAR0 is empty"))?;
    let dma = crate::pci::device_dma(probe.info(), u64::MAX)?;
    let mmio = axklib::mmio::ioremap(bar.start.into(), size)
        .map_err(|error| OnProbeError::other(format!("HDA mapping failed: {error}")))?;
    // The controller core polls completions. Both PCI INTx and its INTCTL remain
    // masked; no interrupt is enabled without an owned registration.
    probe.endpoint_mut().update_command(|mut command| {
        command.insert(
            CommandRegister::MEMORY_ENABLE
                | CommandRegister::BUS_MASTER_ENABLE
                | CommandRegister::INTERRUPT_DISABLE,
        );
        command
    });
    let controller = Controller::new(MappedHdaIo::new(mmio, PlatformClock), device, dma)
        .map_err(|error| OnProbeError::other(format!("HDA initialization failed: {error}")))?;
    log::info!("registered HDA PCM playback at {}", probe.info().address);
    probe.into_platform_device().register_playback(controller);
    Ok(())
}
