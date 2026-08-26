#[cfg(not(test))]
use alloc::format;

#[cfg(not(test))]
use cv181x_sdhci::{Cv181xMmio, Cv181xSdhci};
#[cfg(not(test))]
use log::{info, warn};
#[cfg(not(test))]
use rdrive::{
    probe::{OnProbeError, fdt::ResourcePrepareConfig},
    register::{FdtInfo, ProbeFdt},
};
#[cfg(not(test))]
use sdhci_host::rdif as sdhci_rdif;
#[cfg(not(test))]
use sdmmc_protocol::{
    rdif::device::BlockDevice,
    sdio::{SdMmcIrqHost, init::CardInitPreference, native::SdMmcCard},
};

#[cfg(not(test))]
use crate::{
    block::ProbeFdtBlock,
    cv181x::{
        SDHCI_MIN_MMIO_SIZE, SYSCON_MIN_MMIO_SIZE, controller_region, has_property, host_config,
        required_region,
    },
};

#[cfg(not(test))]
pub const DEVICE_NAME: &str = "cvsd";

#[cfg(not(test))]
#[derive(Clone, Copy)]
struct CvsdFdtPolicy {
    no_sd: bool,
    no_mmc: bool,
    no_sdio: bool,
    non_removable: bool,
}

#[cfg(not(test))]
crate::model_register!(
    name: "FDT CVSD",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["cvitek,cv181x-sd"],
        on_probe: probe_fdt,
    }],
);

#[cfg(not(test))]
fn probe_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let resources = info.prepare_resources(
        ResourcePrepareConfig::default()
            .with_assigned_clocks()
            .with_power_domains()
            .with_named_clock_rate("sdio"),
    )?;
    let controller = controller_region(info, "sdio", SDHCI_MIN_MMIO_SIZE)?;
    let syscon = required_region(info, "syscon", "cvitek,syscon", SYSCON_MIN_MMIO_SIZE)?;
    let config = host_config(info, resources.clock_rate("sdio"));
    let policy = cvsd_fdt_policy(info);
    info!(
        "cvsd probe: node={}, src={}Hz min={}Hz max={}Hz bus_width={:?} no_1v8={} no_mmc={} \
         no_sdio={} cd_gpio={}",
        info.node.name(),
        config.src_frequency_hz,
        config.min_frequency_hz,
        config.max_frequency_hz,
        config.max_bus_width,
        config.no_1v8,
        policy.no_mmc,
        policy.no_sdio,
        config.has_card_detect_gpio,
    );

    let mut host =
        unsafe { Cv181xSdhci::new(Cv181xMmio::new(controller.map()?, syscon.map()?), config) };
    let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        crate::binding_resolver::dma_coherency_from_fdt(info),
        dma_api::DmaConstraints::new(u32::MAX as u64),
    ));
    let block_config = sdhci_rdif::dma_config(DEVICE_NAME, 0, &dma);
    host.configure_dma(dma)
        .map_err(|err| OnProbeError::other(format!("cvsd ADMA2 configuration failed: {err:?}")))?;

    let parts = host.into_parts();
    let mut card = SdMmcCard::new(parts.bus);
    card.set_sd_uhs_selection_enabled(false);
    let dev =
        BlockDevice::new_initializing(card, parts.irq, block_config, card_init_preference(policy));
    let irq = probe.register_block(dev)?;
    info!("cvsd block device registered irq={:?}", irq);
    Ok(())
}

#[cfg(not(test))]
fn cvsd_fdt_policy(info: &FdtInfo<'_>) -> CvsdFdtPolicy {
    CvsdFdtPolicy {
        no_sd: has_property(info, "no-sd"),
        no_mmc: has_property(info, "no-mmc"),
        no_sdio: has_property(info, "no-sdio"),
        non_removable: has_property(info, "non-removable"),
    }
}

#[cfg(not(test))]
fn card_init_preference(policy: CvsdFdtPolicy) -> CardInitPreference {
    if policy.no_sd || policy.non_removable {
        if policy.no_mmc {
            warn!("cvsd: FDT has both no-sd/non-removable and no-mmc; probing SD only");
            return CardInitPreference::SdOnly;
        }
        CardInitPreference::MmcFirst
    } else if policy.no_mmc {
        CardInitPreference::SdOnly
    } else {
        CardInitPreference::SdFirst
    }
}
