use core::sync::atomic::{AtomicBool, Ordering};

use pcie::CommandRegister;
use rdrive::{
    PlatformDevice, module_driver,
    probe::{
        OnProbeError,
        pci::{EndpointRc, FnOnProbe},
    },
};
use realtek_rtl8125::Rtl8125;

use super::PlatformDeviceNet;
use crate::{boot::Kernel, drivers::DmaImpl};

const DRIVER_NAME: &str = "realtek-rtl8125";
const RTL8125_DMA_MASK: u64 = u32::MAX as u64;
static REGISTERED_RTL8125: AtomicBool = AtomicBool::new(false);

module_driver!(
    name: "Realtek RTL8125 PCI Network",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe as FnOnProbe
    }],
);

fn probe(endpoint: &mut EndpointRc, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if !Rtl8125::check_vid_did(endpoint.vendor_id(), endpoint.device_id()) {
        return Err(OnProbeError::NotMatch);
    }

    let address = endpoint.address();
    if REGISTERED_RTL8125.load(Ordering::Acquire) {
        info!("RTL8125 at {address} left unused: first port already registered");
        return Err(OnProbeError::NotMatch);
    }

    let irq = super::pci_legacy_irq_for_address(address);
    let Some((bar_index, bar)) = first_mmio_bar(endpoint) else {
        warn!("RTL8125 at {address} left unused: no PCI MMIO BAR found");
        return Err(OnProbeError::NotMatch);
    };
    info!(
        "RTL8125 PCI endpoint {address}: BAR{bar_index}={:#x}..{:#x}, irq={irq}, int_pin={}, \
         int_line={}, command={:?}, status={:?}",
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
        &DmaImpl,
        &Kernel,
    )
    .map_err(|err| OnProbeError::other(alloc::format!("failed to create RTL8125: {err:?}")))?;

    let status = dev.status();
    if status.link_up() {
        info!("RTL8125 at {address}: link is up after init, status={status:?}");
    } else {
        warn!(
            "RTL8125 at {address}: link is down after init; registering and checking link again \
             on tx, status={status:?}"
        );
    }
    if REGISTERED_RTL8125
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        info!("RTL8125 at {address} left unused: first port already registered");
        return Err(OnProbeError::NotMatch);
    }

    plat_dev.register_net(DRIVER_NAME, dev, Some(irq));
    debug!("RTL8125 PCI network device registered at {address} with irq {irq:#x}");
    Ok(())
}

fn first_mmio_bar(endpoint: &EndpointRc) -> Option<(u8, core::ops::Range<usize>)> {
    (0..6).find_map(|bar| endpoint.bar_mmio(bar).map(|range| (bar, range)))
}
