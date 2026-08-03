//! PCI/FDT resource preparation for the portable `ahci-driver` crate.

use alloc::{format, string::String};

use ahci_driver::{AhciConfig, AhciHost};
use dma_api::DeviceDma;
use log::info;
use mmio_api::Mmio;
#[cfg(feature = "ahci")]
use pcie::CommandRegister;
use rdif_block::DriverGeneric;
use rdrive::probe::OnProbeError;
#[cfg(feature = "ahci")]
use rdrive::probe::pci::{FnOnProbe, ProbePci};
#[cfg(feature = "ahci-fdt")]
use rdrive::{
    probe::fdt::ResourcePrepareConfig,
    register::{FdtInfo, ProbeFdt},
};

#[cfg(feature = "ahci")]
use crate::{PciIrqRequirement, block::ProbePciBlockGroup};
#[cfg(feature = "ahci-fdt")]
use crate::{binding_info_from_fdt, block::PlatformDeviceBlockGroup};

#[cfg(feature = "ahci-fdt")]
const AHCI_FDT_COMPATIBLES: &[&str] = &[
    "generic-ahci",
    "loongson,ls-ahci",
    "loongson,ls2k1000-ahci",
    "loongson,2k1000-ahci",
];

#[cfg(feature = "ahci")]
crate::model_register!(
    name: "AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci as FnOnProbe,
    }],
);

#[cfg(feature = "ahci-fdt")]
crate::model_register!(
    name: "FDT AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: AHCI_FDT_COMPATIBLES,
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
        .ok_or_else(|| OnProbeError::other("AHCI MMIO BAR5/BAR0 is missing"))?;
    probe.endpoint_mut().update_command(|mut command| {
        command.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        command.remove(CommandRegister::INTERRUPT_DISABLE);
        command
    });
    let name = format!("ahci-pci-{:?}", probe.info().address);
    let mmio = map_mmio(bar.start, bar.count().max(1))?;
    let host = create_host(name, mmio, AhciConfig::generic())?;
    probe.register_block_group(host, PciIrqRequirement::Required)?;
    Ok(())
}

#[cfg(feature = "ahci-fdt")]
fn probe_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let binding = binding_info_from_fdt(info)?;
    if binding.irq().is_none() {
        return Err(OnProbeError::other(
            "AHCI requires an interrupt; polling fallback is unsupported",
        ));
    }
    let register = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    prepare_fdt_resources(info)?;
    let size = register.size.unwrap_or(0x1000) as usize;
    let address = register.address as usize;
    let config = fdt_profile(info);
    let name = format!("ahci-fdt-{}-{address:x}", info.node.name());
    let mmio = map_mmio(address, size)?;
    let host = create_host(name, mmio, config)?;
    let (_, platform) = probe.into_parts();
    platform.register_block_group_with_info(host, binding);
    Ok(())
}

#[cfg(feature = "ahci-fdt")]
fn prepare_fdt_resources(info: &FdtInfo<'_>) -> Result<(), OnProbeError> {
    info.prepare_resources(
        ResourcePrepareConfig::default()
            .with_assigned_clocks()
            .with_power_domains()
            .with_supply("target-supply"),
    )?;
    Ok(())
}

#[cfg(feature = "ahci-fdt")]
fn fdt_profile(info: &FdtInfo<'_>) -> AhciConfig {
    profile_for_compatibles(info.node.as_node().compatibles())
}

#[cfg(feature = "ahci-fdt")]
fn profile_for_compatibles<'a>(mut compatibles: impl Iterator<Item = &'a str>) -> AhciConfig {
    if compatibles.any(|compatible| {
        matches!(
            compatible,
            "loongson,ls-ahci" | "loongson,ls2k1000-ahci" | "loongson,2k1000-ahci"
        )
    }) {
        AhciConfig::ls2k()
    } else {
        AhciConfig::generic()
    }
}

fn map_mmio(address: usize, size: usize) -> Result<Mmio, OnProbeError> {
    axklib::mmio::ioremap(address.into(), size)
        .map_err(|error| OnProbeError::other(format!("AHCI MMIO mapping failed: {error:?}")))
}

fn create_host(name: String, mmio: Mmio, config: AhciConfig) -> Result<AhciHost, OnProbeError> {
    let dma: DeviceDma = axklib::dma::device_with_mask(u64::MAX);
    let host = AhciHost::new(name, mmio, dma, config)
        .map_err(|error| OnProbeError::other(format!("AHCI host creation failed: {error}")))?;
    info!("registered portable AHCI controller group {}", host.name());
    Ok(host)
}

#[cfg(all(test, feature = "ahci-fdt"))]
mod tests {
    use super::*;

    #[test]
    fn generic_and_ls2k_fdt_nodes_select_distinct_profiles() {
        assert_eq!(
            profile_for_compatibles(["generic-ahci"].into_iter()),
            AhciConfig::generic()
        );
        assert_eq!(
            profile_for_compatibles(["vendor,board", "loongson,ls2k1000-ahci"].into_iter()),
            AhciConfig::ls2k()
        );
    }

    #[test]
    fn dwc_ahci_is_not_claimed_by_the_generic_driver() {
        assert!(!AHCI_FDT_COMPATIBLES.contains(&"snps,dwc-ahci"));
    }
}
