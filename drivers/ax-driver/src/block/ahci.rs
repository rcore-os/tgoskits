//! AHCI discovery and rdif-block v0.13 activation registration.
//!
//! One move-only activator retains HBA initialization, shared IRQ, DMA, and all
//! sparse physical ports until the runtime selects one immutable ownership
//! plan. The PCI path transfers its INTx lease with that same owner.

extern crate alloc;

use alloc::format;

use ahci_host::{AhciConfig, AhciControllerActivator, AhciHost};
#[cfg(feature = "ahci")]
use pcie::CommandRegister;
use rdrive::probe::OnProbeError;
#[cfg(feature = "ahci")]
use rdrive::probe::pci::{FnOnProbe, ProbePci};
#[cfg(feature = "ls2k1000-ahci")]
use rdrive::register::ProbeFdt;

#[cfg(any(feature = "ahci", feature = "ls2k1000-ahci"))]
use super::PlatformDeviceBlockActivation;
#[cfg(feature = "ls2k1000-ahci")]
use crate::binding_info_from_fdt;
#[cfg(feature = "ahci")]
use crate::{PciIrqRequirement, binding_info_from_pci_endpoint, pci::PciIntxIrqLease};

pub const DEVICE_NAME: &str = "ahci";
#[cfg(feature = "ls2k1000-ahci")]
const LS2K1000_DEVICE_NAME: &str = "ls2k1000-ahci";
const LOGICAL_IRQ_SOURCE: usize = 0;

#[cfg(feature = "ahci")]
crate::model_register!(
    name: "AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci as FnOnProbe,
    }],
);

#[cfg(feature = "ls2k1000-ahci")]
crate::model_register!(
    name: "LS2K1000 AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &[
            "loongson,ls-ahci",
            "loongson,ls2k1000-ahci",
            "loongson,2k1000-ahci",
            "generic-ahci",
            "snps,dwc-ahci",
        ],
        on_probe: probe_fdt,
    }],
);

#[cfg(feature = "ahci")]
fn probe_pci(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    let class = probe.endpoint().revision_and_class();
    if (class.base_class, class.sub_class) != (0x01, 0x06) {
        return Err(OnProbeError::NotMatch);
    }
    let bar = probe
        .endpoint()
        .bar_mmio(5)
        .or_else(|| probe.endpoint().bar_mmio(0))
        .ok_or_else(|| OnProbeError::other("AHCI MMIO BAR missing"))?;
    let binding = binding_info_from_pci_endpoint(
        probe.info(),
        probe.endpoint(),
        PciIrqRequirement::Required,
    )?;
    PciIntxIrqLease::mask_for_discovery(probe.endpoint_mut());
    probe.endpoint_mut().update_command(|mut command| {
        command.insert(
            CommandRegister::MEMORY_ENABLE
                | CommandRegister::BUS_MASTER_ENABLE
                | CommandRegister::INTERRUPT_DISABLE,
        );
        command
    });

    let activator = discover_activator(DEVICE_NAME, bar.start, bar.count().max(1))?;
    let endpoint = probe.take_endpoint();
    let irq_lease = PciIntxIrqLease::new(endpoint, binding);
    let registered = probe
        .into_platform_device()
        .register_irq_bound_block_activator(activator, irq_lease)
        .ok_or_else(|| OnProbeError::other("failed to register AHCI activation owner"))?;
    log::info!("registered AHCI activation owner: slot={registered}");
    Ok(())
}

#[cfg(feature = "ls2k1000-ahci")]
fn probe_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, platform) = probe.into_parts();
    let register = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.path())))?;
    let size = register
        .size
        .and_then(|size| usize::try_from(size).ok())
        .ok_or_else(|| OnProbeError::other("AHCI MMIO register size is missing or too large"))?;
    let address = usize::try_from(register.address)
        .map_err(|_| OnProbeError::other("AHCI MMIO address does not fit usize"))?;
    let binding = binding_info_from_fdt(&info)?;
    if binding.irq_for_source(LOGICAL_IRQ_SOURCE).is_none() {
        return Err(OnProbeError::other(
            "AHCI controller has no interrupt source",
        ));
    }

    let activator = discover_activator(LS2K1000_DEVICE_NAME, address, size)?;
    let registered = platform
        .register_block_activator_with_info(activator, binding)
        .ok_or_else(|| OnProbeError::other("failed to register LS2K1000 AHCI activation owner"))?;
    log::info!("registered LS2K1000 AHCI activation owner: slot={registered}");
    Ok(())
}

fn discover_activator(
    name: &'static str,
    address: usize,
    size: usize,
) -> Result<AhciControllerActivator, OnProbeError> {
    let host = AhciHost::discover(
        name,
        address,
        size,
        u64::MAX,
        axklib::dma::op(),
        axklib::mmio::op(),
        AhciConfig::legacy_irq(LOGICAL_IRQ_SOURCE),
    )
    .map_err(|error| OnProbeError::other(format!("failed to discover {name}: {error}")))?;
    host.into_v13_activator()
        .map_err(|error| OnProbeError::other(format!("failed to prepare {name}: {error}")))
}
